use crate::core::defaults;
use crate::core::engine::shell::quote_path;
use crate::core::error::{Error, Result};
use crate::core::git::{run_git, run_git_output};
use crate::core::stream_capture::StreamCaptureMetadata;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
const SOURCE_UPGRADE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveBinaryInfo {
    pub version: Option<String>,
    pub build_identity: Option<String>,
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
            prepare_source_workspace_for_upgrade(&workspace_root)?;

            // Execute the upgrade command from defaults
            let cmd = source_upgrade_command_for_prepared_workspace(
                &defaults.install_methods.source.upgrade_command,
                &workspace_root,
            )?;
            run_source_upgrade_command(&cmd, &workspace_root, SOURCE_UPGRADE_TIMEOUT)?;
            let replacement_target = active_binary_path().ok();
            // Source command output is streamed to the invoking process so
            // controller timeouts can distinguish a build from a stalled run.
            // It has already returned a precise error for non-zero exits.
            return complete_source_upgrade(
                workspace_root,
                replacement_target.as_deref(),
                method,
                force,
                previous_build_identity,
            );
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

    // A source upgrade's command is responsible for swapping the freshly built
    // artifact into the active binary on disk. When that command exits 0 but the
    // read-back proves the active binary is unchanged, the swap silently
    // no-op'd (e.g. the artifact was built but never installed over the active
    // path, or `command -v` resolved a different binary than the one running).
    // Reporting a soft `upgraded: false` "completed" here lets operators
    // believe the roll-forward succeeded. Fail loudly with an actionable reason
    // so urgent source fixes are not silently dropped (#5772).
    Ok((success, new_version, new_build_identity))
}

fn complete_source_upgrade(
    workspace_root: PathBuf,
    replacement_target: Option<&Path>,
    method: InstallMethod,
    force: bool,
    previous_build_identity: Option<&str>,
) -> Result<(bool, Option<String>, Option<String>)> {
    let replacement_target = replacement_target.ok_or_else(|| {
        Error::internal_unexpected("active binary path unavailable for source upgrade install")
    })?;
    upgrade_phase("installing source-built binary");
    install_source_built_binary(&workspace_root, replacement_target)?;
    upgrade_phase("verifying installed source binary");

    let (_verified_version, active_binary) = verify_upgrade_with_retry(
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
    let success = verify_source_install_with_retry(
        Some(&workspace_root),
        VERIFY_READBACK_ATTEMPTS,
        VERIFY_READBACK_DELAY,
        std::thread::sleep,
    );

    if let Some(error) = source_swap_failure(
        method,
        success,
        new_version.as_deref(),
        new_build_identity.as_deref(),
        Some(&workspace_root),
        Some(replacement_target),
    ) {
        return Err(error);
    }

    upgrade_phase("source binary installation verified");
    Ok((success, new_version, new_build_identity))
}

fn upgrade_phase(phase: &str) {
    eprintln!("[upgrade] {phase}");
}

fn run_source_upgrade_command(
    command: &str,
    workspace_root: &Path,
    timeout: Duration,
) -> Result<()> {
    upgrade_phase("building source workspace");
    let mut child_command = Command::new("sh");
    child_command
        .args(["-c", command])
        .current_dir(workspace_root)
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        child_command.process_group(0);
    }
    let mut child = child_command
        .spawn()
        .map_err(|e| Error::internal_io(e.to_string(), Some("run source upgrade".to_string())))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(upgrade_failure_error(
                    InstallMethod::Source,
                    &format!("source upgrade command exited with {}", status),
                ));
            }
            Ok(None) if start.elapsed() >= timeout => {
                terminate_upgrade_child(&mut child);
                return Err(Error::internal_io(
                    format!(
                        "source upgrade timed out after {}s; child process group was terminated",
                        timeout.as_secs()
                    ),
                    Some("run source upgrade".to_string()),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                terminate_upgrade_child(&mut child);
                return Err(Error::internal_io(
                    e.to_string(),
                    Some("wait for source upgrade".to_string()),
                ));
            }
        }
    }
}

fn terminate_upgrade_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

