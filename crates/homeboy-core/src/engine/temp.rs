use crate::error::{Error, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

const RUNTIME_TEMP_PIN_FILE: &str = ".homeboy-runtime-temp-pin-v1";
const RUNTIME_TEMP_PIN_SCHEMA_LINE: &str = "schema=homeboy-runtime-temp-pin-v1";
const RUN_OWNER_FILE: &str = ".homeboy-run-owner-v1.json";
const RUN_OWNER_SCHEMA: &str = "homeboy/runtime-run-owner/v1";
const CLEANUP_LOCK_DIR: &str = ".cleanup.lock";
const CLEANUP_LOCK_STALE_AFTER: Duration = Duration::from_secs(300);
const CLEANUP_LOCK_ATTEMPTS: usize = 100;
const CLEANUP_LOCK_SLEEP: Duration = Duration::from_millis(20);

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
}

#[derive(Debug)]
struct ManagedRunInspection {
    path: PathBuf,
    name: String,
    owner: RuntimeRunOwner,
    age_seconds: u64,
    size_bytes: u64,
    protection_reason: Option<String>,
}

struct RuntimeTempCleanupLock {
    path: PathBuf,
}

impl Drop for RuntimeTempCleanupLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
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

fn update_run_owner(path: &Path, state: &str, reason: &str) {
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

fn read_run_owner(path: &Path) -> Result<RuntimeRunOwner> {
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

fn write_run_owner(path: &Path, owner: &RuntimeRunOwner) -> Result<()> {
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

enum RuntimeTempPinState {
    Active(u32),
    Dead,
    Malformed(String),
    Absent,
}

fn runtime_temp_pin_state(dir: &Path) -> RuntimeTempPinState {
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

fn runtime_root() -> Result<PathBuf> {
    if let Ok(override_dir) = env::var(runtime_tmpdir_env()) {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    Ok(paths::homeboy()?.join("runtime").join("tmp"))
}

fn runtime_tmpdir_env() -> String {
    crate::product_identity::PRODUCT_IDENTITY.env_var("RUNTIME_TMPDIR")
}

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
    pub rows: Vec<RuntimeTempCleanupRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeTempCleanupRow {
    pub path: String,
    pub name: String,
    pub action: String,
    pub reason: String,
    pub size_bytes: u64,
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
    })
}

pub fn cleanup_runtime_tmp_bounded(
    options: RuntimeTempCleanupOptions<'_>,
) -> Result<RuntimeTempCleanupOutput> {
    let root = runtime_root()?;
    let _lock = acquire_cleanup_lock(&root)?;
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

    let mut managed_inspections = Vec::new();
    for entry in managed {
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
        let owner = match read_run_owner(&path) {
            Ok(owner) => owner,
            Err(error) => {
                output.skipped_count += 1;
                output.rows.push(skip_row(
                    path,
                    name,
                    &format!("owner metadata is protected: {}", error.message),
                ));
                continue;
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
        let protection_reason = match runtime_temp_pin_state(&path) {
            RuntimeTempPinState::Active(pid) => {
                Some(format!("runtime temp pin owner PID {pid} is running"))
            }
            RuntimeTempPinState::Malformed(reason) => Some(reason),
            RuntimeTempPinState::Dead | RuntimeTempPinState::Absent
                if owner.state == "active" && crate::process::pid_is_running(owner.owner_pid) =>
            {
                Some(format!(
                    "runtime run owner PID {} is running",
                    owner.owner_pid
                ))
            }
            RuntimeTempPinState::Dead | RuntimeTempPinState::Absent => None,
        };
        managed_inspections.push(ManagedRunInspection {
            path: path.clone(),
            name,
            owner,
            age_seconds,
            size_bytes: path_size_bytes(&path, &metadata)?,
            protection_reason,
        });
    }

    managed_inspections.sort_by(|left, right| {
        left.age_seconds
            .cmp(&right.age_seconds)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut retained_count = 0usize;
    let mut retained_bytes = 0u64;
    let mut managed_decisions = Vec::new();
    for inspection in managed_inspections {
        let age_expired = inspection.age_seconds >= options.older_than_days.saturating_mul(86_400);
        let exceeds_count = retained_count >= options.run_max_count;
        let exceeds_bytes =
            retained_bytes.saturating_add(inspection.size_bytes) > options.run_max_bytes;
        let eligible =
            inspection.owner.state == "succeeded" || age_expired || exceeds_count || exceeds_bytes;
        let eligibility_reason = if inspection.protection_reason.is_some() || !eligible {
            None
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
        output.totals.inspected_count += 1;
        let mut row = RuntimeTempCleanupRow {
            path: inspection.path.display().to_string(),
            name: inspection.name,
            action: "skip".to_string(),
            reason: String::new(),
            size_bytes: inspection.size_bytes,
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
            if options.apply {
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
                    output.removed_count += 1;
                    output.totals.removed_size_bytes += inspection.size_bytes;
                }
            }
        } else {
            row.reason = "failed run evidence is within bounded retention".to_string();
            row.protection_reason = Some(row.reason.clone());
            output.skipped_count += 1;
        }
        output.rows.push(row);
    }

    for entry in unmanaged
        .into_iter()
        .take(options.limit.max(1).saturating_sub(output.rows.len()))
    {
        output.totals.inspected_count += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let mut row = RuntimeTempCleanupRow {
            path: path.display().to_string(),
            name: name.clone(),
            action: "skip".to_string(),
            reason: String::new(),
            size_bytes: 0,
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

fn skip_row(path: PathBuf, name: String, reason: &str) -> RuntimeTempCleanupRow {
    RuntimeTempCleanupRow {
        path: path.display().to_string(),
        name,
        action: "skip".to_string(),
        reason: reason.to_string(),
        size_bytes: 0,
        owner_id: None,
        owner_pid: None,
        owner_state: None,
        age_seconds: None,
        protection_reason: Some(reason.to_string()),
    }
}

fn acquire_cleanup_lock(root: &Path) -> Result<RuntimeTempCleanupLock> {
    fs::create_dir_all(root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", root.display())),
        )
    })?;
    let path = root.join(CLEANUP_LOCK_DIR);
    for _ in 0..CLEANUP_LOCK_ATTEMPTS {
        match fs::create_dir(&path) {
            Ok(()) => return Ok(RuntimeTempCleanupLock { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age > CLEANUP_LOCK_STALE_AFTER);
                if stale {
                    let _ = fs::remove_dir(&path);
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

fn cleanup_runtime_tmp_entry(
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
    row.size_bytes = path_size_bytes(path, metadata)?;
    output.planned_count += 1;
    output.totals.planned_size_bytes += row.size_bytes;
    if apply {
        remove_runtime_tmp_entry(path, metadata)?;
        output.removed_count += 1;
        output.totals.removed_size_bytes += row.size_bytes;
    }
    Ok(())
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
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

fn path_size_bytes(path: &Path, metadata: &fs::Metadata) -> Result<u64> {
    if metadata.is_dir() {
        let mut total = 0;
        for entry in fs::read_dir(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read directory {}", path.display())),
            )
        })? {
            let entry = entry.map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read directory {}", path.display())),
                )
            })?;
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read runtime tmp {}", entry_path.display())),
                )
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            total += path_size_bytes(&entry_path, &metadata)?;
        }
        Ok(total)
    } else {
        Ok(metadata.len())
    }
}

fn remove_runtime_tmp_entry(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|e| Error::internal_io(e.to_string(), Some(format!("remove {}", path.display()))))
}

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

    #[test]
    fn stale_crashed_run_becomes_eligible() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let (path, pin) = managed_run_temp_dir("homeboy-run-crashed").expect("managed run");
        let mut owner = read_run_owner(&path).expect("owner");
        owner.owner_pid = u32::MAX;
        owner.created_at = "2000-01-01T00:00:00Z".to_string();
        write_run_owner(&path, &owner).expect("stale owner");
        drop(pin);
        fs::write(
            path.join(RUNTIME_TEMP_PIN_FILE),
            format!("{RUNTIME_TEMP_PIN_SCHEMA_LINE}\nowner_pid={}\n", u32::MAX),
        )
        .expect("dead pin");

        let output = cleanup_runtime_tmp_bounded(bounded_options(true, Some("homeboy-run")))
            .expect("cleanup stale run");
        assert_eq!(output.removed_count, 1);
        assert!(!path.exists());
        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn failed_runs_converge_under_count_and_byte_bounds() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let first = failed_run("homeboy-run-bound", 32);
        std::thread::sleep(Duration::from_millis(5));
        let second = failed_run("homeboy-run-bound", 64);
        let mut count_options = bounded_options(true, Some("homeboy-run-bound"));
        count_options.run_max_count = 1;
        count_options.limit = 1;

        let count_output = cleanup_runtime_tmp_bounded(count_options).expect("count cleanup");
        assert_eq!(count_output.removed_count, 1);
        assert!(first.exists() ^ second.exists());

        let mut byte_options = bounded_options(true, Some("homeboy-run-bound"));
        byte_options.run_max_bytes = 0;
        let byte_output = cleanup_runtime_tmp_bounded(byte_options).expect("byte cleanup");
        assert_eq!(byte_output.removed_count, 1);
        assert!(!first.exists() && !second.exists());
        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn apply_reports_verified_bytes_and_is_idempotent() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let path = failed_run("homeboy-run-accounting", 257);
        let metadata = fs::symlink_metadata(&path).expect("metadata");
        let expected = path_size_bytes(&path, &metadata).expect("size");
        let mut options = bounded_options(true, Some("homeboy-run-accounting"));
        options.older_than_days = 0;

        let applied = cleanup_runtime_tmp_bounded(options).expect("apply");
        assert_eq!(applied.removed_count, 1);
        assert_eq!(applied.totals.removed_size_bytes, expected);
        assert!(!path.exists());

        let repeated = cleanup_runtime_tmp_bounded(options).expect("repeat apply");
        assert_eq!(repeated.removed_count, 0);
        assert_eq!(repeated.totals.removed_size_bytes, 0);
        env::remove_var(runtime_tmpdir_env());
    }

    #[test]
    fn concurrent_cleanup_serializes_and_removes_once() {
        let _guard = home_env_guard();
        let root = tempfile::tempdir().expect("tempdir");
        env::set_var(runtime_tmpdir_env(), root.path());
        let path = failed_run("homeboy-run-concurrent", 128);
        let mut options = bounded_options(true, Some("homeboy-run-concurrent"));
        options.older_than_days = 0;

        let first =
            std::thread::spawn(move || cleanup_runtime_tmp_bounded(options).expect("first"));
        let second =
            std::thread::spawn(move || cleanup_runtime_tmp_bounded(options).expect("second"));
        let outputs = [
            first.join().expect("first join"),
            second.join().expect("second join"),
        ];

        assert_eq!(
            outputs
                .iter()
                .map(|output| output.removed_count)
                .sum::<usize>(),
            1
        );
        assert!(!path.exists());
        env::remove_var(runtime_tmpdir_env());
    }
}
