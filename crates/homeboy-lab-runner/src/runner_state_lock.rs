//! Cross-process serialization of one runner's controller-local state.
//!
//! Every lifecycle mutation of a runner's controller state (session
//! publication, admission reservations, replacement journals) runs under one
//! runner-scoped lock file. The lock and the files it guards are resolved from
//! one config root, so a repoint of the environment between taking the lock
//! and writing cannot split them across installations (#7505).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// The controller-local state directory of one runner below a config root.
pub(crate) fn runner_state_dir_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    paths::runner_sessions_dir_in_root(config_root).join(runner_id)
}

fn runner_state_lock_path_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    runner_state_dir_in_root(config_root, runner_id).join("runner-state.lock")
}

/// Cross-process lifecycle serialization for one runner. Every operation that
/// mutates the runner's controller state takes this same lock.
pub(crate) fn with_runner_registry_lock<T>(
    runner_id: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_rooted_runner_registry_lock(runner_id, |_| operation())
}

/// [`with_runner_registry_lock`], handing the operation the config root the
/// lock was taken against so its reads and writes name the same installation.
pub(crate) fn with_rooted_runner_registry_lock<T>(
    runner_id: &str,
    operation: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let config_root = paths::homeboy()?;
    with_lock(
        runner_state_lock_path_in_root(&config_root, runner_id),
        runner_id,
        "runner_state_mutation",
        || operation(&config_root),
    )
}

/// Acquire an exclusive `flock` on `lock_path` for the duration of
/// `operation`. A second descriptor, even in the same process, waits.
pub(crate) fn with_lock<T>(
    lock_path: PathBuf,
    runner_id: &str,
    phase: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_lock_until(lock_path, runner_id, phase, LOCK_TIMEOUT, operation)
}

fn with_lock_until<T>(
    lock_path: PathBuf,
    runner_id: &str,
    phase: &str,
    lock_timeout: Duration,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    const LOCK_RETRY: Duration = Duration::from_millis(25);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create runner state lock directory".to_string()),
            )
        })?;
    }
    let mut lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open runner state lock".to_string()),
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let deadline = Instant::now() + lock_timeout;
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("lock runner state".to_string()),
                ));
            }
            if Instant::now() >= deadline {
                return Err(lock_timeout_error(&lock_path, runner_id, lock_timeout));
            }
            std::thread::sleep(LOCK_RETRY);
        }
    }
    #[cfg(unix)]
    {
        let _ = lock.seek(SeekFrom::Start(0));
        let _ = lock.set_len(0);
        let _ = lock.write_all(
            serde_json::to_string(&serde_json::json!({
                "owner_pid": std::process::id(),
                "phase": phase,
                "acquired_at": Utc::now().to_rfc3339(),
                "runner_id": runner_id,
            }))
            .unwrap_or_default()
            .as_bytes(),
        );
        let _ = lock.sync_data();
    }
    let result = operation();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    }
    result
}

#[cfg(unix)]
fn lock_timeout_error(lock_path: &Path, runner_id: &str, lock_timeout: Duration) -> Error {
    let mut lock_owner = serde_json::Value::Null;
    if let Ok(mut owner_file) = OpenOptions::new().read(true).open(lock_path) {
        let mut owner = String::new();
        if owner_file.read_to_string(&mut owner).is_ok() {
            lock_owner = serde_json::from_str(&owner).unwrap_or(serde_json::Value::Null);
        }
    }
    let owner_age_ms = lock_owner
        .get("acquired_at")
        .and_then(serde_json::Value::as_str)
        .and_then(|acquired| chrono::DateTime::parse_from_rfc3339(acquired).ok())
        .map(|acquired| {
            (Utc::now() - acquired.with_timezone(&Utc))
                .num_milliseconds()
                .max(0)
        });
    let phase = lock_owner
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let message = format!(
        "timed out after {}ms waiting for runner state lock held by pid {} (phase {}, held {}ms)",
        lock_timeout.as_millis(),
        lock_owner
            .get("owner_pid")
            .map(ToString::to_string)
            .unwrap_or_else(|| "unknown".to_string()),
        phase,
        owner_age_ms
            .map(|age| age.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    );
    let mut error = Error::new(
        homeboy_core::error::ErrorCode::InternalIoError,
        message.clone(),
        serde_json::json!({
            "error": message,
            "context": "lock runner state",
            "kind": "runner_state_lock_timeout",
            "runner_id": runner_id,
            "timeout_ms": lock_timeout.as_millis(),
            "owner_pid": lock_owner.get("owner_pid"),
            "phase": phase,
            "owner_acquired_at": lock_owner.get("acquired_at"),
            "owner_age_ms": owner_age_ms,
            "deadline_ms": lock_timeout.as_millis(),
            "legal_action": format!("homeboy runner status {runner_id} --full"),
        }),
    );
    error.retryable = Some(true);
    error
}

/// Atomically replace `path` with `value`: write a synced sibling, rename it
/// over the target, and sync the directory so a crash leaves either the old
/// or the new document, never a torn one.
pub(crate) fn write_durable_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::internal_unexpected("journal has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("journal"),
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize durable journal".to_string()),
        )
    })?;
    let mut file = File::create(&temporary).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", temporary.display())),
        )
    })?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("write {}", temporary.display())),
            )
        })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("rename {}", path.display())),
        )
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })
}

