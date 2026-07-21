//! Runtime-temp implementation behind the stable `engine::temp` facade.

use crate::error::{Error, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

mod contract {
    use super::*;

    pub(super) const RUNTIME_TEMP_PIN_FILE: &str = ".homeboy-runtime-temp-pin-v1";
    pub(super) const RUNTIME_TEMP_PIN_SCHEMA_LINE: &str = "schema=homeboy-runtime-temp-pin-v1";
    pub(super) const RUN_OWNER_FILE: &str = ".homeboy-run-owner-v1.json";
    pub(super) const RUN_OWNER_SCHEMA: &str = "homeboy/runtime-run-owner/v1";
    pub(super) const CLEANUP_LOCK_DIR: &str = ".cleanup.lock";
    pub(super) const CLEANUP_LOCK_STALE_AFTER: Duration = Duration::from_secs(300);
    pub(super) const CLEANUP_LOCK_ATTEMPTS: usize = 100;
    pub(super) const CLEANUP_LOCK_SLEEP: Duration = Duration::from_millis(20);
    pub(super) const CLEANUP_LOCK_OWNER_FILE: &str = "owner.json";
    pub(super) const CORRUPT_OWNER_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
}
use contract::*;

/// A process-owned pin which prevents runtime-temp cleanup from removing its directory.
#[derive(Debug)]
pub(crate) struct RuntimeTempPin {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeRunOwner {
    schema: String,
    owner_id: String,
    owner_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linux_starttime_ticks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    invocation_ids: Vec<String>,
    state: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeTempCleanupOptions<'a> {
    pub apply: bool,
    pub older_than_days: u64,
    pub prefix: Option<&'a str>,
    pub limit: usize,
    pub run_max_bytes: u64,
    pub run_max_count: usize,
    pub cursor: Option<&'a str>,
}

#[derive(Debug)]
struct ManagedRunInspection {
    path: PathBuf,
    name: String,
    owner: RuntimeRunOwner,
    age_seconds: u64,
    size_bytes: u64,
    allocated_bytes: u64,
    protection_reason: Option<String>,
    metadata_warning: Option<String>,
}

struct RuntimeTempCleanupLock {
    path: PathBuf,
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RuntimeTempCleanupLockOwner {
    token: String,
    pid: u32,
    linux_starttime_ticks: Option<u64>,
    heartbeat_unix_ms: u64,
}

impl Drop for RuntimeTempCleanupLock {
    fn drop(&mut self) {
        let owner_path = self.path.join(CLEANUP_LOCK_OWNER_FILE);
        let owned = fs::read_to_string(&owner_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<RuntimeTempCleanupLockOwner>(&raw).ok())
            .is_some_and(|owner| owner.token == self.token);
        if owned {
            let _ = fs::remove_file(owner_path);
            let _ = fs::remove_dir(&self.path);
        }
    }
}

impl RuntimeTempCleanupLock {
    fn heartbeat(&self) -> Result<()> {
        let owner_path = self.path.join(CLEANUP_LOCK_OWNER_FILE);
        let mut owner: RuntimeTempCleanupLockOwner =
            serde_json::from_slice(&fs::read(&owner_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read runtime cleanup lock".to_string()),
                )
            })?)
            .map_err(|error| {
                Error::internal_json(error.to_string(), Some("runtime cleanup lock".to_string()))
            })?;
        if owner.token != self.token {
            return Err(Error::internal_unexpected(
                "runtime cleanup lock ownership changed during cleanup",
            ));
        }
        owner.heartbeat_unix_ms = unix_time_ms();
        write_cleanup_lock_owner(&owner_path, &owner)
    }
}

impl Drop for RuntimeTempPin {
    fn drop(&mut self) {
        // Pins only control cleanup eligibility. Their directories remain subject to
        // the existing retention policy after the last owner releases the pin.
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn pin_runtime_temp_dir(dir: &Path) -> Result<RuntimeTempPin> {
    let metadata = fs::symlink_metadata(dir).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("inspect runtime temp directory {}", dir.display())),
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::validation_invalid_argument(
            "runtimeTempDir",
            format!(
                "Runtime temp pin requires a real directory: {}",
                dir.display()
            ),
            None,
            None,
        ));
    }

    let path = dir.join(RUNTIME_TEMP_PIN_FILE);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create runtime temp pin {}", path.display())),
            )
        })?;
    let record = format!(
        "{RUNTIME_TEMP_PIN_SCHEMA_LINE}\nowner_pid={}\n",
        std::process::id()
    );
    if let Err(error) = std::io::Write::write_all(&mut file, record.as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(Error::internal_io(
            error.to_string(),
            Some(format!("write runtime temp pin {}", path.display())),
        ));
    }

    Ok(RuntimeTempPin { path })
}

pub(crate) fn managed_run_temp_dir(prefix: &str) -> Result<(PathBuf, RuntimeTempPin)> {
    let path = runtime_temp_dir(prefix)?;
    let owner = RuntimeRunOwner {
        schema: RUN_OWNER_SCHEMA.to_string(),
        owner_id: uuid::Uuid::new_v4().to_string(),
        owner_pid: std::process::id(),
        linux_starttime_ticks: crate::process::linux_process_starttime_ticks(std::process::id())
            .ok()
            .flatten(),
        run_id: None,
        invocation_ids: Vec::new(),
        state: "active".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        reason: None,
    };
    if let Err(error) = write_run_owner(&path, &owner) {
        let _ = fs::remove_dir_all(&path);
        return Err(error);
    }
    match pin_runtime_temp_dir(&path) {
        Ok(pin) => Ok((path, pin)),
        Err(error) => {
            let _ = fs::remove_dir_all(&path);
            Err(error)
        }
    }
}

