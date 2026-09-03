use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::server::execute_local_command_in_dir_with_timeout;
use homeboy_engine_primitives::template;

use homeboy_extension_contract::ExtensionManifest;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionReadinessState {
    Ready,
    NotReady,
    Unknown,
    TimedOut,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionReadyStatus {
    pub state: ExtensionReadinessState,
    pub ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_command: Option<String>,
}

/// Whether a caller wants readiness actually probed, or only the metadata that
/// costs nothing to read.
///
/// A `ready_check` is an arbitrary operator-authored shell command declared by
/// the extension; core neither knows nor bounds what it does. Inventory
/// commands (`extension list`, `extension show`) exist to answer "what is
/// installed, from where, at which revision", and that question must not be
/// gated behind an unrelated toolchain's health script (#10517).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionReadinessMode {
    /// Run the declared `ready_check`, bounded by [`ready_check_timeout`].
    Probe,
    /// Do not spawn anything; return matching cached evidence or `unknown`.
    Cached,
}

/// Wall-clock bound applied to a single `ready_check`.
///
/// Deliberately generous — a `ready_check` may legitimately compile or install
/// something — but never unbounded. #10517 reported `extension list` blowing
/// past a 120s operator bound and a readiness child eventually dying to
/// SIGTERM, which is the failure mode a bound exists to convert into an answer.
pub const DEFAULT_READY_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment override for [`ready_check_timeout`], in whole seconds.
pub const READY_CHECK_TIMEOUT_ENV: &str = "HOMEBOY_EXTENSION_READY_CHECK_TIMEOUT_SECONDS";

/// The bound applied to each `ready_check` invocation.
pub fn ready_check_timeout() -> Duration {
    ready_check_timeout_from(std::env::var(READY_CHECK_TIMEOUT_ENV).ok().as_deref())
}

/// Resolve the bound from a raw override. Split from the environment read so
/// the "never unbounded" contract is deterministically testable. A zero or
/// unparseable override is ignored rather than reinstating the hang.
fn ready_check_timeout_from(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_READY_CHECK_TIMEOUT)
}

/// Environment sentinel set while a `ready_check` runs. A `ready_check` command
/// can invoke `homeboy` (e.g. `homeboy component show <id>`), which would in turn
/// re-evaluate readiness and re-run the same `ready_check` — an unbounded
/// process-spawn recursion (issue #8115). The sentinel lets a nested invocation
/// detect that it is already inside a `ready_check` and skip re-running it.
const READY_CHECK_ACTIVE_ENV: &str = "HOMEBOY_EXTENSION_READY_CHECK_ACTIVE";

/// `ready_reason` reported when a caller asked for metadata only.
pub const READY_CHECK_SKIPPED_REASON: &str = "ready_check_skipped";

/// `ready_reason` reported when a `ready_check` hit its wall-clock bound.
pub const READY_CHECK_TIMEOUT_REASON: &str = "ready_check_timeout";

const READINESS_CACHE_SCHEMA: &str = "homeboy/extension-readiness-cache/v1";

#[derive(Deserialize, Serialize)]
struct ExtensionReadinessCache {
    schema: String,
    identity: String,
    checked_at: u64,
    status: ExtensionReadyStatus,
}

pub fn extension_ready_status(extension: &ExtensionManifest) -> ExtensionReadyStatus {
    extension_ready_status_with(extension, ExtensionReadinessMode::Probe)
}

