use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::{Error, Result};

const STORE_ROOT: &str = "cargo-targets";
const LEASE_FILE: &str = ".homeboy-lease";
const OWNER_FILE: &str = ".homeboy-owner";
const LAST_USED_FILE: &str = ".homeboy-last-used-ms";

#[derive(Debug, Clone)]
pub struct CargoTargetCleanupOptions {
    pub root: Option<PathBuf>,
    pub apply: bool,
    pub older_than: Duration,
    pub max_bytes: u64,
    pub limit: usize,
    pub now: SystemTime,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub root: String,
    pub inventory_bytes: u64,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub reclaimed_bytes: u64,
    pub continuation_required: bool,
    pub candidates: Vec<CargoTargetStore>,
    pub retained: Vec<CargoTargetRetained>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetStore {
    pub path: String,
    pub owner: Option<String>,
    pub size_bytes: u64,
    pub last_used_unix_ms: u64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetRetained {
    pub path: String,
    pub owner: Option<String>,
    pub size_bytes: u64,
    pub reason: String,
}

/// Inventory and reclaim Homeboy-owned shared Cargo build stores. A live lease
/// is a heartbeat file owned by the workload using a store; cleanup treats it
/// as authoritative until it expires, then rechecks immediately before remove.
pub fn cleanup_shared_cargo_targets(
    options: CargoTargetCleanupOptions,
) -> Result<CargoTargetCleanupOutput> {
    let root = options
        .root
        .unwrap_or(homeboy_paths::homeboy_data()?.join(STORE_ROOT));
    let mut stores = inventory(&root, options.now, options.older_than)?;
    let inventory_bytes: u64 = stores.iter().map(|store| store.size_bytes).sum();
    stores.sort_by(|left, right| {
        left.last_used_unix_ms
            .cmp(&right.last_used_unix_ms)
            .then_with(|| right.size_bytes.cmp(&left.size_bytes))
            .then_with(|| left.path.cmp(&right.path))
    });

    let mut retained = Vec::new();
    let mut candidates = Vec::new();
    let mut remaining = inventory_bytes;
    for store in stores {
        if store.reasons.iter().any(|reason| reason == "active_lease") {
            retained.push(retained_row(&store, "active lease"));
            continue;
        }
        let stale = store.reasons.iter().any(|reason| reason == "age_expired");
        let pressured = remaining > options.max_bytes;
        if (stale || pressured) && candidates.len() < options.limit {
            remaining = remaining.saturating_sub(store.size_bytes);
            candidates.push(store);
        } else if stale || pressured {
            retained.push(retained_row(&store, "cleanup limit reached"));
        } else {
            retained.push(retained_row(&store, "within age and size budget"));
        }
    }

    let mut applied_count = 0;
    let mut reclaimed_bytes = 0;
    if options.apply {
        for store in &candidates {
            let path = Path::new(&store.path);
            // Re-inventory immediately before removal so a concurrent workload
            // acquiring a lease wins over a cleanup plan created moments ago.
            if lease_is_active(path, options.now, options.older_than)? {
                retained.push(CargoTargetRetained {
                    path: store.path.clone(),
                    owner: store.owner.clone(),
                    size_bytes: store.size_bytes,
                    reason: "lease acquired during cleanup".to_string(),
                });
                continue;
            }
            match fs::remove_dir_all(path) {
                Ok(()) => {
                    applied_count += 1;
                    reclaimed_bytes += store.size_bytes;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Error::internal_io(
                        error.to_string(),
                        Some(format!("remove shared store {}", path.display())),
                    ))
                }
            }
        }
    }
    let continuation_required = retained
        .iter()
        .any(|row| row.reason == "cleanup limit reached")
        || (!options.apply && !candidates.is_empty());
    Ok(CargoTargetCleanupOutput {
        command: "cleanup.shared_cargo_targets",
        mode: if options.apply { "apply" } else { "dry_run" },
        root: root.to_string_lossy().to_string(),
        inventory_bytes,
        candidate_count: candidates.len(),
        applied_count,
        skipped_count: retained.len(),
        reclaimed_bytes,
        continuation_required,
        candidates,
        retained,
    })
}

fn inventory(root: &Path, now: SystemTime, older_than: Duration) -> Result<Vec<CargoTargetStore>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut stores = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read shared store root {}", root.display())),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read shared store entry".to_string()),
            )
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
            })?
            .is_dir()
        {
            continue;
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        let last_used_unix_ms = fs::read_to_string(path.join(LAST_USED_FILE))
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(|| {
                modified
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            });
        let mut reasons = Vec::new();
        let last_used = UNIX_EPOCH + Duration::from_millis(last_used_unix_ms);
        if now.duration_since(last_used).unwrap_or_default() >= older_than {
            reasons.push("age_expired".to_string());
        }
        if lease_is_active(&path, now, older_than)? {
            reasons.push("active_lease".to_string());
        }
        stores.push(CargoTargetStore {
            path: path.to_string_lossy().to_string(),
            owner: fs::read_to_string(path.join(OWNER_FILE))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            size_bytes: path_size(&path)?,
            last_used_unix_ms,
            reasons,
        });
    }
    Ok(stores)
}

