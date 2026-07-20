use homeboy_core::defaults;
use homeboy_core::engine::shell::quote_path;
use homeboy_core::error::{Error, Result};
use homeboy_core::git::{run_git, run_git_output};
use homeboy_core::stream_capture::StreamCaptureMetadata;
use std::env;
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
    explicit_source_path: bool,
    force: bool,
    previous_build_identity: Option<&str>,
) -> Result<(bool, Option<String>, Option<String>, Option<String>)> {
    let defaults = defaults::load_defaults();
    let output = match method {
        InstallMethod::Homebrew => {
            let cmd = &defaults.install_methods.homebrew.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run homebrew upgrade".to_string()))
            })?
        }
        InstallMethod::Secondary => {
            // Legacy cargo-installed binaries are replaced with the release
            // asset now that Homeboy's private workspace is not on crates.io.
            let cmd = &defaults.install_methods.binary.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some("run release binary upgrade".to_string()),
                )
            })?
        }
        InstallMethod::Source => {
            let workspace_root = resolve_source_workspace(source_path)?;
            let source_revision = prepare_source_workspace_for_upgrade(&workspace_root)?;
            let built_binary = source_built_binary_path(&workspace_root);

            // Execute the upgrade command from defaults
            let cmd = source_upgrade_command_for_prepared_workspace(
                &defaults.install_methods.source.upgrade_command,
                &workspace_root,
                explicit_source_path,
            )?;
            run_source_upgrade_command(&cmd, &workspace_root, SOURCE_UPGRADE_TIMEOUT)?;
            let replacement_target = active_binary_path().ok();
            // Source command output is streamed to the invoking process so
            // controller timeouts can distinguish a build from a stalled run.
            // It has already returned a precise error for non-zero exits.
            return complete_source_upgrade(
                workspace_root,
                built_binary,
                replacement_target.as_deref(),
                method,
                force,
                previous_build_identity,
                source_revision,
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
    Ok((success, new_version, new_build_identity, None))
}

fn complete_source_upgrade(
    workspace_root: PathBuf,
    built_binary: PathBuf,
    replacement_target: Option<&Path>,
    method: InstallMethod,
    force: bool,
    previous_build_identity: Option<&str>,
    source_revision: Option<String>,
) -> Result<(bool, Option<String>, Option<String>, Option<String>)> {
    let replacement_target = replacement_target.ok_or_else(|| {
        Error::internal_unexpected("active binary path unavailable for source upgrade install")
    })?;
    upgrade_phase("installing source-built binary");
    install_source_built_binary(&built_binary, replacement_target)?;
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
        Some(&built_binary),
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
        Some(&built_binary),
        Some(replacement_target),
    ) {
        return Err(error);
    }

    upgrade_phase("source binary installation verified");
    Ok((success, new_version, new_build_identity, source_revision))
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
    built_binary: Option<&Path>,
    replacement_target: Option<&Path>,
) -> Option<Error> {
    if method != InstallMethod::Source || success {
        return None;
    }

    let observed = new_build_identity
        .or(new_version)
        .unwrap_or("an unverifiable version");

    let diagnostics =
        source_swap_failure_diagnostics(source_workspace, built_binary, replacement_target);
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
    built_binary: Option<&Path>,
    replacement_target: Option<&Path>,
) -> SourceSwapFailureDiagnostics {
    let active_path = replacement_target
        .map(Path::to_path_buf)
        .or_else(|| active_binary_path().ok());
    source_swap_failure_diagnostics_for_paths(
        source_workspace,
        built_binary,
        active_path.as_deref(),
    )
}

