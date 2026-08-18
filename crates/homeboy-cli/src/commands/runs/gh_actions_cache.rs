//! Filesystem persistence for GitHub Actions run ingestion.
//!
//! The `runs gh-actions` command is a thin adapter: it computes data and paths,
//! then delegates artifact and HTTP-cache file writes to these helpers so the
//! orchestration of directory creation and byte writes lives here.

use homeboy::core::error::{Error, Result};
use homeboy::core::paths;
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
    persist_artifact_file_in_roots(
        &paths::homeboy_data()?,
        homeboy_run_id,
        artifact_id,
        file_name,
        bytes,
    )
}

/// Materialize a downloaded artifact file under an already-resolved data root.
///
/// An import walks every artifact of every run; resolving the data root per
/// file made the destination of one import depend on process-global state that
/// could change between two files of the same run.
pub fn persist_artifact_file_in_roots(
    data_root: &Path,
    homeboy_run_id: &str,
    artifact_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let safe_name = sanitize_artifact_file_name(file_name);
    let target_dir = data_root.join("artifacts").join(homeboy_run_id);
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
    list_runs_cache_path_in_roots(&paths::homeboy()?, key, ext)
}

/// Compute (and ensure) a list-runs cache entry under an injected config root.
///
/// The body and the ETag of one cache entry are two files that must live beside
/// each other; resolving the root once per file let a 304 be validated against
/// an ETag from a different installation.
pub fn list_runs_cache_path_in_roots(config_root: &Path, key: &str, ext: &str) -> Result<PathBuf> {
    let base = config_root.join("cache").join("gh-actions-runs");
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
