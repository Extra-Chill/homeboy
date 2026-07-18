use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

const STORE_ROOT: &str = "cargo-targets";
const LOCK_FILE: &str = ".homeboy-lock";
const LEASE_FILE: &str = ".homeboy-lease";
const OWNER_FILE: &str = ".homeboy-owner";
const LAST_USED_FILE: &str = ".homeboy-last-used-ms";

#[derive(Debug, Clone)]
pub struct CargoTargetCleanupOptions {
    pub root: Option<PathBuf>,
    pub apply: bool,
    pub older_than: Duration,
    pub lease_ttl: Duration,
    pub max_bytes: u64,
    pub limit: usize,
    pub cursor: Option<String>,
    pub now: SystemTime,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub root: String,
    pub inventory_bytes: u64,
    pub inventory_count: usize,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub reclaimed_bytes: u64,
    pub continuation_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
    pub retained_by_reason: BTreeMap<String, usize>,
    pub candidates: Vec<CargoTargetStore>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetStore {
    pub path: String,
    pub owner: Option<String>,
    pub size_bytes: u64,
    pub last_used_unix_ms: u64,
    pub reasons: Vec<String>,
}

/// A live, shared Cargo target-store lease. The shared advisory lock makes the
/// producer and cleaner mutually exclusive; sidecars preserve ownership and
/// liveness evidence for inventory after the producer exits.
pub struct SharedCargoTargetLease {
    target_dir: PathBuf,
    _lock: File,
}

impl SharedCargoTargetLease {
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    pub fn touch(&self) -> Result<()> {
        write_lifecycle(&self.target_dir, None, SystemTime::now())
    }
}

impl Drop for SharedCargoTargetLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.target_dir.join(LEASE_FILE));
        let _ = write_last_used(&self.target_dir, SystemTime::now());
    }
}

pub fn acquire_shared_cargo_target(owner: &str) -> Result<SharedCargoTargetLease> {
    let root = homeboy_paths::homeboy_data()?.join(STORE_ROOT);
    acquire_shared_cargo_target_in(&root, owner, SystemTime::now())
}

fn acquire_shared_cargo_target_in(
    root: &Path,
    owner: &str,
    now: SystemTime,
) -> Result<SharedCargoTargetLease> {
    let mut digest = Sha256::new();
    digest.update(owner.as_bytes());
    let target_dir = root.join(format!("homeboy-{:x}", digest.finalize()));
    fs::create_dir_all(&target_dir)
        .map_err(|error| io_error(error, "create shared Cargo target"))?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(target_dir.join(LOCK_FILE))
        .map_err(|error| io_error(error, "open shared Cargo target lock"))?;
    lock.lock_shared()
        .map_err(|error| io_error(error, "lock shared Cargo target"))?;
    write_lifecycle(&target_dir, Some(owner), now)?;
    Ok(SharedCargoTargetLease {
        target_dir,
        _lock: lock,
    })
}