/// Detect a source upgrade whose command exited successfully but left the
/// active binary unchanged (the swap silently no-op'd), and surface it as a
/// loud, actionable failure instead of a soft `upgraded: false` "completed".
///
/// Returns `None` for every other case (verified swap, or a non-source method
/// where a soft unverified result is reported by the caller).
fn source_swap_failure(
    method: InstallMethod,
    success: bool,
    new_version: Option<&str>,
    new_build_identity: Option<&str>,
    source_workspace: Option<&Path>,
    replacement_target: Option<&Path>,
) -> Option<Error> {
    if method != InstallMethod::Source || success {
        return None;
    }

    let observed = new_build_identity
        .or(new_version)
        .unwrap_or("an unverifiable version");

    let diagnostics = source_swap_failure_diagnostics(source_workspace, replacement_target);
    let mut error = Error::internal_unexpected(format!(
        "source upgrade command exited successfully but the active binary was not replaced (still {observed})"
    ))
    .with_hint("The source build succeeded, but replacing the active binary did not take effect.")
    .with_hint(format!("Active binary path: {}", diagnostics.active_path))
    .with_hint(format!(
        "Built source binary: {}",
        diagnostics.built_binary_path
    ))
    .with_hint(format!(
        "Replacement target path: {}",
        diagnostics.replacement_path
    ))
    .with_hint(format!("Permissions: {}", diagnostics.permissions));

    if let Some(command) = diagnostics.built_binary_command {
        error = error.with_hint(format!("Retry through just-built Homeboy: {command}"));
    }

    if let Some(command) = diagnostics.copy_command {
        error = error.with_hint(format!("Last-resort manual replacement: {command}"));
    }

    Some(error)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceSwapFailureDiagnostics {
    active_path: String,
    built_binary_path: String,
    replacement_path: String,
    permissions: String,
    copy_command: Option<String>,
    built_binary_command: Option<String>,
}

fn source_swap_failure_diagnostics(
    source_workspace: Option<&Path>,
    replacement_target: Option<&Path>,
) -> SourceSwapFailureDiagnostics {
    let active_path = replacement_target
        .map(Path::to_path_buf)
        .or_else(|| active_binary_path().ok());
    source_swap_failure_diagnostics_for_paths(source_workspace, active_path.as_deref())
}

fn source_swap_failure_diagnostics_for_paths(
    source_workspace: Option<&Path>,
    active_path: Option<&Path>,
) -> SourceSwapFailureDiagnostics {
    let built_binary = source_workspace.map(|path| path.join("target/release/homeboy"));
    let active_path_text = active_path.map(display_path).unwrap_or_else(|| {
        "unresolved (command -v homeboy and current executable unavailable)".to_string()
    });
    let built_binary_text = built_binary
        .as_deref()
        .map(display_path)
        .unwrap_or_else(|| "unresolved (source workspace unavailable)".to_string());
    let replacement_path_text = active_path
        .map(display_path)
        .unwrap_or_else(|| "unresolved".to_string());
    let permissions = active_path
        .map(binary_replacement_permission_context)
        .unwrap_or_else(|| "active binary path unresolved; cannot inspect writability".to_string());

    let copy_command = built_binary
        .as_deref()
        .zip(active_path)
        .map(|(built, active)| {
            let prefix = if replacement_target_may_be_writable(active) {
                "install"
            } else {
                "sudo install"
            };
            format!(
                "{prefix} -m 0755 {} {}",
                shell_quote_path(built),
                shell_quote_path(active)
            )
        });

    let built_binary_command = built_binary.as_deref().map(|built| {
        let mut command = format!(
            "{} upgrade --method source --source-path {} --force",
            shell_quote_path(built),
            shell_quote_path(source_workspace.unwrap_or_else(|| Path::new(".")))
        );
        command.push_str(" --skip-runners --skip-extensions");
        command
    });

    SourceSwapFailureDiagnostics {
        active_path: active_path_text,
        built_binary_path: built_binary_text,
        replacement_path: replacement_path_text,
        permissions,
        copy_command,
        built_binary_command,
    }
}

fn binary_replacement_permission_context(path: &Path) -> String {
    let parent = path.parent();
    let parent_context = parent
        .map(path_permission_context)
        .unwrap_or_else(|| "parent=<none>".to_string());
    format!(
        "active={}; parent={}",
        path_permission_context(path),
        parent_context
    )
}

fn path_permission_context(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let writable = !metadata.permissions().readonly();
            format!(
                "{} exists=true writable={}{}",
                display_path(path),
                writable,
                unix_mode_suffix(&metadata)
            )
        }
        Err(err) => format!(
            "{} exists=false writable=false metadata_error={}",
            display_path(path),
            err
        ),
    }
}

