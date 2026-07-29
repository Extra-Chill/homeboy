use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use serde_json::json;

use crate::{Error, Result};

const STORE_ROOT: &str = "cargo-targets";
pub const HOMEBOY_CARGO_TARGET_ROOT_ENV: &str = "HOMEBOY_CARGO_TARGET_ROOT";
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
    /// Cooperative deadline checked between stores. A current store's safe
    /// revalidation/removal is never interrupted mid-mutation.
    pub deadline: Option<SystemTime>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub root: String,
    pub storage: CargoTargetStorageStatus,
    pub inventory_bytes: u64,
    pub inventory_count: usize,
    pub inspected_count: usize,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub reclaimed_bytes: u64,
    pub continuation_required: bool,
    pub time_budget_exhausted: bool,
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CargoTargetStorageStatus {
    pub root: String,
    pub filesystem: String,
    pub available_bytes: u64,
    pub available_inodes: u64,
    pub reserve_bytes: u64,
    pub reserve_inodes: u64,
    pub managed_bytes: u64,
    pub protected_bytes: u64,
    pub cleanup_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_discovery_command: Option<String>,
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
    let root = shared_cargo_target_root()?;
    admit_shared_cargo_target(&root)?;
    acquire_shared_cargo_target_in(&root, owner, SystemTime::now())
}

/// Resolve the one shared Cargo store used by producers, cleanup, and reports.
/// The environment is useful for one process; the persisted setting is for the
/// host. The historical data-root location remains the compatibility default.
pub fn shared_cargo_target_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(HOMEBOY_CARGO_TARGET_ROOT_ENV) {
        if !path.trim().is_empty() {
            return Ok(homeboy_paths::expand_tilde_path(path));
        }
    }
    if let Some(path) = crate::defaults::load_config().cargo_target_root {
        if !path.trim().is_empty() {
            return Ok(homeboy_paths::expand_tilde_path(path));
        }
    }
    legacy_shared_cargo_target_root()
}

fn legacy_shared_cargo_target_root() -> Result<PathBuf> {
    Ok(homeboy_paths::homeboy_data()?.join(STORE_ROOT))
}