pub fn cleanup_shared_cargo_targets(
    options: CargoTargetCleanupOptions,
) -> Result<CargoTargetCleanupOutput> {
    let root = options
        .root
        .unwrap_or(homeboy_paths::homeboy_data()?.join(STORE_ROOT));
    let mut stores = inventory(&root, options.now, options.older_than, options.lease_ttl)?;
    let inventory_bytes: u64 = stores.iter().map(|store| store.size_bytes).sum();
    stores.sort_by(order_stores);
    let start = options
        .cursor
        .as_ref()
        .and_then(|cursor| {
            stores
                .iter()
                .position(|store| &store.path == cursor)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let mut retained_by_reason = BTreeMap::new();
    let mut candidates = Vec::new();
    let mut remaining = inventory_bytes;
    let mut has_more = false;

    for store in stores.iter().skip(start) {
        if let Some(reason) = store
            .reasons
            .iter()
            .find(|reason| reason.starts_with("skipped:"))
        {
            *retained_by_reason
                .entry(reason.trim_start_matches("skipped:").to_string())
                .or_default() += 1;
            continue;
        }
        if store.reasons.iter().any(|reason| reason == "active_lease") {
            *retained_by_reason
                .entry("active lease".to_string())
                .or_default() += 1;
            continue;
        }
        let eligible = store.reasons.iter().any(|reason| reason == "age_expired")
            || remaining > options.max_bytes;
        if !eligible {
            *retained_by_reason
                .entry("within age and size budget".to_string())
                .or_default() += 1;
            continue;
        }
        if candidates.len() == options.limit {
            has_more = true;
            break;
        }
        remaining = remaining.saturating_sub(store.size_bytes);
        candidates.push(store.clone());
    }

    let mut applied_count = 0;
    let mut reclaimed_bytes = 0;
    if options.apply {
        for store in &candidates {
            let path = Path::new(&store.path);
            match remove_store_if_unleased(path, options.now, options.lease_ttl)? {
                RemoveOutcome::Removed => {
                    applied_count += 1;
                    reclaimed_bytes += store.size_bytes;
                }
                RemoveOutcome::Protected => {
                    *retained_by_reason
                        .entry("lease acquired during cleanup".to_string())
                        .or_default() += 1
                }
                RemoveOutcome::Missing => {}
            }
        }
    }
    let next_cursor = has_more
        .then(|| candidates.last().map(|store| store.path.clone()))
        .flatten();
    let next_command = next_cursor.as_ref().map(|cursor| {
        let apply = if options.apply { " --apply" } else { "" };
        format!(
            "homeboy cleanup --include shared-cargo-targets{apply} --cursor {}",
            shell_quote(cursor)
        )
    });
    let skipped_count = retained_by_reason.values().sum();
    Ok(CargoTargetCleanupOutput {
        command: "cleanup.shared_cargo_targets",
        mode: if options.apply { "apply" } else { "dry_run" },
        root: root.to_string_lossy().to_string(),
        inventory_bytes,
        inventory_count: stores.len(),
        candidate_count: candidates.len(),
        applied_count,
        skipped_count,
        reclaimed_bytes,
        continuation_required: has_more,
        next_cursor,
        next_command,
        retained_by_reason,
        candidates,
    })
}

#[derive(PartialEq, Eq)]
enum RemoveOutcome {
    Removed,
    Protected,
    Missing,
}

fn remove_store_if_unleased(
    path: &Path,
    now: SystemTime,
    lease_ttl: Duration,
) -> Result<RemoveOutcome> {
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join(LOCK_FILE))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RemoveOutcome::Missing)
        }
        Err(error) => return Err(io_error(error, "open shared Cargo target lock for cleanup")),
    };
    match lock.try_lock_exclusive() {
        Ok(true) => {}
        Ok(false) | Err(_) => return Ok(RemoveOutcome::Protected),
    }
    if lease_is_fresh(path, now, lease_ttl)? {
        return Ok(RemoveOutcome::Protected);
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(RemoveOutcome::Removed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RemoveOutcome::Missing),
        Err(error) => Err(io_error(error, "remove shared Cargo target")),
    }
}

fn inventory(
    root: &Path,
    now: SystemTime,
    older_than: Duration,
    lease_ttl: Duration,
) -> Result<Vec<CargoTargetStore>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut stores = Vec::new();
    for entry in
        fs::read_dir(root).map_err(|error| io_error(error, "read shared Cargo target root"))?
    {
        let path = entry
            .map_err(|error| io_error(error, "read shared Cargo target entry"))?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error(error, "stat shared Cargo target entry"))?;
        if metadata.file_type().is_symlink() {
            stores.push(skipped_store(&path, "direct-child symlink"));
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let Some(last_used_unix_ms) = last_used(&path) else {
            stores.push(skipped_store(&path, "missing Homeboy lifecycle metadata"));
            continue;
        };
        let mut reasons = Vec::new();
        if now
            .duration_since(UNIX_EPOCH + Duration::from_millis(last_used_unix_ms))
            .unwrap_or_default()
            >= older_than
        {
            reasons.push("age_expired".to_string());
        }
        if store_is_active(&path, now, lease_ttl)? {
            reasons.push("active_lease".to_string());
        }
        stores.push(CargoTargetStore {
            path: path.to_string_lossy().to_string(),
            owner: read_owner(&path),
            size_bytes: path_size(&path)?,
            last_used_unix_ms,
            reasons,
        });
    }
    Ok(stores)
}

