use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use homeboy_error::{Error, Result};

use crate::command;

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
    let common_dir = git_common_dir(repository, deadline)?;
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

fn git_common_dir(repository: &Path, deadline: Instant) -> Result<PathBuf> {
    git_common_dir_with_program(repository, deadline, Path::new("git"))
}

fn git_common_dir_with_program(
    repository: &Path,
    deadline: Instant,
    program: &Path,
) -> Result<PathBuf> {
    let mut git = Command::new(program);
    git.args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(repository)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command::isolate_process_tree(&mut git);
    let mut child = git
        .spawn()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let mut timed_out = false;
    let output = command::wait_with_bounded_output_until_cancelled(
        &mut child,
        command::DEFAULT_CAPTURE_LIMIT_BYTES,
        || {
            timed_out = Instant::now() >= deadline;
            timed_out
        },
    )
    .map_err(|error| Error::git_command_failed(error.to_string()))?
    .into_output();
    if timed_out {
        return Err(Error::git_command_failed(
            "resolve Git common directory deadline exhausted; terminated child process group",
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn common_directory_probe_deadline_reaps_stalled_git_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let pid_file = dir.path().join("descendant.pid");
        let git = dir.path().join("git");
        let script = format!(
            "#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n",
            crate::shell::quote_path(&pid_file.display().to_string())
        );
        fs::write(&git, script).expect("write stalled git");
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755))
            .expect("make stalled git executable");

        let started = Instant::now();
        let error =
            git_common_dir_with_program(dir.path(), Instant::now() + Duration::from_secs(1), &git)
                .expect_err("stalled common-directory probe must exhaust its deadline");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(error.message.contains("deadline exhausted"));
        let descendant_pid = fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant pid");
        assert!(
            !command::process_is_running(descendant_pid),
            "deadline left descendant {descendant_pid} runnable"
        );
    }
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