fn lease_is_active(path: &Path, now: SystemTime, max_age: Duration) -> Result<bool> {
    let lease = path.join(LEASE_FILE);
    let modified = match fs::metadata(&lease).and_then(|metadata| metadata.modified()) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("stat lease {}", lease.display())),
            ))
        }
    };
    Ok(now.duration_since(modified).unwrap_or_default() < max_age)
}

fn retained_row(store: &CargoTargetStore, reason: &str) -> CargoTargetRetained {
    CargoTargetRetained {
        path: store.path.clone(),
        owner: store.owner.clone(),
        size_bytes: store.size_bytes,
        reason: reason.to_string(),
    }
}

fn path_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })? {
        let path = entry
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some("read store entry".to_string()))
            })?
            .path();
        if path
            .file_name()
            .is_some_and(|name| name == LEASE_FILE || name == OWNER_FILE || name == LAST_USED_FILE)
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
        })?;
        total += if metadata.is_dir() && !metadata.file_type().is_symlink() {
            path_size(&path)?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn options(root: &Path, apply: bool, now: SystemTime) -> CargoTargetCleanupOptions {
        CargoTargetCleanupOptions {
            root: Some(root.to_path_buf()),
            apply,
            older_than: Duration::from_secs(60),
            max_bytes: 10,
            limit: 10,
            now,
        }
    }
    fn store(
        root: &Path,
        name: &str,
        bytes: usize,
        age: Duration,
        now: SystemTime,
        lease: bool,
    ) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("artifact"), vec![b'x'; bytes]).unwrap();
        if lease {
            fs::write(path.join(LEASE_FILE), "job-1").unwrap();
        }
        fs::write(path.join(OWNER_FILE), format!("owner-{name}")).unwrap();
        let timestamp = now
            .checked_sub(age)
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        fs::write(path.join(LAST_USED_FILE), timestamp.to_string()).unwrap();
        path
    }
    #[test]
    fn active_lease_is_protected_under_size_pressure() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let active = store(
            root.path(),
            "active",
            9,
            Duration::from_secs(120),
            now,
            true,
        );
        let stale = store(
            root.path(),
            "stale",
            9,
            Duration::from_secs(120),
            now,
            false,
        );
        let output = cleanup_shared_cargo_targets(options(root.path(), true, now)).unwrap();
        assert!(active.exists());
        assert!(!stale.exists());
        assert_eq!(output.reclaimed_bytes, 9);
        assert!(output
            .retained
            .iter()
            .any(|row| row.reason == "active lease"));
    }
    #[test]
    fn stale_store_is_eligible_without_size_pressure() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let stale = store(root.path(), "stale", 3, Duration::from_secs(61), now, false);
        let mut opts = options(root.path(), true, now);
        opts.max_bytes = 100;
        assert_eq!(cleanup_shared_cargo_targets(opts).unwrap().applied_count, 1);
        assert!(!stale.exists());
    }
    #[test]
    fn size_pressure_evicts_oldest_then_largest_and_reports_continuation() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let oldest = store(
            root.path(),
            "oldest",
            8,
            Duration::from_secs(30),
            now,
            false,
        );
        let newer = store(root.path(), "newer", 8, Duration::from_secs(10), now, false);
        let mut opts = options(root.path(), true, now);
        opts.limit = 1;
        opts.max_bytes = 5;
        let output = cleanup_shared_cargo_targets(opts).unwrap();
        assert!(!oldest.exists());
        assert!(newer.exists());
        assert!(output.continuation_required);
    }
    #[test]
    fn retry_is_idempotent_when_another_cleanup_already_removed_store() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let stale = store(root.path(), "stale", 3, Duration::from_secs(61), now, false);
        let preview = cleanup_shared_cargo_targets(options(root.path(), false, now)).unwrap();
        fs::remove_dir_all(&stale).unwrap();
        let mut opts = options(root.path(), true, now);
        opts.max_bytes = 100;
        assert_eq!(cleanup_shared_cargo_targets(opts).unwrap().applied_count, 0);
        assert_eq!(preview.candidate_count, 1);
    }
}