fn store_is_active(path: &Path, now: SystemTime, lease_ttl: Duration) -> Result<bool> {
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join(LOCK_FILE))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return lease_is_fresh(path, now, lease_ttl)
        }
        Err(error) => {
            return Err(io_error(
                error,
                "open shared Cargo target lock for inventory",
            ))
        }
    };
    match lock.try_lock_exclusive() {
        Ok(true) => {
            FileExt::unlock(&lock)
                .map_err(|error| io_error(error, "unlock shared Cargo target inventory lock"))?;
            lease_is_fresh(path, now, lease_ttl)
        }
        Ok(false) | Err(_) => Ok(true),
    }
}

fn lease_is_fresh(path: &Path, now: SystemTime, ttl: Duration) -> Result<bool> {
    let modified =
        match fs::metadata(path.join(LEASE_FILE)).and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_error(error, "stat shared Cargo target lease")),
        };
    Ok(now.duration_since(modified).unwrap_or_default() < ttl)
}

fn write_lifecycle(path: &Path, owner: Option<&str>, now: SystemTime) -> Result<()> {
    if let Some(owner) = owner {
        fs::write(path.join(OWNER_FILE), owner)
            .map_err(|error| io_error(error, "write shared Cargo target owner"))?;
    }
    fs::write(
        path.join(LEASE_FILE),
        now.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string(),
    )
    .map_err(|error| io_error(error, "write shared Cargo target lease"))?;
    write_last_used(path, now)
}

fn write_last_used(path: &Path, now: SystemTime) -> Result<()> {
    fs::write(
        path.join(LAST_USED_FILE),
        now.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string(),
    )
    .map_err(|error| io_error(error, "write shared Cargo target last-used"))
}

fn last_used(path: &Path) -> Option<u64> {
    fs::read_to_string(path.join(LAST_USED_FILE))
        .ok()
        .and_then(|value| value.trim().parse().ok())
}

fn read_owner(path: &Path) -> Option<String> {
    fs::read_to_string(path.join(OWNER_FILE))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
fn skipped_store(path: &Path, reason: &str) -> CargoTargetStore {
    CargoTargetStore {
        path: path.to_string_lossy().to_string(),
        owner: None,
        size_bytes: 0,
        last_used_unix_ms: 0,
        reasons: vec![format!("skipped:{reason}")],
    }
}
fn order_stores(left: &CargoTargetStore, right: &CargoTargetStore) -> std::cmp::Ordering {
    left.last_used_unix_ms
        .cmp(&right.last_used_unix_ms)
        .then_with(|| right.size_bytes.cmp(&left.size_bytes))
        .then_with(|| left.path.cmp(&right.path))
}
fn path_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path).map_err(|error| io_error(error, "read shared Cargo target"))? {
        let path = entry
            .map_err(|error| io_error(error, "read shared Cargo target entry"))?
            .path();
        if path.file_name().is_some_and(|name| {
            name == LOCK_FILE || name == LEASE_FILE || name == OWNER_FILE || name == LAST_USED_FILE
        }) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error(error, "stat shared Cargo target entry"))?;
        total += if metadata.is_dir() && !metadata.file_type().is_symlink() {
            path_size(&path)?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}