#[cfg(unix)]
fn unix_mode_suffix(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!(" mode={:o}", metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn unix_mode_suffix(_metadata: &std::fs::Metadata) -> String {
    String::new()
}

fn replacement_target_may_be_writable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| !metadata.permissions().readonly())
        .unwrap_or(false)
        || path
            .parent()
            .and_then(|parent| std::fs::metadata(parent).ok())
            .map(|metadata| !metadata.permissions().readonly())
            .unwrap_or(false)
}

fn install_source_built_binary(source_workspace: &Path, replacement_target: &Path) -> Result<()> {
    let built_binary = source_workspace.join("target/release/homeboy");
    let parent = replacement_target.parent().ok_or_else(|| {
        Error::internal_io(
            format!(
                "replacement target has no parent directory: {}",
                replacement_target.display()
            ),
            Some("install source-built binary".to_string()),
        )
    })?;
    let temp_target = parent.join(format!(".homeboy-upgrade.{}.tmp", std::process::id()));

    std::fs::copy(&built_binary, &temp_target).map_err(|e| {
        Error::internal_io(
            format!(
                "copy {} to {} failed: {}",
                built_binary.display(),
                temp_target.display(),
                e
            ),
            Some("install source-built binary".to_string()),
        )
    })?;

    if let Err(err) = make_source_install_executable(&temp_target) {
        let _ = std::fs::remove_file(&temp_target);
        return Err(err);
    }

    if let Err(err) = std::fs::rename(&temp_target, replacement_target) {
        let _ = std::fs::remove_file(&temp_target);
        return Err(Error::internal_io(
            format!(
                "rename {} to {} failed: {}",
                temp_target.display(),
                replacement_target.display(),
                err
            ),
            Some("install source-built binary".to_string()),
        ));
    }

    Ok(())
}

#[cfg(unix)]
fn make_source_install_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        Error::internal_io(
            format!("chmod 0755 {} failed: {}", path.display(), e),
            Some("install source-built binary".to_string()),
        )
    })
}

#[cfg(not(unix))]
fn make_source_install_executable(path: &Path) -> Result<()> {
    let mut permissions = std::fs::metadata(path)
        .map_err(|e| {
            Error::internal_io(
                format!("read permissions for {} failed: {}", path.display(), e),
                Some("install source-built binary".to_string()),
            )
        })?
        .permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        Error::internal_io(
            format!("make {} writable failed: {}", path.display(), e),
            Some("install source-built binary".to_string()),
        )
    })
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn shell_quote_path(path: &Path) -> String {
    quote_path(&path.display().to_string())
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

fn prepare_source_workspace_for_upgrade(workspace_root: &Path) -> Result<()> {
    if !git_command_success(workspace_root, &["rev-parse", "--is-inside-work-tree"])? {
        return Ok(());
    }

    // A caller-selected source path is an immutable build input. In particular,
    // worktree branches may be local-only or detached and must not be rewritten
    // to an origin branch before the source build or runner materialization.
    ensure_clean_source_workspace(workspace_root)
}

fn ensure_clean_source_workspace(workspace_root: &Path) -> Result<()> {
    let status = git_command_stdout(workspace_root, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "source_path",
        "Source checkout has uncommitted changes; refusing to prepare upgrade workspace",
        Some(workspace_root.display().to_string()),
        None,
    )
    .with_hint("Commit, stash, or discard local changes before running source upgrade."))
}

fn git_command_success(workspace_root: &Path, args: &[&str]) -> Result<bool> {
    Ok(
        run_git_output(workspace_root, args, "prepare source checkout")?
            .status
            .success(),
    )
}

