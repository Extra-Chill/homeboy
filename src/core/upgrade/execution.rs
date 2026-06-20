use crate::core::defaults;
use crate::core::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::constants::{VERIFY_READBACK_ATTEMPTS, VERIFY_READBACK_DELAY};
use super::helpers::{current_version, version_is_newer};
use super::planning::resolve_binary_on_path;
use super::types::InstallMethod;

/// Maximum number of bytes retained per captured stream when surfacing an
/// upgrade-command failure. The upgrade command's stdout/stderr are
/// attacker-influenced and otherwise unbounded, so the retained evidence is
/// capped with truncation metadata. Mirrors the bounded-capture pattern used by
/// `agent_task_promotion` / runner exec captures (#5297).
const UPGRADE_CAPTURE_LIMIT_BYTES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveBinaryInfo {
    pub version: Option<String>,
    pub build_identity: Option<String>,
}

/// Truncation metadata describing how much of a captured stream was retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamCaptureMetadata {
    pub limit_bytes: usize,
    pub seen_bytes: usize,
    pub retained_bytes: usize,
    pub truncated: bool,
}

/// Bound a captured stream to a retained-byte cap, keeping the trailing bytes
/// (the most relevant tail for a failure message) and returning the retained
/// text plus truncation metadata. Mirrors the `bound_captured_stream` pattern
/// in `agent_task_promotion`.
pub(crate) fn bound_captured_stream(bytes: &[u8], limit: usize) -> (String, StreamCaptureMetadata) {
    let seen = bytes.len();
    let retained: &[u8] = if seen > limit {
        &bytes[seen - limit..]
    } else {
        bytes
    };
    let metadata = StreamCaptureMetadata {
        limit_bytes: limit,
        seen_bytes: seen,
        retained_bytes: retained.len(),
        truncated: seen > retained.len(),
    };
    (String::from_utf8_lossy(retained).to_string(), metadata)
}

/// Append a human-readable truncation note when the retained capture dropped
/// bytes, so a surfaced failure detail makes the truncation observable rather
/// than silently hiding the dropped output.
pub(crate) fn annotate_truncation(detail: &str, capture: &StreamCaptureMetadata) -> String {
    if capture.truncated {
        format!(
            "{detail} [output truncated: retained {} of {} bytes]",
            capture.retained_bytes, capture.seen_bytes
        )
    } else {
        detail.to_string()
    }
}

pub(crate) fn execute_upgrade(
    method: InstallMethod,
    source_path: Option<&Path>,
    force: bool,
    previous_build_identity: Option<&str>,
) -> Result<(bool, Option<String>, Option<String>)> {
    let defaults = defaults::load_defaults();

    let output = match method {
        InstallMethod::Homebrew => {
            let cmd = &defaults.install_methods.homebrew.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run homebrew upgrade".to_string()))
            })?
        }
        InstallMethod::Secondary => {
            let cmd = &defaults.install_methods.secondary.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run secondary upgrade".to_string()))
            })?
        }
        InstallMethod::Source => {
            let workspace_root = resolve_source_workspace(source_path)?;

            // Execute the upgrade command from defaults
            let cmd = &defaults.install_methods.source.upgrade_command;
            Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&workspace_root)
                .output()
                .map_err(|e| {
                    Error::internal_io(e.to_string(), Some("run source upgrade".to_string()))
                })?
        }
        InstallMethod::Binary => {
            let cmd = &defaults.install_methods.binary.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run binary upgrade".to_string()))
            })?
        }
        InstallMethod::Unknown => {
            return Err(Error::validation_invalid_argument(
                "install_method",
                "Cannot upgrade: unknown installation method",
                None,
                None,
            ));
        }
    };

    if !output.status.success() {
        // The upgrade command's stdout/stderr are unbounded; bound the retained
        // bytes (keeping the trailing tail) with truncation metadata so a
        // pathological command cannot force an arbitrarily large failure string
        // into memory or logs (#5297).
        let (stderr, stderr_capture) =
            bound_captured_stream(&output.stderr, UPGRADE_CAPTURE_LIMIT_BYTES);
        let (stdout, stdout_capture) =
            bound_captured_stream(&output.stdout, UPGRADE_CAPTURE_LIMIT_BYTES);
        let error_detail = if !stderr.trim().is_empty() {
            annotate_truncation(stderr.trim(), &stderr_capture)
        } else if !stdout.trim().is_empty() {
            annotate_truncation(stdout.trim(), &stdout_capture)
        } else {
            format!("exit code {}", output.status.code().unwrap_or(1))
        };
        return Err(upgrade_failure_error(method, &error_detail));
    }

    // The upgrade command above succeeded, so the new binary is already on
    // disk. Reading the version back can race the just-replaced binary (atomic
    // rename not yet observable on PATH, stale resolution, the process failing
    // for a moment right after the swap), which previously produced a
    // false-negative `upgraded: false` / `new_version: null` on a successful
    // upgrade. Retry the read-back until it reports a verifiable version before
    // giving up. See issue #3463.
    let (success, active_binary) = verify_upgrade_with_retry(
        method,
        force,
        current_version(),
        previous_build_identity,
        VERIFY_READBACK_ATTEMPTS,
        VERIFY_READBACK_DELAY,
        || active_binary_info().ok().flatten(),
        std::thread::sleep,
    );

    let new_version = active_binary.as_ref().and_then(|info| info.version.clone());
    let new_build_identity = active_binary.and_then(|info| info.build_identity);

    Ok((success, new_version, new_build_identity))
}