fn io_error(error: std::io::Error, operation: &str) -> Error {
    Error::internal_io(error.to_string(), Some(operation.to_string()))
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
            lease_ttl: Duration::from_secs(3600),
            max_bytes: 10,
            limit: 10,
            cursor: None,
            now,
        }
    }
    fn store(root: &Path, owner: &str, bytes: usize, age: Duration, now: SystemTime) -> PathBuf {
        let lease = acquire_shared_cargo_target_in(root, owner, now).unwrap();
        let path = lease.target_dir().to_path_buf();
        fs::write(path.join("artifact"), vec![b'x'; bytes]).unwrap();
        drop(lease);
        write_last_used(&path, now.checked_sub(age).unwrap()).unwrap();
        path
    }
    #[test]
    fn managed_store_lifecycle_records_owner_lease_and_last_used() {
        let root = TempDir::new().unwrap();
        let lease =
            acquire_shared_cargo_target_in(root.path(), "controller:abc", SystemTime::now())
                .unwrap();
        assert!(lease.target_dir().join(LOCK_FILE).exists());
        assert_eq!(
            read_owner(lease.target_dir()).as_deref(),
            Some("controller:abc")
        );
        assert!(lease.target_dir().join(LEASE_FILE).exists());
        drop(lease);
        assert!(!lease_path(root.path()).join(LEASE_FILE).exists());
    }
    fn lease_path(root: &Path) -> PathBuf {
        fs::read_dir(root).unwrap().next().unwrap().unwrap().path()
    }
    #[test]
    fn active_producer_is_protected_even_with_zero_day_retention() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let lease = acquire_shared_cargo_target_in(root.path(), "active", now).unwrap();
        fs::write(lease.target_dir().join("artifact"), b"payload").unwrap();
        let mut opts = options(root.path(), true, now);
        opts.older_than = Duration::ZERO;
        opts.max_bytes = 0;
        let output = cleanup_shared_cargo_targets(opts).unwrap();
        assert_eq!(output.applied_count, 0);
        assert_eq!(output.retained_by_reason["active lease"], 1);
        drop(lease);
    }
    #[test]
    fn stale_store_is_reclaimed_and_retry_is_idempotent() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let stale = store(root.path(), "stale", 3, Duration::from_secs(61), now);
        let mut opts = options(root.path(), true, now);
        opts.max_bytes = 100;
        assert_eq!(
            cleanup_shared_cargo_targets(opts.clone())
                .unwrap()
                .applied_count,
            1
        );
        assert!(!stale.exists());
        assert_eq!(cleanup_shared_cargo_targets(opts).unwrap().applied_count, 0);
    }
    #[test]
    fn lease_acquired_between_plan_and_apply_is_protected() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let stale = store(root.path(), "race", 3, Duration::from_secs(61), now);
        let _lease = acquire_shared_cargo_target_in(root.path(), "race", now).unwrap();
        let mut opts = options(root.path(), true, now);
        opts.max_bytes = 100;
        let output = cleanup_shared_cargo_targets(opts).unwrap();
        assert!(stale.exists());
        assert_eq!(output.applied_count, 0);
    }
    #[test]
    fn large_inventory_returns_bounded_page_with_cursor() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        for index in 0..20 {
            store(
                root.path(),
                &format!("store-{index}"),
                2,
                Duration::from_secs(61 + index),
                now,
            );
        }
        let mut opts = options(root.path(), false, now);
        opts.limit = 3;
        opts.max_bytes = 100;
        let output = cleanup_shared_cargo_targets(opts).unwrap();
        assert_eq!(output.candidates.len(), 3);
        assert!(output.continuation_required);
        assert!(output.next_cursor.is_some());
        assert!(output.next_command.as_deref().unwrap().contains("--cursor"));
    }

    #[test]
    fn legacy_and_symlink_entries_are_reported_without_inspection_or_removal() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("legacy")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("legacy", root.path().join("linked")).unwrap();
        let output =
            cleanup_shared_cargo_targets(options(root.path(), true, SystemTime::now())).unwrap();
        assert_eq!(output.applied_count, 0);
        assert_eq!(
            output.retained_by_reason["missing Homeboy lifecycle metadata"],
            1
        );
        #[cfg(unix)]
        assert_eq!(output.retained_by_reason["direct-child symlink"], 1);
    }

    #[test]
    fn apply_continuation_quotes_unsafe_cursor() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        for owner in ["one space", "two'quote"] {
            store(root.path(), owner, 2, Duration::from_secs(61), now);
        }
        let mut options = options(root.path(), true, now);
        options.limit = 1;
        options.max_bytes = 100;
        let output = cleanup_shared_cargo_targets(options).unwrap();
        let command = output.next_command.unwrap();
        assert!(command.contains("--apply"));
        assert!(command.contains("--cursor '"));
    }
}