pub(crate) fn mark_run_dir_succeeded(path: &Path) {
    update_run_owner(path, "succeeded", "successful teardown");
}

pub(crate) fn retain_failed_run_dir(path: &Path) {
    update_run_owner(path, "failed", "owner exited before successful teardown");
}

pub(crate) fn bind_run_dir_owner(
    path: &Path,
    run_id: Option<&str>,
    invocation_id: Option<&str>,
) -> Result<()> {
    let mut owner = read_run_owner(path)?;
    if let Some(run_id) = run_id {
        owner.run_id = Some(run_id.to_string());
    }
    if let Some(invocation_id) = invocation_id {
        if !owner.invocation_ids.iter().any(|id| id == invocation_id) {
            owner.invocation_ids.push(invocation_id.to_string());
        }
    }
    write_run_owner(path, &owner)
}

mod owner_support {
    use super::*;

    pub(super) fn update_run_owner(path: &Path, state: &str, reason: &str) {
        if !path.exists() {
            return;
        }
        let Ok(mut owner) = read_run_owner(path) else {
            return;
        };
        if owner.state == "active" {
            owner.state = state.to_string();
            owner.completed_at = Some(chrono::Utc::now().to_rfc3339());
            owner.reason = Some(reason.to_string());
            let _ = write_run_owner(path, &owner);
        }
    }