/// Read back the active binary version after a successful swap, retrying while
/// the read-back races the just-replaced binary. Returns the verification
/// result alongside the last observed binary info.
///
/// `read_active` reads the current active binary info (e.g. by exec'ing
/// `homeboy --version`); `sleep` waits between attempts. Both are injected so
/// this can be exercised without spawning processes or real delays in tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_upgrade_with_retry<R, S>(
    method: InstallMethod,
    force: bool,
    previous_version: &str,
    previous_build_identity: Option<&str>,
    attempts: u32,
    delay: std::time::Duration,
    mut read_active: R,
    mut sleep: S,
) -> (bool, Option<ActiveBinaryInfo>)
where
    R: FnMut() -> Option<ActiveBinaryInfo>,
    S: FnMut(std::time::Duration),
{
    let attempts = attempts.max(1);
    let mut last_seen: Option<ActiveBinaryInfo> = None;

    for attempt in 0..attempts {
        let active_binary = read_active();
        let new_version = active_binary.as_ref().and_then(|info| info.version.clone());
        let new_build_identity = active_binary
            .as_ref()
            .and_then(|info| info.build_identity.clone());

        let success = upgrade_verification_result(
            method,
            force,
            previous_version,
            new_version.as_deref(),
            previous_build_identity,
            new_build_identity.as_deref(),
        );

        if success {
            return (true, active_binary);
        }

        // Hold onto the most informative read so the caller can still report a
        // version even if verification never flips to success.
        if active_binary
            .as_ref()
            .and_then(|info| info.version.as_deref())
            .is_some()
            || last_seen.is_none()
        {
            last_seen = active_binary;
        }

        if attempt + 1 < attempts {
            sleep(delay);
        }
    }

    (false, last_seen)
}

fn upgrade_failure_error(method: InstallMethod, error_detail: &str) -> Error {
    let mut error = Error::internal_io(
        format!("{} upgrade failed: {}", method.as_str(), error_detail),
        Some("execute upgrade".to_string()),
    );

    if method == InstallMethod::Binary && error_detail.contains("404") {
        error = error
            .with_hint("No release asset was found for this Homeboy version.")
            .with_hint("Try: homeboy upgrade --method source --source-path <PATH>");
    } else if method == InstallMethod::Secondary && error_detail.contains("not found") {
        error = error
            .with_hint("Required executable is not installed or is not on PATH.")
            .with_hint(
                "Install the required toolchain, or use: homeboy upgrade --method source --source-path <PATH>",
            );
    }

    error
}