fn git_command_stdout(workspace_root: &Path, args: &[&str]) -> Result<String> {
    run_git(workspace_root, args, "prepare source checkout").map(|stdout| stdout.trim().to_string())
}

const DETACHED_SOURCE_GIT_PULL_GUARD: &str = r#"git() {
  for homeboy_git_arg in "$@"; do
    if [ "$homeboy_git_arg" = "pull" ]; then
      echo "Skipping git pull for detached prepared source checkout"
      return 0
    fi
  done
  command git "$@"
}"#;

fn source_upgrade_command_for_prepared_workspace(
    upgrade_command: &str,
    workspace_root: &Path,
) -> Result<String> {
    if !git_command_success(workspace_root, &["rev-parse", "--is-inside-work-tree"])? {
        return Ok(upgrade_command.to_string());
    }

    if git_command_success(workspace_root, &["symbolic-ref", "-q", "HEAD"])? {
        return Ok(upgrade_command.to_string());
    }

    Ok(format!(
        "{DETACHED_SOURCE_GIT_PULL_GUARD}\n\n{upgrade_command}"
    ))
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
    active_binary_info_at(&exe_path)
}

fn active_binary_info_at(exe_path: &Path) -> Result<Option<ActiveBinaryInfo>> {
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
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

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

    if !force || active_version != previous_version {
        return false;
    }

    if method == InstallMethod::Source {
        match (previous_build_identity, active_build_identity) {
            (Some(previous), Some(active)) => active != previous,
            _ => true,
        }
    } else {
        true
    }
}

fn verify_source_install_with_retry<S>(
    source_workspace: Option<&Path>,
    attempts: u32,
    delay: Duration,
    mut sleep: S,
) -> bool
where
    S: FnMut(Duration),
{
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        if source_install_matches_shell_resolved_binary(source_workspace).unwrap_or(false) {
            return true;
        }

        if attempt + 1 < attempts {
            sleep(delay);
        }
    }

    false
}

fn source_install_matches_shell_resolved_binary(source_workspace: Option<&Path>) -> Result<bool> {
    let Some(source_workspace) = source_workspace else {
        return Ok(false);
    };
    let active_binary = active_binary_path()?;

    source_install_matches_binary_path(source_workspace, &active_binary)
}

fn source_install_matches_binary_path(
    source_workspace: &Path,
    active_binary: &Path,
) -> Result<bool> {
    let built_binary = source_workspace.join("target/release/homeboy");

    binary_files_match(&built_binary, active_binary)
}