    pub(super) fn read_run_owner(path: &Path) -> Result<RuntimeRunOwner> {
        let owner_path = path.join(RUN_OWNER_FILE);
        let raw = fs::read_to_string(&owner_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read runtime run owner {}", owner_path.display())),
            )
        })?;
        let owner: RuntimeRunOwner = serde_json::from_str(&raw).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse runtime run owner {}", owner_path.display())),
            )
        })?;
        if owner.schema != RUN_OWNER_SCHEMA || owner.owner_id.is_empty() || owner.owner_pid == 0 {
            return Err(Error::validation_invalid_argument(
                "runtime_run_owner",
                "runtime run owner record has an unrecognized contract",
                Some(owner_path.display().to_string()),
                None,
            ));
        }
        Ok(owner)
    }

    pub(super) fn write_run_owner(path: &Path, owner: &RuntimeRunOwner) -> Result<()> {
        let owner_path = path.join(RUN_OWNER_FILE);
        let temporary = path.join(format!("{RUN_OWNER_FILE}.tmp-{}", std::process::id()));
        let raw = serde_json::to_vec_pretty(owner).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize runtime run owner".to_string()),
            )
        })?;
        fs::write(&temporary, raw).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("write runtime run owner {}", temporary.display())),
            )
        })?;
        fs::rename(&temporary, &owner_path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "replace runtime run owner {}",
                    owner_path.display()
                )),
            )
        })
    }

    pub(super) enum RuntimeTempPinState {
        Active(u32),
        Dead,
        Malformed(String),
        Absent,
    }

    pub(super) fn runtime_temp_pin_state(dir: &Path) -> RuntimeTempPinState {
        let path = dir.join(RUNTIME_TEMP_PIN_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return RuntimeTempPinState::Absent;
            }
            Err(error) => {
                return RuntimeTempPinState::Malformed(format!(
                "runtime temp pin cannot be inspected ({error}); remove {} after verifying no active owner",
                path.display()
            ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return RuntimeTempPinState::Malformed(format!(
                "runtime temp pin is not a regular file; remove {} after verifying no active owner",
                path.display()
            ));
        }
        let record = match fs::read_to_string(&path) {
            Ok(record) => record,
            Err(error) => {
                return RuntimeTempPinState::Malformed(format!(
                "runtime temp pin cannot be read ({error}); remove {} after verifying no active owner",
                path.display()
            ));
            }
        };
        let mut lines = record.lines();
        let schema = lines.next();
        let owner_pid = lines.next();
        if lines.next().is_some()
            || schema != Some(RUNTIME_TEMP_PIN_SCHEMA_LINE)
            || !matches!(owner_pid, Some(line) if line.starts_with("owner_pid="))
        {
            return RuntimeTempPinState::Malformed(format!(
            "runtime temp pin has an unrecognized contract; remove {} after verifying no active owner",
            path.display()
        ));
        }
        let owner_pid = owner_pid
            .and_then(|line| line.strip_prefix("owner_pid="))
            .and_then(|pid| pid.parse::<u32>().ok())
            .filter(|pid| *pid > 0);
        let Some(owner_pid) = owner_pid else {
            return RuntimeTempPinState::Malformed(format!(
            "runtime temp pin has an invalid owner PID; remove {} after verifying no active owner",
            path.display()
        ));
        };

        if crate::process::pid_is_running(owner_pid) {
            RuntimeTempPinState::Active(owner_pid)
        } else {
            RuntimeTempPinState::Dead
        }
    }

    pub(super) fn inspection_owner_protection(owner: &RuntimeRunOwner) -> Option<String> {
        if let Some(invocation_id) = owner
            .invocation_ids
            .iter()
            .find(|id| crate::engine::invocation::InvocationGuard::lease_is_active(id))
        {
            return Some(format!("invocation lease {invocation_id} is active"));
        }
        if let Some(run_id) = owner.run_id.as_deref() {
            let running = crate::observation::ObservationStore::open_initialized()
                .and_then(|store| store.get_run(run_id))
                .ok()
                .flatten()
                .is_some_and(|run| run.status == "running");
            if running {
                return Some(format!("persisted run {run_id} is active"));
            }
        }
        None
    }

    pub(super) fn owner_process_identity_matches(owner: &RuntimeRunOwner) -> bool {
        if !crate::process::pid_is_running(owner.owner_pid) {
            return false;
        }
        match owner.linux_starttime_ticks {
            Some(expected) => crate::process::linux_process_starttime_ticks(owner.owner_pid)
                .ok()
                .flatten()
                .is_some_and(|actual| actual == expected),
            None => !cfg!(target_os = "linux"),
        }
    }

    pub(super) fn runtime_root() -> Result<PathBuf> {
        if let Ok(override_dir) = env::var(runtime_tmpdir_env()) {
            let trimmed = override_dir.trim();
            if !trimmed.is_empty() {
                return Ok(PathBuf::from(trimmed));
            }
        }

        Ok(paths::homeboy()?.join("runtime").join("tmp"))
    }

    pub(super) fn runtime_tmpdir_env() -> String {
        crate::product_identity::PRODUCT_IDENTITY.env_var("RUNTIME_TMPDIR")
    }
}
use owner_support::*;

/// Inspected/planned/removed byte totals shared across cleanup output DTOs.
/// Flattened into the parent structs so the JSON wire format keeps
/// `inspected_count`, `planned_size_bytes`, and `removed_size_bytes` as
/// top-level keys.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CleanupSizeTotals {
    pub inspected_count: usize,
    pub planned_size_bytes: u64,
    pub removed_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeTempCleanupOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub runtime_tmp_root: String,
    pub older_than_days: u64,
    pub run_max_bytes: u64,
    pub run_max_count: usize,
    pub prefix: Option<String>,
    #[serde(flatten)]
    pub totals: CleanupSizeTotals,
    pub planned_count: usize,
    pub removed_count: usize,
    pub skipped_count: usize,
    pub planned_allocated_bytes: u64,
    pub removed_allocated_bytes: u64,
    pub verified_reclaimed_bytes: u64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub rows: Vec<RuntimeTempCleanupRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeTempCleanupRow {
    pub path: String,
    pub name: String,
    pub action: String,
    pub reason: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    pub verified_reclaimed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protection_reason: Option<String>,
}

pub fn cleanup_runtime_tmp(
    apply: bool,
    older_than_days: u64,
    prefix: Option<&str>,
    limit: usize,
) -> Result<RuntimeTempCleanupOutput> {
    cleanup_runtime_tmp_bounded(RuntimeTempCleanupOptions {
        apply,
        older_than_days,
        prefix,
        limit,
        run_max_bytes: u64::MAX,
        run_max_count: usize::MAX,
        cursor: None,
    })
}

pub fn cleanup_runtime_tmp_bounded(
    options: RuntimeTempCleanupOptions<'_>,
) -> Result<RuntimeTempCleanupOutput> {
    let root = runtime_root()?;
    let lock = acquire_cleanup_lock(&root)?;
    let mut output = RuntimeTempCleanupOutput {
        command: "self.cleanup-runtime-tmp",
        dry_run: !options.apply,
        runtime_tmp_root: root.display().to_string(),
        older_than_days: options.older_than_days,
        run_max_bytes: options.run_max_bytes,
        run_max_count: options.run_max_count,
        prefix: options.prefix.map(str::to_string),
        totals: CleanupSizeTotals {
            inspected_count: 0,
            planned_size_bytes: 0,
            removed_size_bytes: 0,
        },
        planned_count: 0,
        removed_count: 0,
        skipped_count: 0,
        planned_allocated_bytes: 0,
        removed_allocated_bytes: 0,
        verified_reclaimed_bytes: 0,
        has_more: false,
        next_cursor: None,
        rows: Vec::new(),
    };

    if !root.exists() {
        return Ok(output);
    }

    let now = SystemTime::now();
    let cutoff = now
        .checked_sub(Duration::from_secs(
            options.older_than_days.saturating_mul(86_400),
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut entries = fs::read_dir(&root)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read runtime tmp directory".into())))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::internal_io(e.to_string(), Some("read runtime tmp entry".into())))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut managed = Vec::new();
    let mut unmanaged = Vec::new();
    for entry in entries {
        if entry.file_name() == CLEANUP_LOCK_DIR {
            continue;
        }
        if entry.path().join(RUN_OWNER_FILE).is_file() {
            managed.push(entry);
        } else {
            unmanaged.push(entry);
        }
    }

    managed.sort_by(|left, right| {
        unique_name_timestamp(&right.file_name().to_string_lossy())
            .cmp(&unique_name_timestamp(&left.file_name().to_string_lossy()))
            .then_with(|| right.file_name().cmp(&left.file_name()))
    });
    let (cursor_name, mut retained_count, mut retained_bytes) = options
        .cursor
        .and_then(parse_runtime_cursor)
        .unwrap_or((None, 0, 0));
    let start = cursor_name
        .as_deref()
        .and_then(|cursor| {
            let cursor_timestamp = unique_name_timestamp(cursor);
            managed.iter().position(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let timestamp = unique_name_timestamp(&name);
                timestamp < cursor_timestamp
                    || (timestamp == cursor_timestamp && name.as_str() < cursor)
            })
        })
        .unwrap_or(0);
    let page_end = start
        .saturating_add(options.limit.max(1))
        .min(managed.len());
    output.has_more = page_end < managed.len();
    let page_last_name = managed
        .get(page_end.saturating_sub(1))
        .map(|entry| entry.file_name().to_string_lossy().to_string());
    let managed = managed.into_iter().skip(start).take(options.limit.max(1));

    let mut managed_inspections = Vec::new();
    for entry in managed {
        lock.heartbeat()?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if options
            .prefix
            .is_some_and(|prefix| !name.starts_with(prefix))
        {
            output.skipped_count += 1;
            output
                .rows
                .push(skip_row(path, name, "entry does not match prefix"));
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read runtime tmp {}", path.display())),
            )
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !path_is_within_root(&path, &root)
        {
            output.skipped_count += 1;
            output.rows.push(skip_row(
                path,
                name,
                "managed run path is not a safe directory",
            ));
            continue;
        }
        let (owner, metadata_warning) = match read_run_owner(&path) {
            Ok(owner) => (owner, None),
            Err(error) => {
                let modified = metadata.modified().unwrap_or(SystemTime::now());
                let age = SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default();
                let owner = RuntimeRunOwner {
                    schema: RUN_OWNER_SCHEMA.to_string(),
                    owner_id: format!("corrupt:{name}"),
                    owner_pid: 0,
                    linux_starttime_ticks: None,
                    run_id: None,
                    invocation_ids: Vec::new(),
                    state: "corrupt".to_string(),
                    created_at: chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339(),
                    completed_at: None,
                    reason: Some(error.message.clone()),
                };
                let warning = format!("owner metadata is corrupt: {}", error.message);
                if age < CORRUPT_OWNER_GRACE {
                    managed_inspections.push(ManagedRunInspection {
                        path: path.clone(),
                        name,
                        owner,
                        age_seconds: age.as_secs(),
                        size_bytes: path_storage_measure(&path)?.logical_bytes,
                        allocated_bytes: path_storage_measure(&path)?.allocated_bytes,
                        protection_reason: Some(format!(
                            "{warning}; protected during {}s quarantine grace",
                            CORRUPT_OWNER_GRACE.as_secs()
                        )),
                        metadata_warning: Some(warning),
                    });
                    continue;
                }
                (owner, Some(warning))
            }
        };
        let age_seconds = owner
            .completed_at
            .as_deref()
            .unwrap_or(&owner.created_at)
            .parse::<chrono::DateTime<chrono::Utc>>()
            .ok()
            .map(|time| {
                chrono::Utc::now()
                    .signed_duration_since(time)
                    .num_seconds()
                    .max(0) as u64
            })
            .unwrap_or(0);
        let lifecycle_protection = inspection_owner_protection(&owner);
        let pin_state = runtime_temp_pin_state(&path);
        let protection_reason = lifecycle_protection.or_else(|| match pin_state {
            RuntimeTempPinState::Active(pid) => {
                Some(format!("runtime temp pin owner PID {pid} is running"))
            }
            RuntimeTempPinState::Malformed(reason)
                if age_seconds < CORRUPT_OWNER_GRACE.as_secs() =>
            {
                Some(format!("{reason}; protected during quarantine grace"))
            }
            RuntimeTempPinState::Malformed(_) => None,
            RuntimeTempPinState::Dead | RuntimeTempPinState::Absent
                if owner.state == "active" && owner_process_identity_matches(&owner) =>
            {
                Some(format!(
                    "runtime run owner PID {} is running",
                    owner.owner_pid
                ))
            }
            RuntimeTempPinState::Dead | RuntimeTempPinState::Absent => None,
        });
        let storage = path_storage_measure(&path)?;
        managed_inspections.push(ManagedRunInspection {
            path: path.clone(),
            name,
            owner,
            age_seconds,
            size_bytes: storage.logical_bytes,
            allocated_bytes: storage.allocated_bytes,
            protection_reason,
            metadata_warning,
        });
    }

    managed_inspections.sort_by(|left, right| {
        left.age_seconds
            .cmp(&right.age_seconds)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut managed_decisions = Vec::new();
    for inspection in managed_inspections {
        let age_expired = inspection.age_seconds >= options.older_than_days.saturating_mul(86_400);
        let exceeds_count = retained_count >= options.run_max_count;
        let exceeds_bytes =
            retained_bytes.saturating_add(inspection.size_bytes) > options.run_max_bytes;
        let eligible = inspection.metadata_warning.is_some()
            || inspection.owner.state == "succeeded"
            || age_expired
            || exceeds_count
            || exceeds_bytes;
        let eligibility_reason = if inspection.protection_reason.is_some() || !eligible {
            None
        } else if let Some(warning) = inspection.metadata_warning.as_deref() {
            Some(format!("{warning}; quarantine grace expired"))
        } else if inspection.owner.state == "succeeded" {
            Some("successful run teardown is complete".to_string())
        } else if age_expired {
            Some("failed or interrupted run exceeded age retention".to_string())
        } else if exceeds_count {
            Some("failed or interrupted run exceeded count retention".to_string())
        } else {
            Some("failed or interrupted run exceeded byte retention".to_string())
        };
        if inspection.protection_reason.is_none() && eligibility_reason.is_none() {
            retained_count += 1;
            retained_bytes = retained_bytes.saturating_add(inspection.size_bytes);
        }
        managed_decisions.push((inspection, eligibility_reason));
    }
    if output.has_more {
        output.next_cursor =
            page_last_name.map(|name| format_runtime_cursor(&name, retained_count, retained_bytes));
    }
    // Candidate-first ordering lets bounded invocations converge even when the
    // configured inspection limit is smaller than the retained count budget.
    managed_decisions.sort_by(|left, right| {
        right
            .1
            .is_some()
            .cmp(&left.1.is_some())
            .then_with(|| right.0.age_seconds.cmp(&left.0.age_seconds))
            .then_with(|| left.0.path.cmp(&right.0.path))
    });
    for (inspection, eligibility_reason) in managed_decisions.into_iter().take(options.limit.max(1))
    {
        lock.heartbeat()?;
        output.totals.inspected_count += 1;
        let mut row = RuntimeTempCleanupRow {
            path: inspection.path.display().to_string(),
            name: inspection.name,
            action: "skip".to_string(),
            reason: String::new(),
            size_bytes: inspection.size_bytes,
            allocated_bytes: inspection.allocated_bytes,
            verified_reclaimed_bytes: 0,
            owner_id: Some(inspection.owner.owner_id.clone()),
            owner_pid: Some(inspection.owner.owner_pid),
            owner_state: Some(inspection.owner.state.clone()),
            age_seconds: Some(inspection.age_seconds),
            protection_reason: inspection.protection_reason.clone(),
        };
        if let Some(reason) = inspection.protection_reason {
            row.reason = reason;
            output.skipped_count += 1;
        } else if let Some(reason) = eligibility_reason {
            row.action = if options.apply { "removed" } else { "remove" }.to_string();
            row.reason = reason;
            output.planned_count += 1;
            output.totals.planned_size_bytes += inspection.size_bytes;
            output.planned_allocated_bytes += inspection.allocated_bytes;
            if options.apply {
                let available_before = filesystem_available_bytes(&inspection.path);
                remove_runtime_tmp_entry(
                    &inspection.path,
                    &fs::symlink_metadata(&inspection.path).map_err(|error| {
                        Error::internal_io(
                            error.to_string(),
                            Some(format!("restat {}", inspection.path.display())),
                        )
                    })?,
                )?;
                if !inspection.path.exists() {
                    let verified = verified_reclaimed_bytes(
                        available_before,
                        filesystem_available_bytes(&inspection.path),
                        inspection.allocated_bytes,
                    );
                    output.removed_count += 1;
                    output.totals.removed_size_bytes += inspection.size_bytes;
                    output.removed_allocated_bytes += inspection.allocated_bytes;
                    output.verified_reclaimed_bytes += verified;
                    row.verified_reclaimed_bytes = verified;
                }
            }
        } else {
            row.reason = "failed run evidence is within bounded retention".to_string();
            row.protection_reason = Some(row.reason.clone());
            output.skipped_count += 1;
        }
        output.rows.push(row);
    }

    for entry in unmanaged.into_iter().take(if output.has_more {
        0
    } else {
        options.limit.max(1).saturating_sub(output.rows.len())
    }) {
        lock.heartbeat()?;
        output.totals.inspected_count += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let mut row = RuntimeTempCleanupRow {
            path: path.display().to_string(),
            name: name.clone(),
            action: "skip".to_string(),
            reason: String::new(),
            size_bytes: 0,
            allocated_bytes: 0,
            verified_reclaimed_bytes: 0,
            owner_id: None,
            owner_pid: None,
            owner_state: None,
            age_seconds: None,
            protection_reason: None,
        };

        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read runtime tmp {}", path.display())),
            )
        })?;

        if options
            .prefix
            .is_some_and(|prefix| !name.starts_with(prefix))
        {
            row.reason = "entry does not match prefix".to_string();
            output.skipped_count += 1;
        } else if metadata.file_type().is_symlink() {
            row.reason = "entry is a symlink".to_string();
            output.skipped_count += 1;
        } else if !path_is_within_root(&path, &root) {
            row.reason = "entry path is outside runtime tmp root".to_string();
            output.skipped_count += 1;
        } else if metadata.is_dir() {
            match runtime_temp_pin_state(&path) {
                RuntimeTempPinState::Active(owner_pid) => {
                    row.reason = format!("runtime temp pin owner PID {owner_pid} is running");
                    output.skipped_count += 1;
                }
                RuntimeTempPinState::Malformed(reason) => {
                    row.reason = reason;
                    output.skipped_count += 1;
                }
                RuntimeTempPinState::Dead | RuntimeTempPinState::Absent => {
                    cleanup_runtime_tmp_entry(
                        &path,
                        &metadata,
                        cutoff,
                        options.apply,
                        &mut row,
                        &mut output,
                    )?;
                }
            }
        } else if metadata.modified().is_ok_and(|modified| modified > cutoff) {
            row.reason = "entry is newer than retention cutoff".to_string();
            output.skipped_count += 1;
        } else {
            cleanup_runtime_tmp_entry(
                &path,
                &metadata,
                cutoff,
                options.apply,
                &mut row,
                &mut output,
            )?;
        }

        output.rows.push(row);
    }

    Ok(output)
}

mod cleanup_support {
    use super::*;

    pub(super) fn unique_name_timestamp(name: &str) -> u128 {
        name.rsplit('-')
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    pub(super) fn format_runtime_cursor(
        name: &str,
        retained_count: usize,
        retained_bytes: u64,
    ) -> String {
        format!("{name}|{retained_count}|{retained_bytes}")
    }

    pub(super) fn parse_runtime_cursor(cursor: &str) -> Option<(Option<String>, usize, u64)> {
        let mut fields = cursor.rsplitn(3, '|');
        let retained_bytes = fields.next()?.parse().ok()?;
        let retained_count = fields.next()?.parse().ok()?;
        let name = fields.next()?.to_string();
        Some((Some(name), retained_count, retained_bytes))
    }

    pub(super) fn skip_row(path: PathBuf, name: String, reason: &str) -> RuntimeTempCleanupRow {
        RuntimeTempCleanupRow {
            path: path.display().to_string(),
            name,
            action: "skip".to_string(),
            reason: reason.to_string(),
            size_bytes: 0,
            allocated_bytes: 0,
            verified_reclaimed_bytes: 0,
            owner_id: None,
            owner_pid: None,
            owner_state: None,
            age_seconds: None,
            protection_reason: Some(reason.to_string()),
        }
    }

    pub(super) fn acquire_cleanup_lock(root: &Path) -> Result<RuntimeTempCleanupLock> {
        fs::create_dir_all(root).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", root.display())),
            )
        })?;
        let path = root.join(CLEANUP_LOCK_DIR);
        let token = uuid::Uuid::new_v4().to_string();
        for _ in 0..CLEANUP_LOCK_ATTEMPTS {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let owner = RuntimeTempCleanupLockOwner {
                        token: token.clone(),
                        pid: std::process::id(),
                        linux_starttime_ticks: crate::process::linux_process_starttime_ticks(
                            std::process::id(),
                        )
                        .ok()
                        .flatten(),
                        heartbeat_unix_ms: unix_time_ms(),
                    };
                    if let Err(error) =
                        write_cleanup_lock_owner(&path.join(CLEANUP_LOCK_OWNER_FILE), &owner)
                    {
                        let _ = fs::remove_dir_all(&path);
                        return Err(error);
                    }
                    return Ok(RuntimeTempCleanupLock {
                        path,
                        token: token.clone(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let owner = fs::read_to_string(path.join(CLEANUP_LOCK_OWNER_FILE))
                        .ok()
                        .and_then(|raw| {
                            serde_json::from_str::<RuntimeTempCleanupLockOwner>(&raw).ok()
                        });
                    let stale = owner.as_ref().is_some_and(|owner| {
                        unix_time_ms().saturating_sub(owner.heartbeat_unix_ms)
                            > CLEANUP_LOCK_STALE_AFTER.as_millis() as u64
                            && !process_identity_matches(owner.pid, owner.linux_starttime_ticks)
                    }) || owner.is_none()
                        && fs::metadata(&path)
                            .ok()
                            .and_then(|metadata| metadata.modified().ok())
                            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                            .is_some_and(|age| age > CLEANUP_LOCK_STALE_AFTER);
                    if stale {
                        let observed_token = owner
                            .as_ref()
                            .map(|owner| owner.token.as_str())
                            .unwrap_or("malformed");
                        let quarantine = root.join(format!(
                            ".cleanup.lock.stale-{}-{}",
                            paths::sanitize_path_segment(observed_token),
                            uuid::Uuid::new_v4()
                        ));
                        if fs::rename(&path, &quarantine).is_ok() {
                            let _ = fs::remove_dir_all(quarantine);
                        }
                    } else {
                        std::thread::sleep(CLEANUP_LOCK_SLEEP);
                    }
                }
                Err(error) => {
                    return Err(Error::internal_io(
                        error.to_string(),
                        Some(format!("acquire runtime cleanup lock {}", path.display())),
                    ));
                }
            }
        }
        Err(Error::internal_unexpected(format!(
            "timed out acquiring runtime cleanup lock {}",
            path.display()
        )))
    }

    fn process_identity_matches(pid: u32, expected_starttime: Option<u64>) -> bool {
        if !crate::process::pid_is_running(pid) {
            return false;
        }
        match expected_starttime {
            Some(expected) => crate::process::linux_process_starttime_ticks(pid)
                .ok()
                .flatten()
                .is_some_and(|actual| actual == expected),
            None => !cfg!(target_os = "linux"),
        }
    }

    pub(super) fn unix_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    pub(super) fn write_cleanup_lock_owner(
        path: &Path,
        owner: &RuntimeTempCleanupLockOwner,
    ) -> Result<()> {
        let temporary = path.with_extension(format!("tmp-{}", owner.token));
        let raw = serde_json::to_vec(owner).map_err(|error| {
            Error::internal_json(error.to_string(), Some("runtime cleanup lock".to_string()))
        })?;
        fs::write(&temporary, raw).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write runtime cleanup lock".to_string()),
            )
        })?;
        fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(temporary);
            Error::internal_io(
                error.to_string(),
                Some("replace runtime cleanup lock".to_string()),
            )
        })
    }

    pub(super) fn cleanup_runtime_tmp_entry(
        path: &Path,
        metadata: &fs::Metadata,
        cutoff: SystemTime,
        apply: bool,
        row: &mut RuntimeTempCleanupRow,
        output: &mut RuntimeTempCleanupOutput,
    ) -> Result<()> {
        if metadata.modified().is_ok_and(|modified| modified > cutoff) {
            row.reason = "entry is newer than retention cutoff".to_string();
            output.skipped_count += 1;
            return Ok(());
        }

        row.action = if apply { "removed" } else { "remove" }.to_string();
        row.reason = "runtime tmp entry is eligible".to_string();
        let storage = path_storage_measure(path)?;
        row.size_bytes = storage.logical_bytes;
        row.allocated_bytes = storage.allocated_bytes;
        output.planned_count += 1;
        output.totals.planned_size_bytes += row.size_bytes;
        output.planned_allocated_bytes += row.allocated_bytes;
        if apply {
            let available_before = filesystem_available_bytes(path);
            remove_runtime_tmp_entry(path, metadata)?;
            if !path.exists() {
                row.verified_reclaimed_bytes = verified_reclaimed_bytes(
                    available_before,
                    filesystem_available_bytes(path),
                    row.allocated_bytes,
                );
                output.removed_count += 1;
                output.totals.removed_size_bytes += row.size_bytes;
                output.removed_allocated_bytes += row.allocated_bytes;
                output.verified_reclaimed_bytes += row.verified_reclaimed_bytes;
            }
        }
        Ok(())
    }

    pub(super) fn path_is_within_root(path: &Path, root: &Path) -> bool {
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let candidate = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        candidate.starts_with(root)
    }

    #[derive(Debug, Clone, Copy)]
    pub(super) struct StorageMeasure {
        pub(super) logical_bytes: u64,
        pub(super) allocated_bytes: u64,
    }

    pub(super) fn path_storage_measure(path: &Path) -> Result<StorageMeasure> {
        path_storage_measure_inner(path, &mut HashSet::new())
    }

    fn path_storage_measure_inner(
        path: &Path,
        seen: &mut HashSet<(u64, u64)>,
    ) -> Result<StorageMeasure> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
        })?;
        if file_identity(&metadata).is_some_and(|identity| !seen.insert(identity)) {
            return Ok(StorageMeasure {
                logical_bytes: 0,
                allocated_bytes: 0,
            });
        }
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Ok(StorageMeasure {
                logical_bytes: metadata.len(),
                allocated_bytes: allocated_bytes(&metadata),
            });
        }
        let mut total = StorageMeasure {
            logical_bytes: 0,
            allocated_bytes: allocated_bytes(&metadata),
        };
        for entry in fs::read_dir(path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })? {
            let entry = entry.map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("read entry {}", path.display())),
                )
            })?;
            let measure = path_storage_measure_inner(&entry.path(), seen)?;
            total.logical_bytes = total.logical_bytes.saturating_add(measure.logical_bytes);
            total.allocated_bytes = total
                .allocated_bytes
                .saturating_add(measure.allocated_bytes);
        }
        Ok(total)
    }

    #[cfg(unix)]
    fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        Some((metadata.dev(), metadata.ino()))
    }

    #[cfg(not(unix))]
    fn file_identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
        None
    }

    #[cfg(unix)]
    fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;
        metadata.blocks().saturating_mul(512)
    }

    #[cfg(not(unix))]
    fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
        metadata.len()
    }

    pub(super) fn filesystem_available_bytes(path: &Path) -> Option<u64> {
        fs4::available_space(path.parent().unwrap_or(path)).ok()
    }

    pub(super) fn verified_reclaimed_bytes(
        before: Option<u64>,
        after: Option<u64>,
        allocated: u64,
    ) -> u64 {
        match (before, after) {
            (Some(before), Some(after)) => after.saturating_sub(before).min(allocated),
            _ => 0,
        }
    }

    pub(super) fn remove_runtime_tmp_entry(path: &Path, metadata: &fs::Metadata) -> Result<()> {
        if metadata.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("remove {}", path.display()))))
    }
}
use cleanup_support::*;

