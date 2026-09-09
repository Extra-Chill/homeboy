use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

use crate::error::{Error, Result};

const WAIT_INTERVAL: Duration = Duration::from_millis(50);
const LOCK_FILE: &str = "homeboy-remote-tracking.lock";

static AUTHORITIES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// Serialize Homeboy operations that can update remote-tracking refs for one
/// repository. Linked worktrees share a Git common directory, while unrelated
/// repositories retain independent concurrency.
pub fn with_remote_tracking_authority_until<T>(
    repository: &Path,
    operation: &str,
    deadline: Instant,
    action: impl FnOnce(Duration) -> Result<T>,
) -> Result<T> {
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
    let common_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    fs::canonicalize(&common_dir).map_err(|error| Error::git_command_failed(error.to_string()))
}

fn acquire_process_guard<'a>(
    authority: &'a Mutex<()>,
    common_dir: &Path,
    operation: &str,
    deadline: Instant,
) -> Result<std::sync::MutexGuard<'a, ()>> {
    let mut reported_wait = false;
    loop {
        match authority.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                if !reported_wait {
                    crate::log_status!(
                        "git",
                        "phase=remote_tracking_wait operation={} common_dir={} owner=local_process",
                        operation,
                        common_dir.display()
                    );
                    reported_wait = true;
                }
                thread::sleep(WAIT_INTERVAL)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(authority_error(
                    operation,
                    common_dir,
                    &common_dir.join(LOCK_FILE),
                    Some("another Homeboy operation in this process".to_string()),
                    timed_out_error(),
                ));
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
    let mut reported_wait = false;
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
                if !reported_wait {
                    let owner = fs::read_to_string(lock_path)
                        .ok()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "unknown owner".to_string());
                    crate::log_status!(
                        "git",
                        "phase=remote_tracking_wait operation={} common_dir={} owner={}",
                        operation,
                        common_dir.display(),
                        owner
                    );
                    reported_wait = true;
                }
                thread::sleep(WAIT_INTERVAL)
            }
            Ok(false) => {
                let error = timed_out_error();
                let owner = fs::read_to_string(lock_path)
                    .ok()
                    .map(|value| value.trim().to_string());
                return Err(authority_error(
                    operation, common_dir, lock_path, owner, error,
                ));
            }
            Err(error) if Instant::now() >= deadline => {
                let owner = fs::read_to_string(lock_path)
                    .ok()
                    .map(|value| value.trim().to_string());
                return Err(authority_error(
                    operation, common_dir, lock_path, owner, error,
                ));
            }
            Err(error) => {
                let owner = fs::read_to_string(lock_path)
                    .ok()
                    .map(|value| value.trim().to_string());
                return Err(authority_error(
                    operation, common_dir, lock_path, owner, error,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::mpsc;

    fn git(path: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git")
            .status
            .success());
    }

    fn repository(path: &Path) {
        git(path, &["init", "-q"]);
        git(path, &["config", "user.email", "homeboy@example.test"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(path.join("README"), "fixture\n").expect("write fixture");
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "fixture"]);
    }

    fn git_stdout(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {:?} failed", args);
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_string()
    }

    #[test]
    fn two_process_sibling_worktrees_fetch_requested_revisions() {
        const CHILD: &str = "HOMEB0Y_REMOTE_TRACKING_FETCH_CHILD";
        if let (Some(path), Some(revision)) = (
            std::env::var_os(CHILD),
            std::env::var_os("HOMEB0Y_REMOTE_TRACKING_FETCH_REVISION"),
        ) {
            crate::git::fetch_remote_tracking_refs_until(
                Path::new(&path),
                &["fetch", "origin", revision.to_str().expect("revision")],
                "two-process remote-tracking fetch",
                &[],
                Instant::now() + Duration::from_secs(10),
            )
            .expect("child fetch");
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        git(
            temp.path(),
            &["init", "--bare", "-q", remote.to_str().expect("path")],
        );
        let source = temp.path().join("source");
        std::fs::create_dir(&source).expect("source");
        repository(&source);
        git(
            &source,
            &["remote", "add", "origin", remote.to_str().expect("path")],
        );
        git(&source, &["push", "-q", "origin", "HEAD:main"]);
        for revision in ["requested-a", "requested-b"] {
            std::fs::write(source.join(revision), format!("{revision}\n")).expect("revision");
            git(&source, &["add", revision]);
            git(&source, &["commit", "-qm", revision]);
            git(
                &source,
                &["push", "-q", "origin", &format!("HEAD:{revision}")],
            );
            git(&source, &["reset", "--hard", "-q", "HEAD~1"]);
        }
        let sibling = temp.path().join("sibling");
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                sibling.to_str().expect("path"),
            ],
        );

        let executable = std::env::current_exe().expect("test executable");
        let test_name = "git::remote_tracking_authority::tests::two_process_sibling_worktrees_fetch_requested_revisions";
        let spawn = |path: &Path, revision: &str| {
            Command::new(&executable)
                .args(["--exact", test_name, "--nocapture"])
                .env(CHILD, path)
                .env("HOMEB0Y_REMOTE_TRACKING_FETCH_REVISION", revision)
                .spawn()
                .expect("spawn fetch child")
        };
        let mut first = spawn(&source, "requested-a");
        let mut second = spawn(&sibling, "requested-b");
        assert!(first.wait().expect("wait first").success());
        assert!(second.wait().expect("wait second").success());

        for revision in ["requested-a", "requested-b"] {
            assert!(!git_stdout(
                &source,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/origin/{revision}^{{commit}}")
                ],
            )
            .is_empty());
        }
    }

    #[test]
    fn sibling_worktrees_serialize_remote_tracking_operations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        std::fs::create_dir(&source).expect("source");
        repository(&source);
        let sibling = temp.path().join("sibling");
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                sibling.to_str().expect("path"),
            ],
        );

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source_for_thread = source.clone();
        let first = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &source_for_thread,
                "first fetch",
                Instant::now() + Duration::from_secs(5),
                |_| {
                    entered_tx.send(()).expect("entered");
                    release_rx.recv().expect("release");
                    git(
                        &source_for_thread,
                        &["update-ref", "refs/remotes/origin/concurrent", "HEAD"],
                    );
                    Ok(())
                },
            )
        });
        entered_rx.recv().expect("first entered");

        let (second_tx, second_rx) = mpsc::channel();
        let second = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &sibling,
                "second fetch",
                Instant::now() + Duration::from_secs(5),
                |_| {
                    git(
                        &sibling,
                        &["update-ref", "refs/remotes/origin/concurrent", "HEAD"],
                    );
                    second_tx.send(()).expect("second entered");
                    Ok(())
                },
            )
        });
        assert!(second_rx.recv_timeout(Duration::from_millis(150)).is_err());
        release_tx.send(()).expect("release first");
        first
            .join()
            .expect("first thread")
            .expect("first authority");
        second_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second entered after release");
        second
            .join()
            .expect("second thread")
            .expect("second authority");
        git(
            &source,
            &["rev-parse", "--verify", "refs/remotes/origin/concurrent"],
        );
    }

    #[test]
    fn unrelated_repositories_keep_remote_tracking_concurrency() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_repo = temp.path().join("first");
        let second_repo = temp.path().join("second");
        std::fs::create_dir(&first_repo).expect("first repo");
        std::fs::create_dir(&second_repo).expect("second repo");
        repository(&first_repo);
        repository(&second_repo);

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &first_repo,
                "first fetch",
                Instant::now() + Duration::from_secs(5),
                |_| {
                    entered_tx.send(()).expect("first entered");
                    release_rx.recv().expect("release");
                    Ok(())
                },
            )
        });
        entered_rx.recv().expect("first entered");
        with_remote_tracking_authority_until(
            &second_repo,
            "second fetch",
            Instant::now() + Duration::from_secs(5),
            |_| Ok(()),
        )
        .expect("unrelated authority");
        release_tx.send(()).expect("release first");
        first
            .join()
            .expect("first thread")
            .expect("first authority");
    }
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
    Error::git_command_failed(format!(
        "{operation} exhausted its caller deadline waiting for remote-tracking authority at {} (owner: {}; lock: {}): {error}",
        common_dir.display(),
        owner,
        lock_path.display(),
    ))
}