pub(crate) fn acquire_shared_cargo_target_in(
    root: &Path,
    owner: &str,
    now: SystemTime,
) -> Result<SharedCargoTargetLease> {
    let target_dir = root.join(format!(
        "homeboy-{}",
        content_hash::sha256_hex(owner.as_bytes())
    ));
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
    let root = options.root.unwrap_or(shared_cargo_target_root()?);
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
    let mut time_budget_exhausted = false;
    let mut inspected_count = 0;
    let mut last_inspected = None;

    for store in stores.iter().skip(start) {
        if options
            .deadline
            .is_some_and(|deadline| SystemTime::now() >= deadline)
        {
            has_more = true;
            time_budget_exhausted = true;
            break;
        }
        if inspected_count == options.limit {
            has_more = true;
            break;
        }
        inspected_count += 1;
        last_inspected = Some(store.path.clone());
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
        let legacy = is_legacy_store(Path::new(&store.path));
        let eligible = store.reasons.iter().any(|reason| reason == "age_expired")
            || (!legacy && remaining > options.max_bytes);
        if !eligible {
            *retained_by_reason
                .entry("within age and size budget".to_string())
                .or_default() += 1;
            continue;
        }
        remaining = remaining.saturating_sub(store.size_bytes);
        candidates.push(store.clone());
    }

    let mut applied_count = 0;
    let mut reclaimed_bytes = 0;
    if options.apply {
        for store in &candidates {
            let path = Path::new(&store.path);
            match remove_store_if_unleased(
                path,
                options.now,
                options.older_than,
                options.lease_ttl,
            )? {
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
    let next_cursor = has_more.then_some(last_inspected).flatten();
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
        storage: storage_status(&root, options.now, options.older_than, options.lease_ttl)?,
        inventory_bytes,
        inventory_count: stores.len(),
        inspected_count,
        candidate_count: candidates.len(),
        applied_count,
        skipped_count,
        reclaimed_bytes,
        continuation_required: has_more,
        time_budget_exhausted,
        next_cursor,
        next_command,
        retained_by_reason,
        candidates,
    })
}

/// Read the complete shared-store lifecycle inventory for a bounded reporting
/// projection. This never acquires a deletion lock or mutates a store.
pub fn shared_cargo_target_inventory(
    root: Option<PathBuf>,
    now: SystemTime,
    older_than: Duration,
    lease_ttl: Duration,
) -> Result<Vec<CargoTargetStore>> {
    let root = root.unwrap_or(shared_cargo_target_root()?);
    let mut stores = inventory(&root, now, older_than, lease_ttl)?;
    stores.sort_by(order_stores);
    Ok(stores)
}

/// Capacity and lifecycle facts for status and retained-storage reporting.
pub fn shared_cargo_target_storage_status(
    now: SystemTime,
    older_than: Duration,
    lease_ttl: Duration,
) -> Result<CargoTargetStorageStatus> {
    let root = shared_cargo_target_root()?;
    storage_status(&root, now, older_than, lease_ttl)
}

fn storage_status(
    root: &Path,
    now: SystemTime,
    older_than: Duration,
    lease_ttl: Duration,
) -> Result<CargoTargetStorageStatus> {
    let retention = crate::defaults::load_config().retention;
    let capacity = filesystem_capacity(root)?;
    let stores = inventory(root, now, older_than, lease_ttl)?;
    let managed_bytes = stores.iter().map(|store| store.size_bytes).sum();
    let protected_bytes = stores
        .iter()
        .filter(|store| store.reasons.iter().any(|reason| reason == "active_lease"))
        .map(|store| store.size_bytes)
        .sum();
    let legacy_root = legacy_shared_cargo_target_root()?;
    let moved = legacy_root != root && legacy_root.exists();
    Ok(CargoTargetStorageStatus {
        root: root.display().to_string(),
        filesystem: capacity.filesystem,
        available_bytes: capacity.available_bytes,
        available_inodes: capacity.available_inodes,
        reserve_bytes: retention.shared_store_reserve_bytes,
        reserve_inodes: retention.shared_store_reserve_inodes,
        managed_bytes,
        protected_bytes,
        cleanup_command: "homeboy cleanup --include shared-cargo-targets --apply".to_string(),
        legacy_root: moved.then(|| legacy_root.display().to_string()),
        legacy_discovery_command: moved.then(|| {
            format!(
                "HOMEBOY_CARGO_TARGET_ROOT={} homeboy cleanup --include shared-cargo-targets",
                shell_quote(&legacy_root.display().to_string())
            )
        }),
    })
}

fn admit_shared_cargo_target(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(|error| io_error(error, "create shared Cargo target root"))?;
    let retention = crate::defaults::load_config().retention;
    let capacity = filesystem_capacity(root)?;
    admit_shared_cargo_target_with_capacity(root, &retention, capacity)
}

fn admit_shared_cargo_target_with_capacity(
    root: &Path,
    retention: &crate::defaults::RetentionConfig,
    capacity: FilesystemCapacity,
) -> Result<()> {
    if capacity.available_bytes >= retention.shared_store_reserve_bytes
        && capacity.available_inodes >= retention.shared_store_reserve_inodes
    {
        return Ok(());
    }
    let stores = inventory(
        root,
        SystemTime::now(),
        Duration::from_secs(retention.shared_store_days.saturating_mul(86_400)),
        Duration::from_secs(retention.shared_store_lease_seconds),
    )?;
    let mut reclaimable: Vec<_> = stores
        .iter()
        .filter(|store| !store.reasons.iter().any(|reason| reason == "active_lease"))
        .collect();
    reclaimable.sort_by_key(|store| std::cmp::Reverse(store.size_bytes));
    let protected_bytes: u64 = stores
        .iter()
        .filter(|store| store.reasons.iter().any(|reason| reason == "active_lease"))
        .map(|store| store.size_bytes)
        .sum();
    let mut error = Error::validation_invalid_argument(
        "shared_cargo_target",
        "target filesystem does not satisfy the configured free-space reserve",
        Some(root.display().to_string()),
        Some(vec![
            "homeboy cleanup --include shared-cargo-targets --apply".to_string(),
        ]),
    );
    error.details["filesystem"] = json!(capacity.filesystem);
    error.details["available_bytes"] = json!(capacity.available_bytes);
    error.details["available_inodes"] = json!(capacity.available_inodes);
    error.details["reserve_bytes"] = json!(retention.shared_store_reserve_bytes);
    error.details["reserve_inodes"] = json!(retention.shared_store_reserve_inodes);
    error.details["protected_bytes"] = json!(protected_bytes);
    error.details["largest_reclaimable_stores"] = json!(reclaimable
        .into_iter()
        .take(5)
        .map(|store| json!({ "path": store.path, "size_bytes": store.size_bytes }))
        .collect::<Vec<_>>());
    error.details["cleanup_command"] =
        json!("homeboy cleanup --include shared-cargo-targets --apply");
    Err(error)
}

struct FilesystemCapacity {
    filesystem: String,
    available_bytes: u64,
    available_inodes: u64,
}

#[cfg(unix)]
fn filesystem_capacity(path: &Path) -> Result<FilesystemCapacity> {
    use std::ffi::CString;

    let probe = existing_ancestor(path);
    let path = CString::new(probe.as_os_str().as_encoded_bytes())
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io_error(
            std::io::Error::last_os_error(),
            "probe shared Cargo target filesystem",
        ));
    }
    let stat = unsafe { stat.assume_init() };
    Ok(FilesystemCapacity {
        filesystem: format!("device:{}", stat.f_fsid),
        available_bytes: u64::try_from(
            u128::from(stat.f_bavail).saturating_mul(u128::from(stat.f_frsize)),
        )
        .unwrap_or(u64::MAX),
        available_inodes: u64::from(stat.f_favail),
    })
}

fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|candidate| candidate.exists())
        .unwrap_or(path)
}

#[cfg(not(unix))]
fn filesystem_capacity(path: &Path) -> Result<FilesystemCapacity> {
    Ok(FilesystemCapacity {
        filesystem: path.display().to_string(),
        available_bytes: u64::MAX,
        available_inodes: u64::MAX,
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
    older_than: Duration,
    lease_ttl: Duration,
) -> Result<RemoveOutcome> {
    let legacy_last_used = legacy_last_used(path);
    let legacy = legacy_last_used.is_some();
    let lock = if legacy {
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path.join(LOCK_FILE))
        {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(RemoveOutcome::Protected)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RemoveOutcome::Missing)
            }
            Err(error) => {
                return Err(io_error(
                    error,
                    "create shared Cargo target lock for cleanup",
                ));
            }
        }
    } else {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join(LOCK_FILE))
        {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RemoveOutcome::Missing)
            }
            Err(error) => return Err(io_error(error, "open shared Cargo target lock for cleanup")),
        }
    };
    match lock.try_lock_exclusive() {
        Ok(true) => {}
        Ok(false) | Err(_) => return Ok(RemoveOutcome::Protected),
    }
    if legacy {
        // The lock is created only after recording stale filesystem evidence, as
        // creating it updates the directory timestamp used by legacy stores.
        if !legacy_lifecycle_sidecars_absent(path)
            || !is_expired(legacy_last_used.expect("checked above"), now, older_than)
        {
            return Ok(RemoveOutcome::Protected);
        }
    } else if lease_is_fresh(path, now, lease_ttl)? {
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
        let Some(last_used_unix_ms) = last_used(&path).or_else(|| legacy_last_used(&path)) else {
            stores.push(skipped_store(&path, "missing Homeboy lifecycle metadata"));
            continue;
        };
        let mut reasons = Vec::new();
        if is_expired(last_used_unix_ms, now, older_than) {
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

fn legacy_last_used(path: &Path) -> Option<u64> {
    is_legacy_store(path)
        .then(|| latest_modified(path))??
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|modified| modified.as_millis() as u64)
}

fn is_legacy_store(path: &Path) -> bool {
    is_canonical_store(path) && legacy_sidecars_absent(path)
}

fn is_canonical_store(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("homeboy-"))
        .is_some_and(|hash| {
            matches!(hash.len(), 12 | 64)
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}

fn legacy_sidecars_absent(path: &Path) -> bool {
    [LOCK_FILE, LEASE_FILE, OWNER_FILE, LAST_USED_FILE]
        .iter()
        .all(|name| sidecar_absent(path, name))
}

fn legacy_lifecycle_sidecars_absent(path: &Path) -> bool {
    [LEASE_FILE, OWNER_FILE, LAST_USED_FILE]
        .iter()
        .all(|name| sidecar_absent(path, name))
}

fn sidecar_absent(path: &Path, name: &str) -> bool {
    matches!(fs::symlink_metadata(path.join(name)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

fn latest_modified(path: &Path) -> Option<SystemTime> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in fs::read_dir(path).ok()? {
            let modified = latest_modified(&entry.ok()?.path())?;
            latest = latest.max(modified);
        }
    }
    Some(latest)
}

fn is_expired(last_used_unix_ms: u64, now: SystemTime, older_than: Duration) -> bool {
    now.duration_since(UNIX_EPOCH + Duration::from_millis(last_used_unix_ms))
        .unwrap_or_default()
        >= older_than
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
            deadline: None,
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
    fn legacy_store(root: &Path, now: SystemTime, hash_len: usize) -> PathBuf {
        let path = root.join(format!("homeboy-{}", "a".repeat(hash_len)));
        fs::create_dir(&path).unwrap();
        let artifact = path.join("artifact");
        fs::write(&artifact, b"payload").unwrap();
        let stale = now.checked_sub(Duration::from_secs(61)).unwrap();
        set_modified(&artifact, stale);
        set_modified(&path, stale);
        path
    }

    #[test]
    fn admission_rejects_before_build_and_reports_capacity_and_reclaimable_stores() {
        let root = TempDir::new().unwrap();
        let now = SystemTime::now();
        let reclaimable = store(root.path(), "reclaimable", 12, Duration::ZERO, now);
        let protected = acquire_shared_cargo_target_in(root.path(), "protected", now).unwrap();
        fs::write(protected.target_dir().join("artifact"), vec![b'x'; 7]).unwrap();
        let retention = crate::defaults::RetentionConfig {
            shared_store_reserve_bytes: 100,
            shared_store_reserve_inodes: 10,
            ..crate::defaults::RetentionConfig::default()
        };

        let error = admit_shared_cargo_target_with_capacity(
            root.path(),
            &retention,
            FilesystemCapacity {
                filesystem: "constrained-test-volume".to_string(),
                available_bytes: 99,
                available_inodes: 9,
            },
        )
        .unwrap_err();

        assert_eq!(error.details["filesystem"], "constrained-test-volume");
        assert_eq!(error.details["reserve_bytes"], 100);
        assert_eq!(error.details["reserve_inodes"], 10);
        assert_eq!(error.details["protected_bytes"], 7);
        assert_eq!(
            error.details["largest_reclaimable_stores"][0]["path"],
            reclaimable.display().to_string()
        );
        assert_eq!(
            error.details["cleanup_command"],
            "homeboy cleanup --include shared-cargo-targets --apply"
        );
        assert!(protected.target_dir().exists());
    }
    fn set_modified(path: &Path, modified: SystemTime) {
        File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
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
    fn stale_canonical_legacy_store_is_a_dry_run_candidate() {
        let root = TempDir::new().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let historical = legacy_store(root.path(), now, 12);
        let current = legacy_store(root.path(), now, 64);
        let output = cleanup_shared_cargo_targets(options(root.path(), false, now)).unwrap();
        assert_eq!(output.candidate_count, 2);
        assert!(output
            .candidates
            .iter()
            .any(|candidate| candidate.path == historical.to_string_lossy()));
        assert!(output
            .candidates
            .iter()
            .any(|candidate| candidate.path == current.to_string_lossy()));
        assert_eq!(output.applied_count, 0);
        assert!(historical.exists());
        assert!(current.exists());
    }

    #[test]
    fn apply_reclaims_stale_canonical_legacy_store() {
        let root = TempDir::new().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let legacy = legacy_store(root.path(), now, 12);
        let output = cleanup_shared_cargo_targets(options(root.path(), true, now)).unwrap();
        assert_eq!(output.candidate_count, 1);
        assert_eq!(output.applied_count, 1);
        assert!(!legacy.exists());
    }

    #[test]
    fn malformed_or_partially_managed_legacy_store_is_protected() {
        let root = TempDir::new().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let malformed = legacy_store(root.path(), now, 13);
        let partially_managed = legacy_store(root.path(), now, 12);
        fs::write(partially_managed.join(OWNER_FILE), "owner").unwrap();

        let output = cleanup_shared_cargo_targets(options(root.path(), true, now)).unwrap();

        assert_eq!(output.candidate_count, 0);
        assert_eq!(
            output.retained_by_reason["missing Homeboy lifecycle metadata"],
            2
        );
        assert!(malformed.exists());
        assert!(partially_managed.exists());
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
