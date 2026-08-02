//! A mkdir-based advisory filesystem lock with mtime-based stale reclaim.
//!
//! `fs::create_dir` is atomic on every platform homeboy targets, so creating a
//! directory is the cheapest cross-process mutex available without a daemon or
//! a filesystem-specific advisory-lock API. Two subsystems had independently
//! grown byte-identical implementations of it -- the invocation lease index in
//! `homeboy-core` and the rig lease index in `homeboy-rig` -- with the same
//! four tuning constants (`.index.lock`, 30s stale, 100 attempts, 20ms sleep)
//! and the same reclaim rule. This is the one copy.
//!
//! # What this is not
//!
//! The reclaim rule here is *mtime only*: a lock directory older than
//! `stale_after` is removed and the acquisition retried. That is correct for a
//! lock held for milliseconds around an index read-modify-write, where an
//! abandoned lock can only mean a crashed process.
//!
//! It is deliberately weaker than the runtime-temp cleanup lock in
//! `homeboy_core::engine::temp`, which is held for minutes, writes an owner
//! descriptor (`owner.json`) with a pid and a Linux process start-time, refuses
//! to reclaim while that identity is still live, and quarantine-renames rather
//! than deletes. Do not "unify" that one into this: it is a genuinely stronger
//! lock, not a duplicate of this one.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use homeboy_error::{Error, Result};

/// Tuning for an [`FsIndexLock`].
#[derive(Debug, Clone, Copy)]
pub struct FsIndexLockConfig {
    /// Directory name created inside the lock's parent directory.
    pub name: &'static str,
    /// A lock directory whose mtime is older than this is considered
    /// abandoned by a crashed process and is removed.
    pub stale_after: Duration,
    /// How many acquisition attempts before giving up.
    pub attempts: usize,
    /// How long to wait between attempts.
    pub sleep: Duration,
    /// Noun used in error messages, e.g. `"rig lease"` produces
    /// "Failed to acquire rig lease lock ...".
    pub subject: &'static str,
}

impl FsIndexLockConfig {
    /// The settings shared by every index lock in the tree: `.index.lock`,
    /// 30s stale, 100 attempts, 20ms apart (so ~2s of contention tolerance).
    pub const fn index(subject: &'static str) -> Self {
        Self {
            name: ".index.lock",
            stale_after: Duration::from_secs(30),
            attempts: 100,
            sleep: Duration::from_millis(20),
            subject,
        }
    }
}

/// An acquired lock. Released on drop.
#[derive(Debug)]
pub struct FsIndexLock {
    path: PathBuf,
    config: FsIndexLockConfig,
}

impl FsIndexLock {
    /// Create `dir` if needed, then block until the lock inside it is held or
    /// `config.attempts` is exhausted.
    pub fn acquire_in(dir: &Path, config: FsIndexLockConfig) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to create {} directory: {}",
                config.subject, e
            ))
        })?;
        let path = dir.join(config.name);
        for _ in 0..config.attempts {
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, config }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_if_stale(&path, config)?;
                    thread::sleep(config.sleep);
                }
                Err(e) => {
                    return Err(Error::internal_unexpected(format!(
                        "Failed to acquire {} lock {}: {}",
                        config.subject,
                        path.display(),
                        e
                    )))
                }
            }
        }
        Err(Error::internal_unexpected(format!(
            "Timed out acquiring {} lock {}",
            config.subject,
            path.display()
        )))
    }

    /// Path of the held lock directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FsIndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

/// Remove a lock directory whose mtime is older than `config.stale_after`.
///
/// Every failure to *read* the lock's metadata is treated as "not stale" so a
/// racing release (the directory vanishing between `create_dir` and
/// `metadata`) is a retry rather than an error.
fn remove_if_stale(path: &Path, config: FsIndexLockConfig) -> Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(());
    };
    if SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > config.stale_after)
    {
        fs::remove_dir(path).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to remove stale {} lock {}: {}",
                config.subject,
                path.display(),
                e
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast(subject: &'static str) -> FsIndexLockConfig {
        FsIndexLockConfig {
            attempts: 3,
            sleep: Duration::from_millis(1),
            ..FsIndexLockConfig::index(subject)
        }
    }

    #[test]
    fn index_config_matches_the_constants_it_replaced() {
        let config = FsIndexLockConfig::index("rig lease");
        assert_eq!(config.name, ".index.lock");
        assert_eq!(config.stale_after, Duration::from_secs(30));
        assert_eq!(config.attempts, 100);
        assert_eq!(config.sleep, Duration::from_millis(20));
    }

    #[test]
    fn acquire_creates_the_lock_directory_and_drop_releases_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("leases");
        let lock_path = {
            let lock = FsIndexLock::acquire_in(&dir, fast("rig lease")).expect("acquire");
            let path = lock.path().to_path_buf();
            assert!(path.is_dir(), "lock directory should exist while held");
            assert_eq!(path.file_name().unwrap(), ".index.lock");
            path
        };
        assert!(!lock_path.exists(), "drop should release the lock");
    }

    #[test]
    fn acquire_creates_missing_parent_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("deeply").join("nested").join("leases");
        let _lock = FsIndexLock::acquire_in(&dir, fast("invocation lease")).expect("acquire");
        assert!(dir.is_dir());
    }

    #[test]
    fn contended_lock_times_out_with_the_subject_in_the_message() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        let _held = FsIndexLock::acquire_in(&dir, fast("rig lease")).expect("first acquire");

        let error = FsIndexLock::acquire_in(&dir, fast("rig lease"))
            .expect_err("second acquire must not succeed while the first is held");
        let message = error.to_string();
        assert!(
            message.contains("Timed out acquiring rig lease lock"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn a_lock_older_than_stale_after_is_reclaimed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        let config = FsIndexLockConfig {
            // Anything already on disk is stale.
            stale_after: Duration::ZERO,
            ..fast("invocation lease")
        };

        // Simulate a crashed holder: the directory exists with no live owner.
        fs::create_dir_all(&dir).expect("create dir");
        let abandoned = dir.join(config.name);
        fs::create_dir(&abandoned).expect("create abandoned lock");

        let lock = FsIndexLock::acquire_in(&dir, config).expect("stale lock should be reclaimed");
        assert_eq!(lock.path(), abandoned);
    }

    #[test]
    fn a_fresh_lock_is_not_reclaimed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        let config = fast("invocation lease");

        fs::create_dir_all(&dir).expect("create dir");
        fs::create_dir(dir.join(config.name)).expect("create fresh lock");

        // stale_after is 30s and the lock was made now, so it must be waited
        // on rather than stolen.
        assert!(FsIndexLock::acquire_in(&dir, config).is_err());
        assert!(dir.join(config.name).is_dir(), "lock must not be stolen");
    }
}
