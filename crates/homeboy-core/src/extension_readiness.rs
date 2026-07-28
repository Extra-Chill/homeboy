use std::time::Duration;

use serde::Serialize;

use crate::project::Project;
use crate::server::execute_local_command_in_dir_with_timeout;
use homeboy_engine_primitives::template;

use crate::extension_store::load_extension;
use homeboy_extension_contract::ExtensionManifest;

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionReadyStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Whether a caller wants readiness actually probed, or only the metadata that
/// costs nothing to read.
///
/// A `ready_check` is an arbitrary operator-authored shell command — a WordPress
/// doctor, an npm install probe, a Codebox health script. Inventory commands
/// (`extension list`, `extension show`) exist to answer "what is installed,
/// from where, at which revision", and that question must not be gated behind
/// someone else's PHP (#10517).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionReadinessMode {
    /// Run the declared `ready_check`, bounded by [`ready_check_timeout`].
    Probe,
    /// Do not spawn anything; report the probe as deliberately not run.
    Skip,
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

pub fn extension_ready_status(extension: &ExtensionManifest) -> ExtensionReadyStatus {
    extension_ready_status_with(extension, ExtensionReadinessMode::Probe)
}

pub fn extension_ready_status_with(
    extension: &ExtensionManifest,
    mode: ExtensionReadinessMode,
) -> ExtensionReadyStatus {
    let Some(runtime) = extension.runtime() else {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(ready_check) = runtime.ready_check.as_ref() else {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    // The caller asked for metadata only. Mirror the re-entrant case below: the
    // check did not run, the reason says so, and the answer does not pretend to
    // be a failed probe. (#10517)
    if matches!(mode, ExtensionReadinessMode::Skip) {
        return ExtensionReadyStatus {
            ready: true,
            reason: Some(READY_CHECK_SKIPPED_REASON.to_string()),
            detail: Some(
                "ready_check not run: this command was asked for metadata only. Run `homeboy extension show <id>` without --skip-ready-check for live readiness.".to_string(),
            ),
        };
    }

    // Re-entry guard: if we are already inside a `ready_check`, do not run it
    // again. A `ready_check` that shells back into `homeboy` (component/extension
    // inspection) would otherwise recurse without bound. Report ready so the
    // nested inspection completes instead of spawning another check. (#8115)
    if std::env::var_os(READY_CHECK_ACTIVE_ENV).is_some() {
        return ExtensionReadyStatus {
            ready: true,
            reason: Some("ready_check_reentrant_skipped".to_string()),
            detail: Some(
                "ready_check skipped: already evaluating readiness for this extension (re-entrant invocation)".to_string(),
            ),
        };
    }

    let Some(extension_path) = extension.extension_path.as_ref() else {
        return ExtensionReadyStatus {
            ready: false,
            reason: Some("missing_extension_path".to_string()),
            detail: Some("ready_check configured but extension_path is missing".to_string()),
        };
    };

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("extension_path", extension_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];
    let command = template::render(ready_check, &vars);
    let timeout = ready_check_timeout();
    // Mark the child (and anything it spawns) as running inside a ready_check so
    // a re-entrant `homeboy` invocation trips the guard above.
    let output = execute_local_command_in_dir_with_timeout(
        &command,
        Some(extension_path),
        Some(&[(READY_CHECK_ACTIVE_ENV, "1")]),
        timeout,
    );

    if output.success {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    }

    // A probe that ran out of wall clock is not the same answer as a probe that
    // ran and said no. Naming it keeps "the doctor is slow" distinguishable from
    // "the extension is broken", and keeps the surrounding metadata usable.
    if output.timed_out {
        return ExtensionReadyStatus {
            ready: false,
            reason: Some(READY_CHECK_TIMEOUT_REASON.to_string()),
            detail: Some(format!(
                "ready_check '{}' exceeded its {}s bound and its process group was terminated; \
                 extension metadata is unaffected. Set {} to change the bound, or pass \
                 --skip-ready-check to inventory commands.",
                command,
                timeout.as_secs(),
                READY_CHECK_TIMEOUT_ENV
            )),
        };
    }

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
        ready: false,
        reason: Some("ready_check_failed".to_string()),
        detail: Some(detail),
    }
}

/// Check if a extension is compatible with a project.
pub fn is_extension_compatible(extension: &ExtensionManifest, project: Option<&Project>) -> bool {
    let Some(ref requires) = extension.requires else {
        return true;
    };

    // Required extensions must be installed globally
    for required_extension in &requires.extensions {
        if load_extension(required_extension).is_err() {
            return false;
        }
    }

    // Required components must be linked to the project (if project context exists)
    if let Some(project) = project {
        for component in &requires.components {
            if !crate::project::has_component(project, component) {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let prior = std::env::var_os(READY_CHECK_ACTIVE_ENV);
        std::env::set_var(READY_CHECK_ACTIVE_ENV, "1");

        let status = extension_ready_status(&manifest);

        match prior {
            Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
            None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
        }

        assert!(
            status.ready,
            "a re-entrant ready_check must report ready instead of recursing"
        );
        assert_eq!(
            status.reason.as_deref(),
            Some("ready_check_reentrant_skipped")
        );
    }

    #[test]
    fn ready_check_runs_when_not_reentrant() {
        let manifest = manifest_with_ready_check("/tmp", "true");
        let prior = std::env::var_os(READY_CHECK_ACTIVE_ENV);
        std::env::remove_var(READY_CHECK_ACTIVE_ENV);

        let status = extension_ready_status(&manifest);

        match prior {
            Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
            None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
        }

        assert!(status.ready, "a passing ready_check reports ready");
        assert_eq!(status.reason, None);
    }

    /// Metadata inspection must not spawn an operator-authored shell command.
    /// The `ready_check` here fails instantly if it ever runs, and the sentinel
    /// is explicitly absent so the re-entry guard cannot mask the result.
    #[test]
    fn a_skipped_ready_check_never_runs_the_command() {
        let manifest = manifest_with_ready_check("/tmp", "exit 1");
        let prior = std::env::var_os(READY_CHECK_ACTIVE_ENV);
        std::env::remove_var(READY_CHECK_ACTIVE_ENV);

        let status = extension_ready_status_with(&manifest, ExtensionReadinessMode::Skip);

        if let Some(value) = prior {
            std::env::set_var(READY_CHECK_ACTIVE_ENV, value);
        }

        assert_eq!(status.reason.as_deref(), Some(READY_CHECK_SKIPPED_REASON));
        assert!(
            status.ready,
            "a skipped probe reports 'not evaluated', not 'not ready'"
        );
        assert!(status.detail.unwrap_or_default().contains("metadata only"));
    }

    /// A `ready_check` that outlives its budget must produce an answer rather
    /// than hanging the inventory command that asked for it (#10517). Uses a
    /// one-second override so the test costs a second, not the 30s default.
    #[test]
    fn a_ready_check_that_outlives_its_bound_reports_a_timeout_instead_of_hanging() {
        let manifest = manifest_with_ready_check("/tmp", "sleep 30");
        let prior_active = std::env::var_os(READY_CHECK_ACTIVE_ENV);
        let prior_timeout = std::env::var_os(READY_CHECK_TIMEOUT_ENV);
        std::env::remove_var(READY_CHECK_ACTIVE_ENV);
        std::env::set_var(READY_CHECK_TIMEOUT_ENV, "1");

        let status = extension_ready_status(&manifest);

        match prior_active {
            Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
            None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
        }
        match prior_timeout {
            Some(value) => std::env::set_var(READY_CHECK_TIMEOUT_ENV, value),
            None => std::env::remove_var(READY_CHECK_TIMEOUT_ENV),
        }

        assert!(!status.ready);
        assert_eq!(status.reason.as_deref(), Some(READY_CHECK_TIMEOUT_REASON));
        let detail = status.detail.unwrap_or_default();
        assert!(detail.contains("exceeded its 1s bound"), "{detail}");
        assert!(detail.contains("metadata is unaffected"), "{detail}");
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
