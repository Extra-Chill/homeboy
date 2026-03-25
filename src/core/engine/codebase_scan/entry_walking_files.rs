//! entry_walking_files — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};
use super::WalkEntry;
use super::ScanConfig;


/// Walk a directory tree and collect entries matching a name predicate.
///
/// Unlike `walk_files`, this can also collect directory paths. The `matcher`
/// receives the entry name and whether it's a directory, returning `true`
/// to include it in results.
pub fn walk_entries<F>(root: &Path, config: &ScanConfig, matcher: F) -> Vec<WalkEntry>
where
    F: Fn(&str, bool) -> bool,
{
    let mut results = Vec::new();
    walk_entries_recursive(root, root, config, &matcher, &mut results);
    results
}

pub(crate) fn walk_entries_recursive<F>(
    dir: &Path,
    root: &Path,
    config: &ScanConfig,
    matcher: &F,
    results: &mut Vec<WalkEntry>,
) where
    F: Fn(&str, bool) -> bool,
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
            if matcher(&name, true) {
                results.push(WalkEntry::Dir(path.clone()));
            }
            walk_entries_recursive(&path, root, config, matcher, results);
        } else {
            if config.skip_hidden && name.starts_with('.') {
                continue;
            }
            if matcher(&name, false) {
                results.push(WalkEntry::File(path.clone()));
            }
        }
    }
}