fn binary_files_match(left: &Path, right: &Path) -> Result<bool> {
    let left_metadata = match std::fs::metadata(left) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(false),
    };
    let right_metadata = match std::fs::metadata(right) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(false),
    };
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }

    let left_contents = std::fs::read(left).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("verify source-built binary install".to_string()),
        )
    })?;
    let right_contents = std::fs::read(right).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("verify source-built binary install".to_string()),
        )
    })?;

    Ok(left_contents == right_contents)
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
    if identity.is_empty() {
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
    fn command_output_with_timeout_captures_child_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'homeboy 0.247.5'; printf 'warn' >&2"]);

        let output = command_output_with_timeout(&mut command, Duration::from_secs(5))
            .expect("command output");

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "homeboy 0.247.5");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "warn");
    }

    #[test]
    fn source_upgrade_command_returns_after_same_binary_success() {
        let workspace = tempfile::tempdir().expect("workspace");

        run_source_upgrade_command(
            "printf 'built same-version binary\\n'",
            workspace.path(),
            Duration::from_secs(1),
        )
        .expect("source command completes");
    }

    #[cfg(unix)]
    #[test]
    fn source_upgrade_timeout_terminates_the_entire_child_process_group() {
        let workspace = tempfile::tempdir().expect("workspace");
        let pid_file = workspace.path().join("child.pid");
        let command = format!("sleep 30 & echo $! > {}; wait", shell_quote_path(&pid_file));

        let err = run_source_upgrade_command(&command, workspace.path(), Duration::from_millis(50))
            .expect_err("long-running source command times out");
        assert!(
            err.details.to_string().to_lowercase().contains("timed out"),
            "unexpected timeout error: {err:?}"
        );

        let child_pid = std::fs::read_to_string(&pid_file)
            .expect("background child pid")
            .trim()
            .parse::<i32>()
            .expect("numeric pid");
        // The shell is the process-group leader and timeout termination must
        // remove its background child as well as reap the direct child.
        for _ in 0..40 {
            if unsafe { libc::kill(child_pid, 0) } != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("source-upgrade child must not be orphaned");
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
            false,
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
    fn forced_source_upgrade_rejects_unchanged_same_version_build_identity() {
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
    fn forced_secondary_upgrade_accepts_same_version_active_binary() {
        assert!(upgrade_verification_result(
            InstallMethod::Secondary,
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
    fn source_install_byte_match_rejects_same_version_stale_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let built = source.join("target/release/homeboy");
        let active = dir.path().join("bin/homeboy");
        std::fs::create_dir_all(built.parent().expect("built parent")).expect("built parent dir");
        std::fs::create_dir_all(active.parent().expect("active parent"))
            .expect("active parent dir");
        std::fs::write(&built, b"homeboy 0.281.20 with new source behavior")
            .expect("write built binary");
        std::fs::write(&active, b"homeboy 0.281.20 stale installed binary")
            .expect("write active binary");

        assert!(
            !upgrade_verification_result(
                InstallMethod::Source,
                true,
                "0.281.20",
                Some("0.281.20"),
                Some("homeboy 0.281.20"),
                Some("homeboy 0.281.20"),
            ),
            "identity-only verification cannot prove same-version source replacement"
        );
        assert!(
            !source_install_matches_binary_path(&source, &active).expect("compare binaries"),
            "same-version stale active binary must not verify"
        );

        std::fs::copy(&built, &active).expect("install built binary");

        assert!(
            source_install_matches_binary_path(&source, &active).expect("compare binaries"),
            "source upgrade only verifies after the active binary is the built artifact"
        );
    }

    #[test]
    fn forced_source_upgrade_accepts_same_version_without_build_identity() {
        assert!(upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            None,
            Some("homeboy 0.157.1+new"),
        ));
    }

    #[test]
    fn non_forced_upgrade_rejects_same_version_active_binary() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            false,
            "0.157.1",
            Some("0.157.1"),
            Some("homeboy 0.157.1+old"),
            Some("homeboy 0.157.1+old"),
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
    fn parses_plain_homeboy_version_output_as_build_identity() {
        let info = parse_cli_version_info("homeboy 0.158.0");

        assert_eq!(info.version.as_deref(), Some("0.158.0"));
        assert_eq!(info.build_identity.as_deref(), Some("homeboy 0.158.0"));
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
    fn source_swap_failure_errors_when_active_binary_unchanged() {
        // Issue #5772: the source upgrade command exited 0 but the read-back
        // proves the active binary was not replaced — fail loudly instead of a
        // soft `upgraded: false` "completed".
        let err = source_swap_failure(
            InstallMethod::Source,
            false,
            Some("0.247.5"),
            Some("homeboy 0.247.5+old"),
            Some(Path::new("/src/homeboy")),
            Some(Path::new("/active/homeboy")),
        )
        .expect("unverified source swap must surface an error");

        assert!(err.message.contains("active binary was not replaced"));
        assert!(err.message.contains("homeboy 0.247.5+old"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--method source")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Active binary path:")));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Built source binary: /src/homeboy/target/release/homeboy")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Replacement target path:")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Permissions:")));
    }

    #[test]
    fn source_swap_failure_reports_version_when_no_build_identity() {
        let err = source_swap_failure(
            InstallMethod::Source,
            false,
            Some("0.247.5"),
            None,
            Some(Path::new("/src/homeboy")),
            None,
        )
        .expect("unverified source swap must surface an error");

        assert!(err.message.contains("0.247.5"));
    }

    #[test]
    fn source_swap_failure_reports_placeholder_when_version_unverifiable() {
        let err = source_swap_failure(
            InstallMethod::Source,
            false,
            None,
            None,
            Some(Path::new("/src/homeboy")),
            None,
        )
        .expect("unverified source swap must surface an error");

        assert!(err.message.contains("an unverifiable version"));
    }

    #[test]
    fn source_swap_failure_silent_on_verified_swap() {
        assert!(
            source_swap_failure(
                InstallMethod::Source,
                true,
                Some("0.249.0"),
                None,
                Some(Path::new("/src/homeboy")),
                None,
            )
            .is_none(),
            "a verified source swap is not a failure"
        );
    }

    #[test]
    fn source_swap_failure_ignores_non_source_methods() {
        // Non-source methods keep their soft unverified reporting; only source
        // (where the swap is part of the command's contract) fails loudly here.
        assert!(source_swap_failure(
            InstallMethod::Binary,
            false,
            Some("0.247.5"),
            None,
            Some(Path::new("/src/homeboy")),
            None,
        )
        .is_none());
        assert!(source_swap_failure(
            InstallMethod::Secondary,
            false,
            Some("0.247.5"),
            None,
            Some(Path::new("/src/homeboy")),
            None,
        )
        .is_none());
    }

    #[test]
    fn source_swap_failure_diagnostics_use_replacement_target_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let target = dir.path().join("configured-runner-homeboy");
        std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
        std::fs::write(&target, "old").expect("target");

        let err = source_swap_failure(
            InstallMethod::Source,
            false,
            Some("0.255.8"),
            Some("homeboy 0.255.8+old"),
            Some(&source),
            Some(&target),
        )
        .expect("unverified source swap must surface an error");

        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains(&format!("Active binary path: {}", target.display()))));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains(&format!("Replacement target path: {}", target.display()))));
        let first_recovery = err
            .hints
            .iter()
            .find(|hint| hint.message.contains("Homeboy") || hint.message.contains("replacement"))
            .map(|hint| hint.message.as_str())
            .unwrap_or_default();
        assert!(first_recovery.contains("Retry through just-built Homeboy"));
    }

    #[test]
    fn source_swap_failure_diagnostics_include_paths_permissions_and_remediation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source checkout");
        let bin_dir = dir.path().join("bin dir");
        std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let active = bin_dir.join("homeboy active");
        let built = source.join("target/release/homeboy");
        std::fs::write(&active, "old").expect("active");
        std::fs::write(&built, "new").expect("built");

        let diagnostics = source_swap_failure_diagnostics_for_paths(Some(&source), Some(&active));

        assert_eq!(diagnostics.active_path, active.display().to_string());
        assert_eq!(diagnostics.built_binary_path, built.display().to_string());
        assert_eq!(diagnostics.replacement_path, active.display().to_string());
        assert!(diagnostics.permissions.contains("active="));
        assert!(diagnostics.permissions.contains("parent="));
        assert!(diagnostics.permissions.contains("writable="));
        let expected_copy_command = format!(
            "install -m 0755 '{}' '{}'",
            built.display(),
            active.display()
        );
        assert_eq!(
            diagnostics.copy_command.as_deref(),
            Some(expected_copy_command.as_str())
        );
        let built_command = diagnostics
            .built_binary_command
            .as_deref()
            .expect("built command");
        assert!(built_command.contains("upgrade --method source"));
        assert!(built_command.contains("--source-path"));
        assert!(built_command.contains(&source.display().to_string()));
    }

    #[test]
    fn install_source_built_binary_replaces_active_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let target_dir = dir.path().join("bin");
        std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
        std::fs::create_dir_all(&target_dir).expect("target dir");
        let built = source.join("target/release/homeboy");
        let active = target_dir.join("homeboy");
        std::fs::write(&built, "new homeboy").expect("built binary");
        std::fs::write(&active, "old homeboy").expect("active binary");

        install_source_built_binary(&source, &active).expect("install source binary");

        assert_eq!(
            std::fs::read_to_string(&active).expect("active"),
            "new homeboy"
        );
        assert!(binary_files_match(&built, &active).expect("files match"));
    }

    #[cfg(unix)]
    #[test]
    fn install_source_built_binary_sets_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let target_dir = dir.path().join("bin");
        std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
        std::fs::create_dir_all(&target_dir).expect("target dir");
        let built = source.join("target/release/homeboy");
        let active = target_dir.join("homeboy");
        std::fs::write(&built, "new homeboy").expect("built binary");
        std::fs::write(&active, "old homeboy").expect("active binary");

        install_source_built_binary(&source, &active).expect("install source binary");

        let mode = std::fs::metadata(&active)
            .expect("active metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn install_source_built_binary_reports_copy_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let target_dir = dir.path().join("bin");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&target_dir).expect("target dir");
        let active = target_dir.join("homeboy");
        std::fs::write(&active, "old homeboy").expect("active binary");

        let err = install_source_built_binary(&source, &active).expect_err("missing build fails");

        let details = err.details.to_string();
        assert!(details.contains("target/release/homeboy"));
        assert!(details.contains("install source-built binary"));
    }

    #[test]
    fn shell_quote_handles_paths_with_single_quotes() {
        assert_eq!(quote_path("/tmp/homeboy's/bin"), "'/tmp/homeboy'\\''s/bin'");
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
    fn source_upgrade_preparation_preserves_detached_checkout_identity() {
        let remote = tempfile::tempdir().expect("remote tempdir");
        git(
            remote.path(),
            &["init", "--bare", "--initial-branch", "main"],
        );

        let seed = source_workspace_with_package_name("homeboy");
        git(seed.path(), &["init", "--initial-branch", "main"]);
        git(seed.path(), &["config", "user.email", "test@example.com"]);
        git(seed.path(), &["config", "user.name", "Homeboy Test"]);
        git(seed.path(), &["add", "."]);
        git(seed.path(), &["commit", "-m", "initial"]);
        git(
            seed.path(),
            &[
                "remote",
                "add",
                "origin",
                &remote.path().display().to_string(),
            ],
        );
        git(seed.path(), &["push", "-u", "origin", "main"]);

        let checkout = tempfile::tempdir().expect("checkout tempdir");
        std::fs::remove_dir(checkout.path()).expect("remove placeholder checkout dir");
        git(
            remote.path(),
            &[
                "clone",
                &remote.path().display().to_string(),
                &checkout.path().display().to_string(),
            ],
        );

        std::fs::write(seed.path().join("src.txt"), "new source\n").expect("write source change");
        git(seed.path(), &["add", "."]);
        git(seed.path(), &["commit", "-m", "update source"]);
        git(seed.path(), &["push", "origin", "main"]);

        git(checkout.path(), &["switch", "--detach", "HEAD"]);
        let stale_head = git_stdout(checkout.path(), &["rev-parse", "HEAD"]);

        prepare_source_workspace_for_upgrade(checkout.path()).expect("prepare detached checkout");

        let prepared_head = git_stdout(checkout.path(), &["rev-parse", "HEAD"]);
        assert_eq!(prepared_head, stale_head);
        assert_eq!(
            git_stdout(checkout.path(), &["branch", "--show-current"]),
            ""
        );
    }

    #[test]
    fn source_upgrade_preparation_preserves_local_only_worktree_branch() {
        let remote = tempfile::tempdir().expect("remote tempdir");
        git(
            remote.path(),
            &["init", "--bare", "--initial-branch", "main"],
        );

        let seed = source_workspace_with_package_name("homeboy");
        git(seed.path(), &["init", "--initial-branch", "main"]);
        git(seed.path(), &["config", "user.email", "test@example.com"]);
        git(seed.path(), &["config", "user.name", "Homeboy Test"]);
        git(seed.path(), &["add", "."]);
        git(seed.path(), &["commit", "-m", "initial"]);
        git(
            seed.path(),
            &[
                "remote",
                "add",
                "origin",
                &remote.path().display().to_string(),
            ],
        );
        git(seed.path(), &["push", "-u", "origin", "main"]);

        let root = tempfile::tempdir().expect("root tempdir");
        let main_checkout = root.path().join("main-checkout");
        git(
            root.path(),
            &[
                "clone",
                &remote.path().display().to_string(),
                &main_checkout.display().to_string(),
            ],
        );

        let source_worktree = root.path().join("source-worktree");
        git(
            &main_checkout,
            &[
                "worktree",
                "add",
                "-b",
                "feature-upgrade-source",
                &source_worktree.display().to_string(),
                "HEAD",
            ],
        );

        std::fs::write(seed.path().join("src.txt"), "new source\n").expect("write source change");
        git(seed.path(), &["add", "."]);
        git(seed.path(), &["commit", "-m", "update source"]);
        git(seed.path(), &["push", "origin", "main"]);

        let switch_main = std::process::Command::new("git")
            .arg("-C")
            .arg(&source_worktree)
            .args(["switch", "main"])
            .output()
            .expect("run git switch main");
        assert!(
            !switch_main.status.success(),
            "test setup should reproduce branch ownership failure"
        );

        let source_head = git_stdout(&source_worktree, &["rev-parse", "HEAD"]);
        prepare_source_workspace_for_upgrade(&source_worktree).expect("prepare source worktree");

        assert_eq!(
            git_stdout(&source_worktree, &["branch", "--show-current"]),
            "feature-upgrade-source"
        );
        assert_eq!(
            git_stdout(&source_worktree, &["rev-parse", "HEAD"]),
            source_head
        );
    }

    #[test]
    fn source_upgrade_preparation_rejects_dirty_source_checkout() {
        let remote = tempfile::tempdir().expect("remote tempdir");
        git(
            remote.path(),
            &["init", "--bare", "--initial-branch", "main"],
        );

        let seed = source_workspace_with_package_name("homeboy");
        git(seed.path(), &["init", "--initial-branch", "main"]);
        git(seed.path(), &["config", "user.email", "test@example.com"]);
        git(seed.path(), &["config", "user.name", "Homeboy Test"]);
        git(seed.path(), &["add", "."]);
        git(seed.path(), &["commit", "-m", "initial"]);
        git(
            seed.path(),
            &[
                "remote",
                "add",
                "origin",
                &remote.path().display().to_string(),
            ],
        );
        git(seed.path(), &["push", "-u", "origin", "main"]);
        std::fs::write(seed.path().join("uncommitted.txt"), "dirty\n").expect("dirty file");

        let err = prepare_source_workspace_for_upgrade(seed.path()).expect_err("dirty rejected");

        assert!(err.message.contains("uncommitted changes"));
    }

    #[test]
    fn source_upgrade_command_preserves_branch_checkout_command() {
        let checkout = source_workspace_with_package_name("homeboy");
        git(checkout.path(), &["init", "--initial-branch", "main"]);

        let command = source_upgrade_command_for_prepared_workspace(
            "git pull --ff-only\ncargo build --release",
            checkout.path(),
        )
        .expect("source command");

        assert_eq!(command, "git pull --ff-only\ncargo build --release");
    }

    #[test]
    fn source_upgrade_command_skips_pull_for_detached_prepared_checkout() {
        let checkout = source_workspace_with_package_name("homeboy");
        git(checkout.path(), &["init", "--initial-branch", "main"]);
        git(
            checkout.path(),
            &["config", "user.email", "test@example.com"],
        );
        git(checkout.path(), &["config", "user.name", "Homeboy Test"]);
        git(checkout.path(), &["add", "."]);
        git(checkout.path(), &["commit", "-m", "initial"]);
        git(checkout.path(), &["switch", "--detach", "HEAD"]);

        let command = source_upgrade_command_for_prepared_workspace(
            "git pull --ff-only\ncargo build --release",
            checkout.path(),
        )
        .expect("source command");

        assert!(command.contains("Skipping git pull for detached prepared source checkout"));
        assert!(command.contains("command git \"$@\""));
        assert!(command.ends_with("git pull --ff-only\ncargo build --release"));
    }

    #[test]
    fn source_upgrade_command_preserves_snapshot_command() {
        let checkout = source_workspace_with_package_name("homeboy");

        let command =
            source_upgrade_command_for_prepared_workspace("cargo build --release", checkout.path())
                .expect("source command");

        assert_eq!(command, "cargo build --release");
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

    fn git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn git_stdout(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
