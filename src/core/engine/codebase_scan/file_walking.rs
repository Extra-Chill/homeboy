//! file_walking — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};
use super::ExtensionFilter;
use super::ScanConfig;


/// Walk a directory tree and return matching file paths.
///
/// Uses two-tier skip logic:
/// - `ALWAYS_SKIP_DIRS` + `extra_skip_dirs` are skipped at any depth
/// - `ROOT_ONLY_SKIP_DIRS` + `extra_root_skip_dirs` are skipped only at root level
///
/// Files are filtered by the configured extension filter.
pub fn walk_files(root: &Path, config: &ScanConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_recursive(root, root, config, &mut files);
    files
}

pub(crate) fn walk_recursive(dir: &Path, root: &Path, config: &ScanConfig, files: &mut Vec<PathBuf>) {
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
            walk_recursive(&path, root, config, files);
        } else {
            if config.skip_hidden && name.starts_with('.') {
                continue;
            }

            if matches_extension(&path, &config.extensions) {
                files.push(path);
            }
        }
    }
}

pub(crate) fn matches_extension(path: &Path, filter: &ExtensionFilter) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match filter {
        ExtensionFilter::All => true,
        ExtensionFilter::Only(exts) => exts.iter().any(|e| e.as_str() == ext),
        ExtensionFilter::Except(exts) => !exts.iter().any(|e| e.as_str() == ext),
        ExtensionFilter::SourceDefaults => SOURCE_EXTENSIONS.contains(&ext),
    }
}
