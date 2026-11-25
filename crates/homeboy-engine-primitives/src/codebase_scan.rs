//! Generic codebase file walker.
//!
//! Provides shared infrastructure for walking source files across a codebase
//! with consistent skip-directory and extension filtering.
//!
//! Zero domain knowledge — all language-specific behavior comes from callers.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

// ============================================================================
// Skip directory configuration
// ============================================================================

/// Directories to always skip at any depth (VCS, dependencies, caches).
pub const ALWAYS_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    ".git",
    ".homeboy-build",
    ".homeboy-bin",
    ".svn",
    ".hg",
    "__pycache__",
];

/// Directories to skip only at root level (build output).
/// At deeper levels (e.g., `scripts/build/`) they may contain source files.
pub const ROOT_ONLY_SKIP_DIRS: &[&str] = &["artifacts", "build", "dist", "target", "cache", "tmp"];

/// Common source file extensions across languages.
pub const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "php", "js", "jsx", "ts", "tsx", "mjs", "json", "toml", "yaml", "yml", "md", "txt", "sh",
    "bash", "py", "rb", "go", "swift", "kt", "java", "c", "cpp", "h", "lock", "css", "scss",
    "sass", "less", "html", "vue", "svelte",
];

// ============================================================================
// Extension filter
// ============================================================================

/// Controls which file extensions are included in a scan.
#[derive(Debug, Clone, Default)]
pub enum ExtensionFilter {
    /// Include all files regardless of extension.
    All,
    /// Include only files with these extensions.
    Only(Vec<String>),
    /// Include all files except those with these extensions.
    Except(Vec<String>),
    /// Use the default SOURCE_EXTENSIONS list.
    #[default]
    SourceDefaults,
}

// ============================================================================
// Scan configuration
// ============================================================================

/// Configuration for a codebase scan.
#[derive(Debug, Clone, Default)]
pub struct ScanConfig {
    /// Additional directories to always skip (merged with ALWAYS_SKIP_DIRS).
    pub extra_skip_dirs: Vec<String>,
    /// Additional directories to skip at root only (merged with ROOT_ONLY_SKIP_DIRS).
    pub extra_root_skip_dirs: Vec<String>,
    /// File extension filter.
    pub extensions: ExtensionFilter,
    /// Whether to skip hidden files/directories (names starting with `.`).
    /// VCS dirs (.git, .svn, .hg) are always skipped regardless of this setting.
    pub skip_hidden: bool,
}

// ============================================================================
// File walking
// ============================================================================

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

fn walk_recursive(dir: &Path, root: &Path, config: &ScanConfig, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let is_root = dir == root;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path_name(&path);

        if entry_is_dir(&entry, &path) {
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

fn matches_extension(path: &Path, filter: &ExtensionFilter) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match filter {
        ExtensionFilter::All => true,
        ExtensionFilter::Only(exts) => exts.iter().any(|e| e.as_str() == ext),
        ExtensionFilter::Except(exts) => !exts.iter().any(|e| e.as_str() == ext),
        ExtensionFilter::SourceDefaults => SOURCE_EXTENSIONS.contains(&ext),
    }
}

// ============================================================================
// Callback-based walking
// ============================================================================

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

fn walk_recursive_with<F>(dir: &Path, root: &Path, config: &ScanConfig, callback: &mut F)
where
    F: FnMut(&Path),
{
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let is_root = dir == root;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path_name(&path);

        if entry_is_dir(&entry, &path) {
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

// ============================================================================
// Shared skip-dir logic
// ============================================================================

fn path_name(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or(Cow::Borrowed(""))
}

fn entry_is_dir(entry: &std::fs::DirEntry, path: &Path) -> bool {
    match entry.file_type() {
        Ok(file_type) => file_type.is_dir() || (file_type.is_symlink() && path.is_dir()),
        Err(_) => path.is_dir(),
    }
}

/// Check if a directory should be skipped based on config.
///
/// Centralizes the skip-dir decision for all walker variants.
fn should_skip_dir(name: &str, is_root: bool, config: &ScanConfig) -> bool {
    // Skip hidden directories if configured
    if config.skip_hidden && name.starts_with('.') {
        return true;
    }

    // Always skip VCS/dependency dirs at any depth
    if ALWAYS_SKIP_DIRS.contains(&name) {
        return true;
    }
    if config.extra_skip_dirs.iter().any(|d| d.as_str() == name) {
        return true;
    }

    // Skip build output dirs only at root level
    if is_root {
        if ROOT_ONLY_SKIP_DIRS.contains(&name) {
            return true;
        }
        if config
            .extra_root_skip_dirs
            .iter()
            .any(|d| d.as_str() == name)
        {
            return true;
        }
    }

    false
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(root: &Path, config: &ScanConfig) -> Vec<String> {
        let mut files: Vec<String> = walk_files(root, config)
            .iter()
            .map(|f| {
                f.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn walk_files_skips_dependency_dirs_everywhere_and_build_dirs_only_at_root() {
        let dir = tempfile::tempdir().expect("tmpdir");
        for file in [
            "node_modules/lib.js",
            "vendor/lib.php",
            ".homeboy-build/component/copied.rs",
            ".homeboy-bin/helper.rs",
            "src/__pycache__/module.py",
            "src/main.py",
            "build/output.rs",
            "scripts/build/setup.sh",
            "src/main.rs",
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }

        assert_eq!(
            scanned(dir.path(), &ScanConfig::default()),
            vec!["scripts/build/setup.sh", "src/main.py", "src/main.rs"]
        );
    }

    #[test]
    fn walk_files_applies_extension_and_hidden_filters() {
        let dir = tempfile::tempdir().expect("tmpdir");
        for file in [
            ".hidden/secret.rs",
            ".dotfile.rs",
            "main.rs",
            "style.css",
            "readme.md",
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }

        let config = ScanConfig {
            extensions: ExtensionFilter::Only(vec!["rs".to_string(), "md".to_string()]),
            skip_hidden: true,
            ..Default::default()
        };
        assert_eq!(scanned(dir.path(), &config), vec!["main.rs", "readme.md"]);
    }
}