fn source_swap_failure_diagnostics_for_paths(
    source_workspace: Option<&Path>,
    built_binary: Option<&Path>,
    active_path: Option<&Path>,
) -> SourceSwapFailureDiagnostics {
    let active_path_text = active_path.map(display_path).unwrap_or_else(|| {
        "unresolved (command -v homeboy and current executable unavailable)".to_string()
    });
    let built_binary_text = built_binary
        .map(display_path)
        .unwrap_or_else(|| "unresolved (source workspace unavailable)".to_string());
    let replacement_path_text = active_path
        .map(display_path)
        .unwrap_or_else(|| "unresolved".to_string());
    let permissions = active_path
        .map(binary_replacement_permission_context)
        .unwrap_or_else(|| "active binary path unresolved; cannot inspect writability".to_string());

    let copy_command = built_binary.zip(active_path).map(|(built, active)| {
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

    let built_binary_command = built_binary.map(|built| {
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

fn install_source_built_binary(built_binary: &Path, replacement_target: &Path) -> Result<()> {
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

    std::fs::copy(built_binary, &temp_target).map_err(|e| {
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

pub(crate) fn prepare_source_workspace_for_upgrade(
    workspace_root: &Path,
) -> Result<Option<String>> {
    if !git_command_success(workspace_root, &["rev-parse", "--is-inside-work-tree"])? {
        return Ok(None);
    }

    // A caller-selected source path is an immutable build input. In particular,
    // worktree branches may be local-only or detached and must not be rewritten
    // to an origin branch before the source build or runner materialization.
    ensure_clean_source_workspace(workspace_root)?;
    source_workspace_revision(workspace_root).map(Some)
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

fn source_workspace_revision(workspace_root: &Path) -> Result<String> {
    let output = run_git_output(
        workspace_root,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "resolve source checkout revision",
    )?;
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || revision.is_empty() {
        return Err(Error::validation_invalid_argument(
            "source_path",
            "Source checkout does not resolve HEAD to an immutable commit",
            Some(workspace_root.display().to_string()),
            None,
        ));
    }

    Ok(revision)
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

const EXPLICIT_SOURCE_GIT_UPDATE_GUARD: &str = r#"HOMEBOY_REAL_GIT="$(command -v git)"
HOMEBOY_GIT_GUARD_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-git-guard.XXXXXX")"
cat > "$HOMEBOY_GIT_GUARD_DIR/git" <<'HOMEBOY_GIT_GUARD'
#!/bin/sh
for homeboy_git_arg in "$@"; do
  case "$homeboy_git_arg" in
    pull|fetch|reset)
      echo "Skipping git $homeboy_git_arg for explicitly selected source checkout"
      exit 0
      ;;
  esac
done
exec "$HOMEBOY_REAL_GIT" "$@"
HOMEBOY_GIT_GUARD
chmod 700 "$HOMEBOY_GIT_GUARD_DIR/git"
export HOMEBOY_REAL_GIT
PATH="$HOMEBOY_GIT_GUARD_DIR:$PATH"
export PATH"#;

fn source_upgrade_command_for_prepared_workspace(
    upgrade_command: &str,
    workspace_root: &Path,
    explicit_source_path: bool,
) -> Result<String> {
    if !git_command_success(workspace_root, &["rev-parse", "--is-inside-work-tree"])? {
        return Ok(upgrade_command.to_string());
    }

    if !explicit_source_path
        && git_command_success(workspace_root, &["symbolic-ref", "-q", "HEAD"])?
    {
        return Ok(upgrade_command.to_string());
    }

    Ok(format!(
        "{EXPLICIT_SOURCE_GIT_UPDATE_GUARD}\n\n(\n{upgrade_command}\n)\nhomeboy_upgrade_status=$?\nrm -rf \"$HOMEBOY_GIT_GUARD_DIR\"\nexit $homeboy_upgrade_status"
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

pub(crate) fn active_binary_path() -> Result<PathBuf> {
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
    built_binary: Option<&Path>,
    attempts: u32,
    delay: Duration,
    mut sleep: S,
) -> bool
where
    S: FnMut(Duration),
{
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        if source_install_matches_shell_resolved_binary(built_binary).unwrap_or(false) {
            return true;
        }

        if attempt + 1 < attempts {
            sleep(delay);
        }
    }

    false
}

fn source_install_matches_shell_resolved_binary(built_binary: Option<&Path>) -> Result<bool> {
    let Some(built_binary) = built_binary else {
        return Ok(false);
    };
    let active_binary = active_binary_path()?;

    source_install_matches_binary_path(built_binary, &active_binary)
}

fn source_install_matches_binary_path(built_binary: &Path, active_binary: &Path) -> Result<bool> {
    binary_files_match(built_binary, active_binary)
}

/// Cargo resolves a relative CARGO_TARGET_DIR from its current working directory.
/// Source upgrades run Cargo from the source workspace, so use that same base here.
fn source_built_binary_path(workspace_root: &Path) -> PathBuf {
    source_built_binary_path_for_target_dir(
        workspace_root,
        env::var_os("CARGO_TARGET_DIR").as_deref(),
    )
}

fn source_built_binary_path_for_target_dir(
    workspace_root: &Path,
    cargo_target_dir: Option<&std::ffi::OsStr>,
) -> PathBuf {
    let target_dir = cargo_target_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workspace_root.join(path)
            }
        })
        .unwrap_or_else(|| workspace_root.join("target"));

    target_dir.join("release/homeboy")
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
mod tests;