fn ensure_runtime_tmp_dir() -> Result<PathBuf> {
    let runtime_dir = runtime_root()?;
    fs::create_dir_all(&runtime_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("create homeboy runtime tmp directory".to_string()),
        )
    })?;
    Ok(runtime_dir)
}

/// Create a temporary directory under the runtime temp root.
///
/// Used by `RunDir::create()` for pipeline run directories and by
/// `git/release_download.rs` for ephemeral download artifacts.
pub fn runtime_temp_dir(prefix: &str) -> Result<PathBuf> {
    let path = ensure_runtime_tmp_dir()?.join(unique_name(prefix, ""));
    fs::create_dir_all(&path).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("create temp dir {prefix}")))
    })?;
    Ok(path)
}

pub fn unique_name(prefix: &str, suffix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    format!("{prefix}-{}-{nanos}{suffix}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{home_env_guard, with_isolated_home};

    fn failed_run(prefix: &str, payload_bytes: usize) -> PathBuf {
        let (path, pin) = managed_run_temp_dir(prefix).expect("managed run dir");
        fs::write(path.join("evidence.bin"), vec![b'x'; payload_bytes]).expect("write evidence");
        retain_failed_run_dir(&path);
        drop(pin);
        path
    }

    fn bounded_options<'a>(apply: bool, prefix: Option<&'a str>) -> RuntimeTempCleanupOptions<'a> {
        RuntimeTempCleanupOptions {
            apply,
            older_than_days: 7,
            prefix,
            limit: 100,
            run_max_bytes: 1024 * 1024 * 1024,
            run_max_count: 100,
            cursor: None,
        }
    }

    #[test]
    fn runtime_temp_dir_honors_override() {
        let _guard = home_env_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), dir.path());

        let path = runtime_temp_dir("homeboy-test-dir").expect("temp dir path");
        assert!(path.starts_with(dir.path()));
        assert!(path.is_dir());

        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn runtime_temp_dir_creates_dir() {
        with_isolated_home(|_| {
            let result = runtime_temp_dir("test-dir");
            assert!(result.is_ok());
            if let Ok(path) = result {
                assert!(path.is_dir());
            }
        });
    }

    #[test]
    fn cleanup_runtime_tmp_plans_and_removes_old_entries() {
        let _guard = home_env_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), dir.path());
        let prefix = "homeboy-cleanup-test";
        let stale = runtime_temp_dir(prefix).expect("temp dir");
        fs::write(stale.join("trace.json"), b"trace").expect("write trace");

        let dry = cleanup_runtime_tmp(false, 0, Some(prefix), 100).expect("dry-run");
        assert!(dry.dry_run);
        assert_eq!(dry.planned_count, 1);
        assert!(stale.exists());

        let applied = cleanup_runtime_tmp(true, 0, Some(prefix), 100).expect("apply");
        assert!(!applied.dry_run);
        assert_eq!(applied.removed_count, 1);
        assert!(!stale.exists());

        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn cleanup_runtime_tmp_reclaims_dead_owner_pin() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let stale = runtime_temp_dir("deploy-download").expect("runtime directory");
        std::fs::write(
            stale.join(RUNTIME_TEMP_PIN_FILE),
            format!("{RUNTIME_TEMP_PIN_SCHEMA_LINE}\nowner_pid=4294967295\n"),
        )
        .expect("dead owner pin");

        let output =
            cleanup_runtime_tmp(true, 0, Some("deploy-download"), 10).expect("reclaim stale pin");
        assert_eq!(output.removed_count, 1);
        assert!(!stale.exists());

        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn cleanup_runtime_tmp_skips_malformed_pin_with_remediation() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let directory = runtime_temp_dir("deploy-download").expect("runtime directory");
        std::fs::write(directory.join(RUNTIME_TEMP_PIN_FILE), "not a pin\n")
            .expect("malformed pin");

        let output = cleanup_runtime_tmp(true, 0, Some("deploy-download"), 10)
            .expect("inspect malformed pin");
        assert_eq!(output.removed_count, 0);
        assert_eq!(output.skipped_count, 1);
        assert!(output.rows[0].reason.contains("unrecognized contract"));
        assert!(output.rows[0]
            .reason
            .contains("after verifying no active owner"));
        assert!(directory.exists());

        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn failed_run_evidence_is_retained_with_owner_diagnostics() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let path = failed_run("homeboy-run-retained", 32);

        let output = cleanup_runtime_tmp_bounded(bounded_options(true, Some("homeboy-run")))
            .expect("cleanup");

        assert_eq!(output.removed_count, 0);
        assert_eq!(output.skipped_count, 1);
        assert_eq!(output.rows[0].owner_state.as_deref(), Some("failed"));
        assert!(output.rows[0].owner_id.is_some());
        assert!(output.rows[0].owner_pid.is_some());
        assert!(output.rows[0].age_seconds.is_some());
        assert_eq!(
            output.rows[0].protection_reason.as_deref(),
            Some("failed run evidence is within bounded retention")
        );
        assert!(path.exists());
        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn active_run_and_artifact_promotion_are_protected_until_release() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let (path, pin) = managed_run_temp_dir("homeboy-run-promotion").expect("managed run");
        fs::write(path.join("artifact.json"), b"promote me").expect("artifact");
        let mut options = bounded_options(true, Some("homeboy-run"));
        options.older_than_days = 0;
        options.run_max_bytes = 0;
        options.run_max_count = 0;

        let protected = cleanup_runtime_tmp_bounded(options).expect("protected cleanup");
        assert_eq!(protected.removed_count, 0);
        assert!(protected.rows[0]
            .protection_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("owner PID")));
        assert_eq!(
            fs::read(path.join("artifact.json")).expect("artifact remains"),
            b"promote me"
        );

        mark_run_dir_succeeded(&path);
        drop(pin);
        let removed = cleanup_runtime_tmp_bounded(options).expect("released cleanup");
        assert_eq!(removed.removed_count, 1);
        assert!(!path.exists());
        env::remove_var(runtime_tmpdir_env());
    }

    mod retention_tests;
}