pub fn extension_ready_status_with(
    extension: &ExtensionManifest,
    mode: ExtensionReadinessMode,
) -> ExtensionReadyStatus {
    let Some(runtime) = extension.runtime() else {
        return static_ready_status();
    };

    let Some(ready_check) = runtime.ready_check.as_ref() else {
        return static_ready_status();
    };

    let follow_up_command = format!("homeboy extension show {} --live-readiness", extension.id);
    let identity = readiness_identity(extension, ready_check, runtime.entrypoint.as_deref());
    if matches!(mode, ExtensionReadinessMode::Cached) {
        return read_cached_status(extension, &identity).unwrap_or_else(|| ExtensionReadyStatus {
            state: ExtensionReadinessState::Unknown,
            ready: None,
            reason: Some(READY_CHECK_SKIPPED_REASON.to_string()),
            detail: Some(format!(
                "Metadata-only inspection did not run ready_check and no matching live readiness probe is cached. Run `{follow_up_command}`."
            )),
            cache_age_seconds: None,
            probe_duration_ms: None,
            timeout_ms: Some(duration_ms(ready_check_timeout())),
            follow_up_command: Some(follow_up_command),
        });
    }

    // Re-entry guard: if we are already inside a `ready_check`, do not run it
    // again. A `ready_check` that shells back into `homeboy` (component/extension
    // inspection) would otherwise recurse without bound. Report ready so the
    // nested inspection completes instead of spawning another check. (#8115)
    if std::env::var_os(READY_CHECK_ACTIVE_ENV).is_some() {
        return ExtensionReadyStatus {
            state: ExtensionReadinessState::Unknown,
            ready: None,
            reason: Some("ready_check_reentrant_skipped".to_string()),
            detail: Some(
                "ready_check skipped: already evaluating readiness for this extension (re-entrant invocation)".to_string(),
            ),
            cache_age_seconds: None,
            probe_duration_ms: None,
            timeout_ms: Some(duration_ms(ready_check_timeout())),
            follow_up_command: Some(follow_up_command),
        };
    }

    let Some(extension_path) = extension.extension_path.as_ref() else {
        return ExtensionReadyStatus {
            state: ExtensionReadinessState::NotReady,
            ready: Some(false),
            reason: Some("missing_extension_path".to_string()),
            detail: Some("ready_check configured but extension_path is missing".to_string()),
            cache_age_seconds: None,
            probe_duration_ms: None,
            timeout_ms: Some(duration_ms(ready_check_timeout())),
            follow_up_command: Some(follow_up_command),
        };
    };

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("extension_path", extension_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];
    let command = template::render(ready_check, &vars);
    let timeout = ready_check_timeout();
    let started = Instant::now();
    // Mark the child (and anything it spawns) as running inside a ready_check so
    // a re-entrant `homeboy` invocation trips the guard above.
    let output = execute_local_command_in_dir_with_timeout(
        &command,
        Some(extension_path),
        Some(&[(READY_CHECK_ACTIVE_ENV, "1")]),
        timeout,
    );
    let probe_duration_ms = duration_ms(started.elapsed());

    let status = if output.success {
        ExtensionReadyStatus {
            state: ExtensionReadinessState::Ready,
            ready: Some(true),
            reason: None,
            detail: None,
            cache_age_seconds: Some(0),
            probe_duration_ms: Some(probe_duration_ms),
            timeout_ms: Some(duration_ms(timeout)),
            follow_up_command: Some(follow_up_command.clone()),
        }
    } else if output.timed_out {
        // The process did not answer. Keep that distinct from a negative answer.
        ExtensionReadyStatus {
            state: ExtensionReadinessState::TimedOut,
            ready: None,
            reason: Some(READY_CHECK_TIMEOUT_REASON.to_string()),
            detail: Some(format!(
                "ready_check '{}' exceeded its {}s bound and its process group was terminated; \
                 extension metadata is unaffected. Set {} to change the bound, or retry with `{}`.",
                command,
                timeout.as_secs(),
                READY_CHECK_TIMEOUT_ENV,
                follow_up_command
            )),
            cache_age_seconds: Some(0),
            probe_duration_ms: Some(probe_duration_ms),
            timeout_ms: Some(duration_ms(timeout)),
            follow_up_command: Some(follow_up_command.clone()),
        }
    } else {
        let detail_output = if output.stderr.trim().is_empty() {
            output.stdout
        } else {
            output.stderr
        };
        let detail = detail_output.trim();
        let detail = if detail.is_empty() {
            format!(
                "ready_check '{}' failed with exit code {}",
                command, output.exit_code
            )
        } else {
            format!(
                "ready_check '{}' failed with exit code {}: {}",
                command, output.exit_code, detail
            )
        };

        ExtensionReadyStatus {
            state: ExtensionReadinessState::NotReady,
            ready: Some(false),
            reason: Some("ready_check_failed".to_string()),
            detail: Some(detail),
            cache_age_seconds: Some(0),
            probe_duration_ms: Some(probe_duration_ms),
            timeout_ms: Some(duration_ms(timeout)),
            follow_up_command: Some(follow_up_command.clone()),
        }
    };

    write_cached_status(extension, identity, &status);
    status
}

