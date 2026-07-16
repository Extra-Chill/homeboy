use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::engine::run_dir::RunDir;
use crate::error::{Error, ErrorCode, Result};
use crate::paths;

#[derive(Debug)]
pub(super) struct TraceOverlayLock {
    path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TraceOverlayLockHolder {
    pid: u32,
    component_path: String,
    run_dir: String,
    acquired_at: String,
    command: String,
    #[serde(default)]
    overlay_paths: Vec<String>,
    #[serde(default)]
    touched_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceOverlayLockRecord {
    pub lock_path: String,
    pub status: TraceOverlayLockStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<TraceOverlayLockHolder>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceOverlayLockStatus {
    Active,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceOverlayLockCleanupResult {
    pub removed: Vec<TraceOverlayLockRecord>,
    pub kept: Vec<TraceOverlayLockRecord>,
}

impl TraceOverlayLock {
    pub(super) fn acquire(
        component_path: &Path,
        overlay_paths: &[String],
        run_dir: &RunDir,
    ) -> Result<Self> {
        let lock_dir = trace_overlay_lock_dir()?;
        let normalized_component_path = normalize_component_path(component_path);
        let path = lock_dir.join(format!(
            "{}.lock",
            trace_overlay_lock_id(&normalized_component_path)
        ));

        match fs::create_dir(&path) {
            Ok(()) => {
                let touched_files = trace_overlay_touched_files_for_paths(
                    &normalized_component_path,
                    overlay_paths,
                )?;
                let holder = TraceOverlayLockHolder {
                    pid: std::process::id(),
                    component_path: normalized_component_path.to_string_lossy().to_string(),
                    run_dir: run_dir.path().to_string_lossy().to_string(),
                    acquired_at: chrono::Utc::now().to_rfc3339(),
                    command: std::env::args().collect::<Vec<_>>().join(" "),
                    overlay_paths: overlay_paths.to_vec(),
                    touched_files,
                };
                let holder_path = path.join("holder.json");
                if let Err(error) = write_trace_overlay_lock_holder(&holder_path, &holder) {
                    let _ = fs::remove_dir_all(&path);
                    return Err(error);
                }
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = read_trace_overlay_lock_holder(&path);
                Err(trace_overlay_lock_error(
                    &normalized_component_path,
                    &path,
                    run_dir,
                    holder,
                ))
            }
            Err(e) => Err(Error::internal_io(
                format!(
                    "Failed to acquire trace overlay lock {}: {}",
                    path.display(),
                    e
                ),
                Some("trace.overlay.lock.acquire".to_string()),
            )),
        }
    }
}

impl Drop for TraceOverlayLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn list_trace_overlay_locks() -> Result<Vec<TraceOverlayLockRecord>> {
    let lock_dir = trace_overlay_lock_dir()?;
    let entries = fs::read_dir(&lock_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read trace overlay lock dir {}: {}",
                lock_dir.display(),
                e
            ),
            Some("trace.overlay.lock.read_dir".to_string()),
        )
    })?;
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                format!("Failed to read trace overlay lock entry: {e}"),
                Some("trace.overlay.lock.read_entry".to_string()),
            )
        })?;
        let path = entry.path();
        if !path.is_dir() || path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        records.push(read_trace_overlay_lock_record(&path));
    }
    records.sort_by(|a, b| a.lock_path.cmp(&b.lock_path));
    Ok(records)
}

pub fn cleanup_stale_trace_overlay_locks(force: bool) -> Result<TraceOverlayLockCleanupResult> {
    let locks = list_trace_overlay_locks()?;
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for lock in locks {
        if lock.status != TraceOverlayLockStatus::Stale {
            kept.push(lock);
            continue;
        }
        if !force {
            ensure_trace_overlay_lock_touched_files_clean(&lock)?;
        }
        fs::remove_dir_all(&lock.lock_path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to remove stale trace overlay lock {}: {}",
                    lock.lock_path, e
                ),
                Some("trace.overlay.lock.cleanup".to_string()),
            )
        })?;
        removed.push(lock);
    }
    Ok(TraceOverlayLockCleanupResult { removed, kept })
}

