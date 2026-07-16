use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::error::{Error, Result};
use crate::paths;

const INDEX_LOCK_NAME: &str = ".index.lock";
const INDEX_LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
const INDEX_LOCK_ATTEMPTS: usize = 100;
const INDEX_LOCK_SLEEP: Duration = Duration::from_millis(20);

pub(super) struct LeaseIndexLock {
    path: PathBuf,
}

impl LeaseIndexLock {
    pub(super) fn acquire() -> Result<Self> {
        let dir = paths::rig_leases_dir()?;
        fs::create_dir_all(&dir).map_err(|e| {
            Error::internal_unexpected(format!("Failed to create rig lease directory: {}", e))
        })?;
        let path = dir.join(INDEX_LOCK_NAME);
        for _ in 0..INDEX_LOCK_ATTEMPTS {
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_stale_index_lock(&path)?;
                    thread::sleep(INDEX_LOCK_SLEEP);
                }
                Err(e) => {
                    return Err(Error::internal_unexpected(format!(
                        "Failed to acquire rig lease lock {}: {}",
                        path.display(),
                        e
                    )))
                }
            }
        }
        Err(Error::internal_unexpected(format!(
            "Timed out acquiring rig lease lock {}",
            path.display()
        )))
    }
}

impl Drop for LeaseIndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn remove_stale_index_lock(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(());
    };
    if SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > INDEX_LOCK_STALE_AFTER)
    {
        fs::remove_dir(path).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to remove stale rig lease lock {}: {}",
                path.display(),
                e
            ))
        })?;
    }
    Ok(())
}
