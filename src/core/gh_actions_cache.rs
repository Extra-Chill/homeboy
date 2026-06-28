//! Filesystem persistence for GitHub Actions run ingestion.
//!
//! The `runs gh-actions` command is a thin adapter: it computes data and paths,
//! then delegates artifact and HTTP-cache file writes to these helpers so the
//! orchestration of directory creation and byte writes lives in core.

use crate::core::error::{Error, Result};
use crate::core::paths;
use std::fs;
use std::path::{Path, PathBuf};

/// Sanitize an artifact file name so it cannot escape its target directory.
pub fn sanitize_artifact_file_name(raw: &str) -> String {
    raw.replace(['/', '\\', '\0'], "_")
}

/// Materialize a downloaded artifact file under the homeboy data dir.
///
/// Creates `<data>/artifacts/<homeboy_run_id>/` and writes
/// `<artifact_id>-<safe_name>`, returning the written path. Errors propagate.
pub fn persist_artifact_file(
    homeboy_run_id: &str,
    artifact_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let safe_name = sanitize_artifact_file_name(file_name);
    let target_dir = paths::homeboy_data()?
        .join("artifacts")
        .join(homeboy_run_id);
    fs::create_dir_all(&target_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create artifact dir {}", target_dir.display())),
        )
    })?;
    let target = target_dir.join(format!("{artifact_id}-{safe_name}"));
    fs::write(&target, bytes).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write artifact file {}", target.display())),
        )
    })?;
    Ok(target)
}

/// Compute (and ensure) the cache path for a list-runs cache entry.
///
/// Creates `<homeboy>/cache/gh-actions-runs/` and returns the path for
/// `<key>.<ext>`. Errors propagate.
pub fn list_runs_cache_path(key: &str, ext: &str) -> Result<PathBuf> {
    let base = paths::homeboy()?.join("cache").join("gh-actions-runs");
    fs::create_dir_all(&base).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create cache dir {}", base.display())),
        )
    })?;
    Ok(base.join(format!("{key}.{ext}")))
}

/// Persist the list-runs HTTP cache body and optional ETag for the next call.
///
/// Best-effort: directory creation and writes are ignored on failure to match
/// the prior inline behavior (a failed cache write must not fail the command).
pub fn write_runs_cache(body_path: &Path, etag_path: &Path, body: &[u8], etag: Option<&str>) {
    if let Some(parent) = body_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(body_path, body);
    if let Some(value) = etag {
        let _ = fs::write(etag_path, value);
    }
}