fn trace_overlay_lock_dir() -> Result<PathBuf> {
    let lock_dir = paths::homeboy_data()?.join("trace-overlay-locks");
    fs::create_dir_all(&lock_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create trace overlay lock dir {}: {}",
                lock_dir.display(),
                e
            ),
            Some("trace.overlay.lock.create_dir".to_string()),
        )
    })?;
    Ok(lock_dir)
}

fn read_trace_overlay_lock_record(lock_path: &Path) -> TraceOverlayLockRecord {
    let holder = read_trace_overlay_lock_holder(lock_path);
    let status = holder
        .as_ref()
        .map(trace_overlay_lock_status)
        .unwrap_or(TraceOverlayLockStatus::Unknown);
    TraceOverlayLockRecord {
        lock_path: lock_path.to_string_lossy().to_string(),
        status,
        holder,
    }
}

fn trace_overlay_lock_status(holder: &TraceOverlayLockHolder) -> TraceOverlayLockStatus {
    if process_is_alive(holder.pid) {
        TraceOverlayLockStatus::Active
    } else {
        TraceOverlayLockStatus::Stale
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        if libc::kill(pid as libc::pid_t, 0) == 0 {
            return true;
        }
        last_errno() == libc::EPERM
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
unsafe fn last_errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

#[cfg(all(
    unix,
    any(target_os = "macos", target_os = "ios", target_os = "freebsd")
))]
unsafe fn last_errno() -> i32 {
    unsafe { *libc::__error() }
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

pub(super) fn normalize_component_path(component_path: &Path) -> PathBuf {
    fs::canonicalize(component_path).unwrap_or_else(|_| component_path.to_path_buf())
}

pub(super) fn trace_overlay_lock_id(component_path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(component_path.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    hex[..24].to_string()
}

fn write_trace_overlay_lock_holder(path: &Path, holder: &TraceOverlayLockHolder) -> Result<()> {
    let content = serde_json::to_string_pretty(holder).map_err(|e| {
        Error::internal_json(e.to_string(), Some("trace.overlay.lock.holder".to_string()))
    })?;
    fs::write(path, content).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace overlay lock holder {}: {}",
                path.display(),
                e
            ),
            Some("trace.overlay.lock.write_holder".to_string()),
        )
    })
}

pub(super) fn read_trace_overlay_lock_holder(lock_path: &Path) -> Option<TraceOverlayLockHolder> {
    let holder_path = lock_path.join("holder.json");
    let content = fs::read_to_string(holder_path).ok()?;
    serde_json::from_str(&content).ok()
}

fn trace_overlay_lock_error(
    component_path: &Path,
    lock_path: &Path,
    run_dir: &RunDir,
    holder: Option<TraceOverlayLockHolder>,
) -> Error {
    let holder_summary = holder
        .as_ref()
        .and_then(trace_overlay_holder_summary)
        .unwrap_or_else(|| "unavailable".to_string());
    let status = holder
        .as_ref()
        .map(trace_overlay_lock_status)
        .unwrap_or(TraceOverlayLockStatus::Unknown);
    let status_label = match status {
        TraceOverlayLockStatus::Active => "active",
        TraceOverlayLockStatus::Stale => "stale",
        TraceOverlayLockStatus::Unknown => "unknown",
    };
    let holder_label = if status == TraceOverlayLockStatus::Stale {
        "Dead holder"
    } else {
        "Active holder"
    };
    let message_prefix = if status == TraceOverlayLockStatus::Stale {
        "Trace overlay lock is stale"
    } else {
        "Trace overlay already active"
    };
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "{} for component path {}. Lock path: {}. {}: {}. Current run directory: {}",
            message_prefix,
            component_path.display(),
            lock_path.display(),
            holder_label,
            holder_summary,
            run_dir.path().display()
        ),
        serde_json::json!({
            "field": "--overlay",
            "component_path": component_path.to_string_lossy(),
            "lock_path": lock_path.to_string_lossy(),
            "run_dir": run_dir.path().to_string_lossy(),
            "lock_status": status_label,
            "holder": holder,
        }),
    )
    .with_hint("Inspect locks: homeboy trace overlay-locks list")
    .with_hint(
        "Remove stale locks after safety checks: homeboy trace overlay-locks cleanup --stale",
    )
}

