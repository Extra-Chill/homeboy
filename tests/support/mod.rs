//! Shared scaffolding for integration tests.
//!
//! Centralizes temp-directory creation and fixture-file writes so individual
//! test files do not re-implement manual temp-dir naming or remember to clean
//! up after themselves. Backed by [`tempfile::TempDir`] for automatic removal
//! when the returned guard is dropped.

use std::path::Path;

use tempfile::TempDir;

/// Create an isolated, auto-cleaned temporary directory for a test.
///
/// The `prefix` keeps fixture directories human-recognizable on disk while a
/// debug session is paused; the underlying [`TempDir`] is removed when the
/// returned value is dropped, so tests no longer need an explicit
/// `fs::remove_dir_all` at the end.
pub fn temp_dir(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(&format!("homeboy-{prefix}-"))
        .tempdir()
        .expect("temp dir should be created")
}

/// Write a fixture file into `dir`, panicking with a descriptive message on
/// failure. Parent directories are created as needed so callers can write into
/// nested fixture layouts.
pub fn write_file(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!("failed to create fixture dir {}: {err}", parent.display())
        });
    }
    std::fs::write(&path, body)
        .unwrap_or_else(|err| panic!("failed to write fixture {}: {err}", path.display()));
}
