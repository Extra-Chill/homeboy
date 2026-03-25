//! callback_based_walking — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};
use super::matches_extension;
use super::ScanConfig;
use super::should_skip_dir;


/// Walk a directory tree and call `callback` for each matching file.
///
/// Same skip logic as `walk_files`, but avoids collecting into a Vec
/// when the caller only needs to process files one at a time.
pub fn walk_files_with<F>(root: &Path, config: &ScanConfig, callback: &mut F)
where
    F: FnMut(&Path),
{
    walk_recursive_with(root, root, config, callback);
}

pub(crate) fn walk_recursive_with<F>(dir: &Path, root: &Path, config: &ScanConfig, callback: &mut F)
where
    F: FnMut(&Path),
{
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let is_root = dir == root;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if path.is_dir() {
            if should_skip_dir(&name, is_root, config) {
                continue;
            }
            walk_recursive_with(&path, root, config, callback);
        } else {
            if config.skip_hidden && name.starts_with('.') {
                continue;
            }
            if matches_extension(&path, &config.extensions) {
                callback(&path);
            }
        }
    }
}