fn trace_overlay_holder_summary(holder: &TraceOverlayLockHolder) -> Option<String> {
    Some(format!(
        "pid {}, run directory {}, acquired at {}",
        holder.pid, holder.run_dir, holder.acquired_at
    ))
}

fn trace_overlay_touched_files_for_paths(
    component_path: &Path,
    overlay_paths: &[String],
) -> Result<Vec<String>> {
    let mut touched_files = Vec::new();
    for overlay_path in overlay_paths {
        for touched_file in
            super::overlay::overlay_touched_files(component_path, Path::new(overlay_path))?
        {
            if !touched_files.contains(&touched_file) {
                touched_files.push(touched_file);
            }
        }
    }
    Ok(touched_files)
}

fn ensure_trace_overlay_lock_touched_files_clean(lock: &TraceOverlayLockRecord) -> Result<()> {
    let Some(holder) = &lock.holder else {
        return Err(Error::validation_invalid_argument(
            "--stale",
            format!(
                "trace overlay lock {} has no holder metadata; pass --force to remove it",
                lock.lock_path
            ),
            None,
            None,
        ));
    };
    if holder.touched_files.is_empty() {
        return Ok(());
    }
    let component_path = Path::new(&holder.component_path);
    let dirty = trace_overlay_dirty_files(component_path, &holder.touched_files, &lock.lock_path)?;
    if dirty.trim().is_empty() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "--stale",
        format!(
            "stale trace overlay lock {} touches dirty files; pass --force to remove the lock anyway",
            lock.lock_path
        ),
        Some(dirty),
        None,
    ))
}

