//! search_early_return — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};
use super::ScanConfig;


/// Walk a directory tree and return `true` as soon as `predicate` matches a file.
///
/// Useful for existence checks — avoids scanning the entire tree when
/// only a yes/no answer is needed.
pub fn any_file_matches<F>(root: &Path, config: &ScanConfig, predicate: F) -> bool
where
    F: Fn(&Path) -> bool,
{
    any_file_matches_recursive(root, root, config, &predicate)
}

pub(crate) fn any_file_matches_recursive<F>(
    dir: &Path,
    root: &Path,
    config: &ScanConfig,
    predicate: &F,
) -> bool
where
    F: Fn(&Path) -> bool,
{
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
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
            if any_file_matches_recursive(&path, root, config, predicate) {
                return true;
            }
        } else {
            if config.skip_hidden && name.starts_with('.') {
                continue;
            }
            if matches_extension(&path, &config.extensions) && predicate(&path) {
                return true;
            }
        }
    }

    false
}