/// Read a JSON document this module's callers wrote with
/// [`write_durable_json`]. `None` means the document does not exist.
pub(crate) fn read_durable_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("read {}", path.display())),
            ))
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))
}

/// Remove a state file if it exists.
pub(crate) fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!("remove {}", path.display())),
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn durable_json_write_overwrites_existing_file() {
        let directory = tempfile::tempdir().expect("create journal directory");
        let path = directory.path().join("journal.json");
        std::fs::write(&path, "stale payload that is longer than the replacement")
            .expect("write stale journal");

        write_durable_json(&path, &json!({ "state": "current" })).expect("overwrite journal");

        assert_eq!(
            std::fs::read_to_string(path).expect("read replacement journal"),
            "{\n  \"state\": \"current\"\n}"
        );
    }

    #[test]
    fn registry_lock_publishes_durable_owner_and_phase_metadata() {
        crate::test_support::with_isolated_home(|_| {
            with_runner_registry_lock("runner-a", || {
                let lock_path = runner_state_lock_path_in_root(&paths::homeboy()?, "runner-a");
                let owner: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(lock_path).expect("read held lock metadata"),
                )
                .expect("lock metadata is JSON");
                assert_eq!(owner["owner_pid"], std::process::id());
                assert_eq!(owner["phase"], "runner_state_mutation");
                assert_eq!(owner["runner_id"], "runner-a");
                assert!(owner["acquired_at"].as_str().is_some());
                Ok(())
            })
            .expect("lock operation succeeds");
        });
    }

    #[test]
    fn lock_timeout_reports_the_live_holder_phase_age_deadline_and_action() {
        crate::test_support::with_isolated_home(|_| {
            let lock_path =
                runner_state_lock_path_in_root(&paths::homeboy().expect("root"), "runner-a");
            with_lock_until(
                lock_path.clone(),
                "runner-a",
                "runner_state_mutation",
                Duration::from_millis(75),
                || {
                    let error = with_lock_until(
                        lock_path,
                        "runner-a",
                        "runner_state_mutation",
                        Duration::from_millis(75),
                        || Ok(()),
                    )
                    .expect_err("second descriptor waits for the held flock");

                    assert!(error.message.contains("timed out after 75ms"));
                    assert!(error
                        .message
                        .contains(&format!("held by pid {}", std::process::id())));
                    assert!(error.message.contains("phase runner_state_mutation, held "));
                    assert_eq!(error.details["error"], error.message);
                    assert_eq!(error.details["context"], "lock runner state");
                    assert_eq!(error.details["kind"], "runner_state_lock_timeout");
                    assert_eq!(error.retryable, Some(true));
                    assert_eq!(error.details["owner_pid"], std::process::id());
                    assert_eq!(error.details["phase"], "runner_state_mutation");
                    assert!(error.details["owner_age_ms"].as_i64().is_some());
                    assert_eq!(error.details["deadline_ms"], 75);
                    assert_eq!(
                        error.details["legal_action"],
                        "homeboy runner status runner-a --full"
                    );
                    Ok(())
                },
            )
            .expect("outer lock operation succeeds");
        });
    }
}