fn trace_overlay_dirty_files(
    component_path: &Path,
    touched_files: &[String],
    context_path: &str,
) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(["status", "--porcelain=v1", "--"])
        .args(touched_files)
        .current_dir(component_path);
    let output = command.output().map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to check trace overlay target status for {}: {}",
                context_path, e
            ),
            Some("trace.overlay.status".to_string()),
        )
    })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "--overlay",
            format!("failed to check overlay target status for {}", context_path),
            Some(String::from_utf8_lossy(&output.stderr).to_string()),
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn test_acquire() {
        with_isolated_home(|_| {
            let component_dir = tempfile::tempdir().unwrap();
            let run_dir = RunDir::create().unwrap();
            let lock_path;

            {
                let lock = TraceOverlayLock::acquire(component_dir.path(), &[], &run_dir).unwrap();
                lock_path = lock.path.clone();
                assert!(lock_path.exists());
                assert!(lock_path.join("holder.json").exists());
            }

            assert!(!lock_path.exists());
            run_dir.cleanup();
        });
    }

    #[test]
    fn test_normalize_component_path() {
        let component_dir = tempfile::tempdir().unwrap();

        let normalized = normalize_component_path(component_dir.path());

        assert!(normalized.is_absolute());
        assert_eq!(normalized, fs::canonicalize(component_dir.path()).unwrap());
    }

    #[test]
    fn test_trace_overlay_lock_id() {
        let first = trace_overlay_lock_id(Path::new("/tmp/component-a"));
        let second = trace_overlay_lock_id(Path::new("/tmp/component-b"));

        assert_eq!(first.len(), 24);
        assert_ne!(first, second);
    }

    #[test]
    fn test_read_trace_overlay_lock_holder() {
        with_isolated_home(|_| {
            let component_dir = tempfile::tempdir().unwrap();
            let run_dir = RunDir::create().unwrap();
            let lock = TraceOverlayLock::acquire(component_dir.path(), &[], &run_dir).unwrap();

            let holder = read_trace_overlay_lock_holder(&lock.path).unwrap();

            assert_eq!(holder.pid, std::process::id());
            assert_eq!(holder.run_dir, run_dir.path().to_string_lossy());
            drop(lock);
            run_dir.cleanup();
        });
    }

    #[test]
    fn trace_overlay_lock_contention_fails_fast_with_context() {
        with_isolated_home(|_| {
            let component_dir = tempfile::tempdir().unwrap();
            let first_run_dir = RunDir::create().unwrap();
            let second_run_dir = RunDir::create().unwrap();
            let lock =
                TraceOverlayLock::acquire(component_dir.path(), &[], &first_run_dir).unwrap();

            let err =
                TraceOverlayLock::acquire(component_dir.path(), &[], &second_run_dir).unwrap_err();

            assert!(err.message.contains("Trace overlay already active"));
            assert!(err
                .message
                .contains(&component_dir.path().display().to_string()));
            assert!(err.message.contains(&lock.path.display().to_string()));
            assert!(err
                .message
                .contains(&first_run_dir.path().display().to_string()));
            assert!(err
                .message
                .contains(&second_run_dir.path().display().to_string()));
            assert_eq!(
                err.details["component_path"].as_str(),
                Some(
                    normalize_component_path(component_dir.path())
                        .to_str()
                        .unwrap()
                )
            );
            assert_eq!(
                err.details["lock_path"].as_str(),
                Some(lock.path.to_str().unwrap())
            );
            assert_eq!(err.details["lock_status"].as_str(), Some("active"));

            drop(lock);
            first_run_dir.cleanup();
            second_run_dir.cleanup();
        });
    }

    #[test]
    fn test_list_trace_overlay_locks() {
        with_isolated_home(|_| {
            let component_dir = tempfile::tempdir().unwrap();
            let run_dir = RunDir::create().unwrap();
            let lock = TraceOverlayLock::acquire(component_dir.path(), &[], &run_dir).unwrap();

            let locks = list_trace_overlay_locks().unwrap();

            assert_eq!(locks.len(), 1);
            assert_eq!(locks[0].status, TraceOverlayLockStatus::Active);
            assert_eq!(locks[0].lock_path, lock.path.to_string_lossy());
            assert_eq!(locks[0].holder.as_ref().unwrap().pid, std::process::id());

            drop(lock);
            run_dir.cleanup();
        });
    }

    #[test]
    fn test_cleanup_stale_trace_overlay_locks() {
        with_isolated_home(|_| {
            let component_dir = tempfile::tempdir().unwrap();
            init_git_repo(component_dir.path());
            let lock_path = write_test_overlay_lock(component_dir.path(), dead_test_pid());

            let result = cleanup_stale_trace_overlay_locks(false).unwrap();

            assert_eq!(result.removed.len(), 1);
            assert_eq!(result.removed[0].status, TraceOverlayLockStatus::Stale);
            assert!(!lock_path.exists());
        });
    }

    #[test]
    fn trace_overlay_locks_cleanup_refuses_dead_holder_with_dirty_checkout() {
        with_isolated_home(|_| {
            let component_dir = tempfile::tempdir().unwrap();
            init_git_repo(component_dir.path());
            let lock_path = write_test_overlay_lock(component_dir.path(), dead_test_pid());
            fs::write(component_dir.path().join("scenario.txt"), "dirty\n").unwrap();

            let err = cleanup_stale_trace_overlay_locks(false).unwrap_err();

            assert!(err.message.contains("touches dirty files"));
            assert!(lock_path.exists());

            let forced = cleanup_stale_trace_overlay_locks(true).unwrap();
            assert_eq!(forced.removed.len(), 1);
            assert!(!lock_path.exists());
        });
    }

    fn init_git_repo(path: &Path) {
        fs::write(path.join("scenario.txt"), "base\n").unwrap();
        git(path, &["init"]);
        git(path, &["add", "scenario.txt"]);
        git(
            path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "init",
            ],
        );
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_test_overlay_lock(component_dir: &Path, pid: u32) -> PathBuf {
        let component_path = normalize_component_path(component_dir);
        let lock_dir = trace_overlay_lock_dir().unwrap();
        let lock_path = lock_dir.join(format!("{}.lock", trace_overlay_lock_id(&component_path)));
        fs::create_dir_all(&lock_path).unwrap();
        let holder = TraceOverlayLockHolder {
            pid,
            component_path: component_path.to_string_lossy().to_string(),
            run_dir: component_dir.join("run").to_string_lossy().to_string(),
            acquired_at: "2026-05-02T00:00:00Z".to_string(),
            command: "homeboy trace example overlay --overlay overlay.patch".to_string(),
            overlay_paths: vec!["overlay.patch".to_string()],
            touched_files: vec!["scenario.txt".to_string()],
        };
        write_trace_overlay_lock_holder(&lock_path.join("holder.json"), &holder).unwrap();
        lock_path
    }

    fn dead_test_pid() -> u32 {
        999_999
    }
}