fn static_ready_status() -> ExtensionReadyStatus {
    ExtensionReadyStatus {
        state: ExtensionReadinessState::Ready,
        ready: Some(true),
        reason: None,
        detail: None,
        cache_age_seconds: None,
        probe_duration_ms: None,
        timeout_ms: None,
        follow_up_command: None,
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn readiness_identity(
    extension: &ExtensionManifest,
    ready_check: &str,
    entrypoint: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(extension.id.as_bytes());
    hasher.update([0]);
    hasher.update(extension.version.as_bytes());
    hasher.update([0]);
    hasher.update(
        extension
            .extension_path
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update([0]);
    if let Some(revision) = extension
        .extension_path
        .as_deref()
        .and_then(|path| crate::extension::lifecycle::read_source_revision_at(path.as_ref()))
    {
        hasher.update(revision.as_bytes());
    }
    hasher.update([0]);
    hasher.update(ready_check.as_bytes());
    hasher.update([0]);
    hasher.update(entrypoint.unwrap_or_default().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn readiness_cache_filename(extension_id: &str) -> String {
    let digest = Sha256::digest(extension_id.as_bytes());
    format!("extension-readiness.{digest:x}.json")
}

fn read_cached_status(
    extension: &ExtensionManifest,
    identity: &str,
) -> Option<ExtensionReadyStatus> {
    let cache: ExtensionReadinessCache =
        crate::update_check_cache::read_cache(&readiness_cache_filename(&extension.id))?;
    if cache.schema != READINESS_CACHE_SCHEMA || cache.identity != identity {
        return None;
    }
    let mut status = cache.status;
    status.cache_age_seconds =
        Some(crate::update_check_cache::now_unix().saturating_sub(cache.checked_at));
    Some(status)
}

fn write_cached_status(
    extension: &ExtensionManifest,
    identity: String,
    status: &ExtensionReadyStatus,
) {
    crate::update_check_cache::write_cache(
        &readiness_cache_filename(&extension.id),
        &ExtensionReadinessCache {
            schema: READINESS_CACHE_SCHEMA.to_string(),
            identity,
            checked_at: crate::update_check_cache::now_unix(),
            status: status.clone(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReadinessEnvironment {
        _home: crate::test_support::HomeGuard,
        active: Option<std::ffi::OsString>,
        timeout: Option<std::ffi::OsString>,
    }

    impl ReadinessEnvironment {
        fn new(active: Option<&str>, timeout: Option<&str>) -> Self {
            let home = crate::test_support::HomeGuard::new();
            let prior_active = std::env::var_os(READY_CHECK_ACTIVE_ENV);
            let prior_timeout = std::env::var_os(READY_CHECK_TIMEOUT_ENV);
            match active {
                Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
                None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
            }
            match timeout {
                Some(value) => std::env::set_var(READY_CHECK_TIMEOUT_ENV, value),
                None => std::env::remove_var(READY_CHECK_TIMEOUT_ENV),
            }
            Self {
                _home: home,
                active: prior_active,
                timeout: prior_timeout,
            }
        }
    }

    impl Drop for ReadinessEnvironment {
        fn drop(&mut self) {
            match &self.active {
                Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
                None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
            }
            match &self.timeout {
                Some(value) => std::env::set_var(READY_CHECK_TIMEOUT_ENV, value),
                None => std::env::remove_var(READY_CHECK_TIMEOUT_ENV),
            }
        }
    }

    fn manifest_with_ready_check(extension_path: &str, ready_check: &str) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "fixture",
            "version": "1.0.0",
            "executable": {
                "runtime": {
                    "ready_check": ready_check
                }
            }
        }))
        .expect("manifest parses");
        manifest.id = "fixture".to_string();
        manifest.extension_path = Some(extension_path.to_string());
        manifest
    }

    #[test]
    fn reentrant_ready_check_is_skipped_without_running_the_command() {
        // A ready_check that would fail if executed proves the command is never
        // run when the re-entry sentinel is already set. (#8115)
        let manifest = manifest_with_ready_check("/tmp", "exit 1");
        let _environment = ReadinessEnvironment::new(Some("1"), None);

        let status = extension_ready_status(&manifest);

        assert_eq!(status.ready, None);
        assert_eq!(status.state, ExtensionReadinessState::Unknown);
        assert_eq!(
            status.reason.as_deref(),
            Some("ready_check_reentrant_skipped")
        );
    }

    #[test]
    fn ready_check_runs_when_not_reentrant() {
        let manifest = manifest_with_ready_check("/tmp", "true");
        let _environment = ReadinessEnvironment::new(None, None);

        let status = extension_ready_status(&manifest);

        assert_eq!(status.ready, Some(true));
        assert_eq!(status.state, ExtensionReadinessState::Ready);
        assert_eq!(status.reason, None);
    }

    /// Metadata inspection must not spawn an operator-authored shell command.
    /// The `ready_check` here fails instantly if it ever runs, and the sentinel
    /// is explicitly absent so the re-entry guard cannot mask the result.
    #[test]
    fn a_skipped_ready_check_never_runs_the_command() {
        let manifest = manifest_with_ready_check("/tmp", "exit 1");
        let _environment = ReadinessEnvironment::new(None, None);

        let status = extension_ready_status_with(&manifest, ExtensionReadinessMode::Cached);

        assert_eq!(status.reason.as_deref(), Some(READY_CHECK_SKIPPED_REASON));
        assert_eq!(status.ready, None);
        assert_eq!(status.state, ExtensionReadinessState::Unknown);
        assert!(status.detail.unwrap_or_default().contains("Metadata-only"));
    }

    /// A `ready_check` that outlives its budget must produce an answer rather
    /// than hanging the inventory command that asked for it (#10517). Uses a
    /// one-second override so the test costs a second, not the 30s default.
    #[test]
    fn a_ready_check_that_outlives_its_bound_reports_a_timeout_instead_of_hanging() {
        let manifest = manifest_with_ready_check("/tmp", "sleep 30");
        let _environment = ReadinessEnvironment::new(None, Some("1"));

        let status = extension_ready_status(&manifest);

        assert_eq!(status.ready, None);
        assert_eq!(status.state, ExtensionReadinessState::TimedOut);
        assert_eq!(status.reason.as_deref(), Some(READY_CHECK_TIMEOUT_REASON));
        let detail = status.detail.unwrap_or_default();
        assert!(detail.contains("exceeded its 1s bound"), "{detail}");
        assert!(detail.contains("metadata is unaffected"), "{detail}");
    }

    #[test]
    fn a_failed_ready_check_is_typed_not_ready() {
        let manifest = manifest_with_ready_check("/tmp", "exit 7");
        let _environment = ReadinessEnvironment::new(None, None);

        let status = extension_ready_status(&manifest);

        assert_eq!(status.state, ExtensionReadinessState::NotReady);
        assert_eq!(status.ready, Some(false));
        assert_eq!(status.reason.as_deref(), Some("ready_check_failed"));
        assert!(status.probe_duration_ms.is_some());
        assert_eq!(status.timeout_ms, Some(30_000));
    }

    #[test]
    fn a_live_probe_refreshes_the_static_readiness_cache() {
        crate::test_support::with_isolated_home(|home| {
            let sentinel = home.path().join("ready");
            let command = format!("test -f {}", sentinel.display());
            let manifest =
                manifest_with_ready_check(home.path().to_string_lossy().as_ref(), &command);

            let missing = extension_ready_status(&manifest);
            assert_eq!(missing.ready, Some(false));
            assert_eq!(
                extension_ready_status_with(&manifest, ExtensionReadinessMode::Cached).ready,
                Some(false)
            );

            std::fs::write(&sentinel, "ready").expect("sentinel");
            let refreshed = extension_ready_status(&manifest);
            assert_eq!(refreshed.ready, Some(true));
            let cached = extension_ready_status_with(&manifest, ExtensionReadinessMode::Cached);
            assert_eq!(cached.ready, Some(true));
            assert_eq!(cached.state, ExtensionReadinessState::Ready);
            assert_eq!(cached.cache_age_seconds, Some(0));
            assert_eq!(
                cached.follow_up_command.as_deref(),
                Some("homeboy extension show fixture --live-readiness")
            );
        });
    }

    #[test]
    fn the_ready_check_bound_is_never_unbounded_and_honors_a_positive_override() {
        assert_eq!(ready_check_timeout_from(None), DEFAULT_READY_CHECK_TIMEOUT);
        // "0" would reintroduce the unbounded probe this bound exists to prevent.
        assert_eq!(
            ready_check_timeout_from(Some("0")),
            DEFAULT_READY_CHECK_TIMEOUT
        );
        assert_eq!(
            ready_check_timeout_from(Some("not-a-number")),
            DEFAULT_READY_CHECK_TIMEOUT
        );
        assert_eq!(
            ready_check_timeout_from(Some(" 3 ")),
            Duration::from_secs(3)
        );
    }
}