pub(crate) fn resolve_source_workspace(source_path: Option<&Path>) -> Result<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = source_path {
        candidates.push(path.to_path_buf());
    } else {
        if let Ok(current_dir) = std::env::current_dir() {
            candidates.push(current_dir);
        }

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(workspace_root) = workspace_from_exe_path(&exe_path) {
                candidates.push(workspace_root);
            }
        }
    }

    for candidate in candidates {
        if let Some(checkout) = find_homeboy_source_checkout(&candidate) {
            return Ok(checkout);
        }
    }

    let id = source_path
        .map(|path| path.to_string_lossy().to_string())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        });

    Err(Error::validation_invalid_argument(
        "source_path",
        "Could not find a Homeboy source workspace for source build",
        id,
        None,
    )
    .with_hint("Run from the Homeboy source workspace, or pass: homeboy upgrade --method source --source-path <PATH>"))
}

fn workspace_from_exe_path(exe_path: &Path) -> Option<PathBuf> {
    let parent = exe_path.parent()?;
    let build_dir = parent.file_name()?.to_string_lossy();
    if build_dir != "release" && build_dir != "debug" {
        return None;
    }

    let target_dir = parent.parent()?;
    if target_dir.file_name()?.to_string_lossy() != "target" {
        return None;
    }

    target_dir.parent().map(Path::to_path_buf)
}

fn is_homeboy_source_checkout(path: &Path) -> bool {
    let manifest = path.join("homeboy.json");
    let Ok(contents) = std::fs::read_to_string(manifest) else {
        return false;
    };

    let is_homeboy_manifest = serde_json::from_str::<serde_json::Value>(&contents)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .as_deref()
        == Some("homeboy");

    is_homeboy_manifest && is_homeboy_build_package(path)
}

fn is_homeboy_build_package(path: &Path) -> bool {
    let defaults = defaults::load_defaults();
    let Some(package_manifest) = defaults
        .version_candidates
        .iter()
        .map(|candidate| candidate.file.as_str())
        .find(|file| file.ends_with(".toml"))
    else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(path.join(package_manifest)) else {
        return false;
    };

    toml::from_str::<toml::Value>(&contents)
        .ok()
        .and_then(|value| {
            value
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .as_deref()
        == Some("homeboy")
}

fn find_homeboy_source_checkout(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| is_homeboy_source_checkout(candidate))
        .map(Path::to_path_buf)
}

fn active_binary_info() -> Result<Option<ActiveBinaryInfo>> {
    let exe_path = active_binary_path()?;

    let mut command = Command::new(exe_path);
    command.arg("--version");
    let output = command_output_with_timeout(&mut command, Duration::from_secs(5))?;

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(parse_cli_version_info(&String::from_utf8_lossy(
        &output.stdout,
    ))))
}

fn command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut child = command.spawn().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("verify active binary version".to_string()),
        )
    })?;
    let start = Instant::now();

    loop {
        if child
            .try_wait()
            .map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some("verify active binary version".to_string()),
                )
            })?
            .is_some()
        {
            return child.wait_with_output().map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some("verify active binary version".to_string()),
                )
            });
        }

        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::internal_io(
                format!(
                    "active binary did not answer --version within {}s",
                    timeout.as_secs()
                ),
                Some("verify active binary version".to_string()),
            ));
        }

        std::thread::sleep(Duration::from_millis(25));
    }
}

fn active_binary_path() -> Result<PathBuf> {
    if let Some(path) = resolve_binary_on_path() {
        return Ok(path);
    }

    std::env::current_exe().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("get current executable path".to_string()),
        )
    })
}

pub(crate) fn upgrade_verification_result(
    method: InstallMethod,
    force: bool,
    previous_version: &str,
    active_version: Option<&str>,
    previous_build_identity: Option<&str>,
    active_build_identity: Option<&str>,
) -> bool {
    let Some(active_version) = active_version else {
        return false;
    };

    if version_is_newer(active_version, previous_version) {
        return true;
    }

    method == InstallMethod::Source
        && force
        && active_version == previous_version
        && previous_build_identity.is_some()
        && active_build_identity.is_some()
        && previous_build_identity != active_build_identity
}

