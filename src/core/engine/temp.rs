use crate::core::error::{Error, Result};
use crate::core::paths;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

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
    crate::core::product_identity::PRODUCT_IDENTITY.env_var("RUNTIME_TMPDIR")
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
        } else if metadata.modified().is_ok_and(|modified| modified > cutoff) {
            row.reason = "entry is newer than retention cutoff".to_string();
            output.skipped_count += 1;
        } else {
            row.action = if apply { "removed" } else { "remove" }.to_string();
            row.reason = "runtime tmp entry is eligible".to_string();
            row.size_bytes = path_size_bytes(&path, &metadata)?;
            output.planned_count += 1;
            output.totals.planned_size_bytes += row.size_bytes;
            if apply {
                remove_runtime_tmp_entry(&path, &metadata)?;
                output.removed_count += 1;
                output.totals.removed_size_bytes += row.size_bytes;
            }
        }

        output.rows.push(row);
    }

    Ok(output)
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
/// `deploy/release_download.rs` for ephemeral download artifacts.
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
}
