use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use homeboy_error::{Error, Result};

const WAIT_INTERVAL: Duration = Duration::from_millis(50);
const LOCK_FILE: &str = "homeboy-remote-tracking.lock";

static AUTHORITIES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// Serialize operations that update remote-tracking refs for one repository.
/// Linked worktrees share a Git common directory; unrelated repositories do not.
pub fn with_remote_tracking_authority_until<T>(
    repository: &Path,
    operation: &str,
    deadline: Instant,
    action: impl FnOnce(Duration) -> Result<T>,
) -> Result<T> {
    report_authority_attempt();
    let common_dir = git_common_dir(repository)?;
    let authority = {
        let mut authorities = AUTHORITIES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        authorities
            .entry(common_dir.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _process_guard = acquire_process_guard(&authority, &common_dir, operation, deadline)?;
    let lock_path = common_dir.join(LOCK_FILE);
    let mut lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| authority_error(operation, &common_dir, &lock_path, None, error))?;
    acquire_file_guard(&mut lock, &common_dir, &lock_path, operation, deadline)?;
    action(remaining(
        deadline,
        operation,
        &common_dir,
        &lock_path,
        None,
    )?)
}

fn git_common_dir(repository: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(repository)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "resolve Git common directory for {}",
            repository.display()
        )));
    }
    fs::canonicalize(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
    .map_err(|error| Error::git_command_failed(error.to_string()))
}

fn acquire_process_guard<'a>(
    authority: &'a Mutex<()>,
    common_dir: &Path,
    operation: &str,
    deadline: Instant,
) -> Result<std::sync::MutexGuard<'a, ()>> {
    loop {
        match authority.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(WAIT_INTERVAL)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(authority_error(
                    operation,
                    common_dir,
                    &common_dir.join(LOCK_FILE),
                    Some("another Homeboy operation in this process".to_string()),
                    timed_out_error(),
                ))
            }
        }
    }
}

fn acquire_file_guard(
    lock: &mut File,
    common_dir: &Path,
    lock_path: &Path,
    operation: &str,
    deadline: Instant,
) -> Result<()> {
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => {
                let owner = format!("pid={} operation={}\n", std::process::id(), operation);
                lock.set_len(0)
                    .and_then(|()| lock.write_all(owner.as_bytes()))
                    .and_then(|()| lock.sync_data())
                    .map_err(|error| {
                        authority_error(operation, common_dir, lock_path, None, error)
                    })?;
                return Ok(());
            }
            Ok(false) | Err(_) if Instant::now() < deadline => {
                report_authority_attempt();
                thread::sleep(WAIT_INTERVAL);
            }
            Ok(false) => {
                return Err(authority_error(
                    operation,
                    common_dir,
                    lock_path,
                    lock_owner(lock_path),
                    timed_out_error(),
                ))
            }
            Err(error) => {
                return Err(authority_error(
                    operation,
                    common_dir,
                    lock_path,
                    lock_owner(lock_path),
                    error,
                ))
            }
        }
    }
}

fn lock_owner(lock_path: &Path) -> Option<String> {
    fs::read_to_string(lock_path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn remaining(
    deadline: Instant,
    operation: &str,
    common_dir: &Path,
    lock_path: &Path,
    owner: Option<String>,
) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| authority_error(operation, common_dir, lock_path, owner, timed_out_error()))
}

fn timed_out_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "authority deadline exhausted")
}

fn authority_error(
    operation: &str,
    common_dir: &Path,
    lock_path: &Path,
    owner: Option<String>,
    error: std::io::Error,
) -> Error {
    let owner = owner
        .filter(|owner| !owner.is_empty())
        .unwrap_or_else(|| "unknown owner".to_string());
    Error::git_command_failed(format!("{operation} exhausted its caller deadline waiting for remote-tracking authority at {} (owner: {}; lock: {}): {error}", common_dir.display(), owner, lock_path.display()))
}

fn report_authority_attempt() {
    if let Some(path) = std::env::var_os("HOMEB0Y_REMOTE_TRACKING_FETCH_LOCK_ATTEMPTED") {
        let _ = std::fs::write(path, "attempted\n");
    }
}