fn parse_cli_version_info(output: &str) -> ActiveBinaryInfo {
    ActiveBinaryInfo {
        version: parse_cli_version_output(output),
        build_identity: parse_cli_build_identity_output(output),
    }
}

fn parse_cli_version_output(output: &str) -> Option<String> {
    let re = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}

fn parse_cli_build_identity_output(output: &str) -> Option<String> {
    let identity = output.trim();
    if identity.is_empty() || !identity.contains('+') {
        None
    } else {
        Some(identity.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_captured_stream_retains_full_source_within_limit() {
        let (text, capture) = bound_captured_stream(b"boom", 1024);

        assert_eq!(text, "boom");
        assert_eq!(capture.limit_bytes, 1024);
        assert_eq!(capture.seen_bytes, 4);
        assert_eq!(capture.retained_bytes, 4);
        assert!(!capture.truncated);
    }

    #[test]
    fn bound_captured_stream_keeps_trailing_tail_when_truncated() {
        let source = vec![b'x'; 16];
        let (text, capture) = bound_captured_stream(&source, 4);

        assert_eq!(text, "xxxx");
        assert_eq!(capture.limit_bytes, 4);
        assert_eq!(capture.retained_bytes, 4);
        assert_eq!(capture.seen_bytes, 16);
        assert!(capture.truncated);
    }

    #[test]
    fn bound_captured_stream_retains_most_relevant_tail() {
        let source = b"head-noise-TAIL".to_vec();
        let (text, capture) = bound_captured_stream(&source, 4);

        assert_eq!(text, "TAIL");
        assert!(capture.truncated);
    }

    #[test]
    fn annotate_truncation_notes_dropped_bytes() {
        let capture = StreamCaptureMetadata {
            limit_bytes: 4,
            seen_bytes: 16,
            retained_bytes: 4,
            truncated: true,
        };

        let annotated = annotate_truncation("TAIL", &capture);

        assert!(annotated.starts_with("TAIL"));
        assert!(annotated.contains("output truncated"));
        assert!(annotated.contains("retained 4 of 16 bytes"));
    }

    #[test]
    fn annotate_truncation_leaves_untruncated_detail_unchanged() {
        let capture = StreamCaptureMetadata {
            limit_bytes: 1024,
            seen_bytes: 4,
            retained_bytes: 4,
            truncated: false,
        };

        assert_eq!(annotate_truncation("boom", &capture), "boom");
    }

    #[test]
    fn parses_homeboy_version_output() {
        assert_eq!(
            parse_cli_version_output("homeboy 0.158.0").as_deref(),
            Some("0.158.0")
        );
    }

    #[test]
    fn test_execute_upgrade() {
        assert_eq!(
            parse_cli_version_output("homeboy 0.158.0").as_deref(),
            Some("0.158.0")
        );
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            false,
            "0.157.1",
            Some("0.157.1"),
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
    }

    #[test]
    fn test_upgrade_verification_result() {
        assert!(upgrade_verification_result(
            InstallMethod::Secondary,
            false,
            "0.157.1",
            Some("0.158.0"),
            None,
            None,
        ));
        assert!(!upgrade_verification_result(
            InstallMethod::Secondary,
            true,
            "0.157.1",
            Some("0.157.1"),
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            None,
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
    }

    #[test]
    fn verification_rejects_unchanged_active_binary() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            Some("commit same, dirty=false"),
            Some("commit same, dirty=false"),
        ));
    }

    #[test]
    fn verification_accepts_newer_active_binary() {
        assert!(upgrade_verification_result(
            InstallMethod::Secondary,
            false,
            "0.157.1",
            Some("0.158.0"),
            None,
            None,
        ));
    }

    #[test]
    fn verification_rejects_missing_active_binary_version() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            None,
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
    }

    #[test]
    fn forced_source_upgrade_accepts_same_version_with_new_build_identity() {
        assert!(upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            Some("homeboy 0.157.1+old"),
            Some("homeboy 0.157.1+new"),
        ));
    }

    #[test]
    fn forced_source_upgrade_requires_build_identity_for_same_version() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            None,
            Some("homeboy 0.157.1+new"),
        ));
    }

    #[test]
    fn parses_homeboy_version_output_with_build_identity() {
        let info = parse_cli_version_info("homeboy 0.158.0+abc123-dirty");

        assert_eq!(info.version.as_deref(), Some("0.158.0"));
        assert_eq!(
            info.build_identity.as_deref(),
            Some("homeboy 0.158.0+abc123-dirty")
        );
    }

    #[test]
    fn verify_retry_succeeds_after_transient_unreadable_binary() {
        // Issue #3463: the swap succeeded but the first read-back of the new
        // binary returns nothing (racing the just-replaced binary). A later
        // attempt reports the upgraded version and verification must succeed.
        let reads = std::cell::RefCell::new(vec![
            None,
            Some(ActiveBinaryInfo {
                version: Some("0.220.3".to_string()),
                build_identity: None,
            }),
        ]);
        let mut sleeps = 0u32;

        let (success, active) = verify_upgrade_with_retry(
            InstallMethod::Binary,
            false,
            "0.220.0",
            None,
            5,
            std::time::Duration::from_millis(0),
            || reads.borrow_mut().remove(0),
            |_| sleeps += 1,
        );

        assert!(success, "transient read-back failure should be retried");
        assert_eq!(
            active.and_then(|info| info.version).as_deref(),
            Some("0.220.3")
        );
        assert_eq!(sleeps, 1, "should sleep once between the two attempts");
    }

    #[test]
    fn verify_retry_succeeds_after_stale_old_version() {
        // The read-back briefly reports the old version before the new binary
        // is observable; the retry should pick up the upgraded version.
        let reads = std::cell::RefCell::new(vec![
            Some(ActiveBinaryInfo {
                version: Some("0.220.0".to_string()),
                build_identity: None,
            }),
            Some(ActiveBinaryInfo {
                version: Some("0.220.3".to_string()),
                build_identity: None,
            }),
        ]);

        let (success, active) = verify_upgrade_with_retry(
            InstallMethod::Binary,
            false,
            "0.220.0",
            None,
            5,
            std::time::Duration::from_millis(0),
            || reads.borrow_mut().remove(0),
            |_| {},
        );

        assert!(success);
        assert_eq!(
            active.and_then(|info| info.version).as_deref(),
            Some("0.220.3")
        );
    }

    #[test]
    fn verify_retry_first_attempt_success_does_not_sleep() {
        let mut sleeps = 0u32;

        let (success, active) = verify_upgrade_with_retry(
            InstallMethod::Binary,
            false,
            "0.220.0",
            None,
            5,
            std::time::Duration::from_millis(0),
            || {
                Some(ActiveBinaryInfo {
                    version: Some("0.220.3".to_string()),
                    build_identity: None,
                })
            },
            |_| sleeps += 1,
        );

        assert!(success);
        assert_eq!(
            active.and_then(|info| info.version).as_deref(),
            Some("0.220.3")
        );
        assert_eq!(sleeps, 0, "no retries needed when first read verifies");
    }

    #[test]
    fn verify_retry_exhausts_attempts_when_never_readable() {
        let mut reads = 0u32;
        let mut sleeps = 0u32;

        let (success, active) = verify_upgrade_with_retry(
            InstallMethod::Binary,
            false,
            "0.220.0",
            None,
            3,
            std::time::Duration::from_millis(0),
            || {
                reads += 1;
                None
            },
            |_| sleeps += 1,
        );

        assert!(
            !success,
            "genuinely unverifiable upgrade still reports false"
        );
        assert!(active.is_none());
        assert_eq!(reads, 3, "all attempts consumed");
        assert_eq!(sleeps, 2, "sleeps between attempts but not after the last");
    }

    #[test]
    fn verify_retry_reports_last_seen_version_on_exhaustion() {
        // The new version never becomes observable, but a stale old-version
        // read is retained so the caller can still surface a version string.
        let (success, active) = verify_upgrade_with_retry(
            InstallMethod::Binary,
            false,
            "0.220.0",
            None,
            2,
            std::time::Duration::from_millis(0),
            || {
                Some(ActiveBinaryInfo {
                    version: Some("0.220.0".to_string()),
                    build_identity: None,
                })
            },
            |_| {},
        );

        assert!(!success);
        assert_eq!(
            active.and_then(|info| info.version).as_deref(),
            Some("0.220.0")
        );
    }

    #[test]
    fn test_verify_upgrade_with_retry() {
        // Smoke test covering the happy path with a single immediate read.
        let (success, active) = verify_upgrade_with_retry(
            InstallMethod::Secondary,
            false,
            "0.157.1",
            None,
            1,
            std::time::Duration::from_millis(0),
            || {
                Some(ActiveBinaryInfo {
                    version: Some("0.158.0".to_string()),
                    build_identity: None,
                })
            },
            |_| {},
        );

        assert!(success);
        assert_eq!(
            active.and_then(|info| info.version).as_deref(),
            Some("0.158.0")
        );
    }

    #[test]
    fn test_resolve_source_workspace() {
        let dir = checkout_with_package_name("homeboy");

        let resolved = resolve_source_workspace(Some(dir.path())).expect("source checkout");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn source_workspace_rejects_non_homeboy_checkout() {
        let dir = checkout_with_package_name("other");

        let err = resolve_source_workspace(Some(dir.path())).expect_err("invalid checkout");

        assert!(err.message.contains("Homeboy source workspace"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--source-path")));
    }

    #[test]
    fn source_workspace_accepts_snapshot_without_git_metadata() {
        let dir = source_workspace_with_package_name("homeboy");

        let resolved = resolve_source_workspace(Some(dir.path())).expect("source snapshot");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn source_workspace_resolves_from_nested_checkout_path() {
        let dir = checkout_with_package_name("homeboy");
        let nested = dir.path().join("src/core");
        std::fs::create_dir_all(&nested).expect("nested dir");

        let resolved = resolve_source_workspace(Some(&nested)).expect("source checkout");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn executable_workspace_only_resolves_target_build_paths() {
        let path = Path::new("/repo/target/release/homeboy");
        assert_eq!(
            workspace_from_exe_path(path).as_deref(),
            Some(Path::new("/repo"))
        );

        let installed = Path::new("/usr/local/bin/homeboy");
        assert!(workspace_from_exe_path(installed).is_none());
    }

    #[test]
    fn binary_404_upgrade_error_suggests_source_fallback() {
        let err = upgrade_failure_error(
            InstallMethod::Binary,
            "curl: (22) The requested URL returned error: 404",
        );

        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("No release asset")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--source-path")));
    }

    #[test]
    fn missing_tool_upgrade_error_suggests_source_fallback() {
        let err = upgrade_failure_error(
            InstallMethod::Secondary,
            &format!(
                "sh: 1: {}: not found",
                defaults::secondary_install_method_key()
            ),
        );

        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Required executable")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--source-path")));
    }

    fn checkout_with_package_name(package_name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("git dir");
        write_source_workspace_files(dir.path(), package_name);
        dir
    }

    fn source_workspace_with_package_name(package_name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write_source_workspace_files(dir.path(), package_name);
        dir
    }

    fn write_source_workspace_files(path: &Path, package_name: &str) {
        let manifest = serde_json::json!({ "id": package_name });
        std::fs::write(path.join("homeboy.json"), manifest.to_string()).expect("manifest");
        let package_manifest = ["Car", "go.toml"].concat();
        std::fs::write(
            path.join(package_manifest),
            format!("[package]\nname = \"{package_name}\"\nversion = \"0.0.0\"\n"),
        )
        .expect("package manifest");
    }
}
