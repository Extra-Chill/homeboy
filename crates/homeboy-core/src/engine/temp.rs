use crate::error::{Error, Result};
use crate::paths;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

const RUNTIME_TEMP_PIN_FILE: &str = ".homeboy-runtime-temp-pin-v1";
const RUNTIME_TEMP_PIN_SCHEMA_LINE: &str = "schema=homeboy-runtime-temp-pin-v1";

/// A process-owned pin which prevents runtime-temp cleanup from removing its directory.
#[derive(Debug)]
pub(crate) struct RuntimeTempPin {
    path: PathBuf,
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
}

pub fn cleanup_runtime_tmp(
    apply: bool,
    older_than_days: u64,
    prefix: Option<&str>,
    limit: usize,
) -> Result<RuntimeTempCleanupOutput> {
    let root = runtime_root()?;
    let mut output = RuntimeTempCleanupOutput {
        command: "self.cleanup-runtime-tmp",
        dry_run: !apply,
        runtime_tmp_root: root.display().to_string(),
        older_than_days,
        prefix: prefix.map(str::to_string),
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

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(older_than_days.saturating_mul(86_400)))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut entries = fs::read_dir(&root)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read runtime tmp directory".into())))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::internal_io(e.to_string(), Some("read runtime tmp entry".into())))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries.into_iter().take(limit.max(1)) {
        output.totals.inspected_count += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let mut row = RuntimeTempCleanupRow {
            path: path.display().to_string(),
            name: name.clone(),
            action: "skip".to_string(),
            reason: String::new(),
            size_bytes: 0,
        };

        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read runtime tmp {}", path.display())),
            )
        })?;

        if prefix.is_some_and(|prefix| !name.starts_with(prefix)) {
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
                        apply,
                        &mut row,
                        &mut output,
                    )?;
                }
            }
        } else if metadata.modified().is_ok_and(|modified| modified > cutoff) {
            row.reason = "entry is newer than retention cutoff".to_string();
            output.skipped_count += 1;
        } else {
            cleanup_runtime_tmp_entry(&path, &metadata, cutoff, apply, &mut row, &mut output)?;
        }

        output.rows.push(row);
    }

    Ok(output)
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

pub(crate) fn unique_name(prefix: &str, suffix: &str) -> String {
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
}
