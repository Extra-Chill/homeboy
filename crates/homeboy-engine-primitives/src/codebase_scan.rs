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

    // --- File walking tests ---

    #[test]
    fn walk_files_skips_always_dirs() {
        let dir = std::env::temp_dir().join("homeboy_scan_always_skip_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("node_modules"));
        let _ = std::fs::create_dir_all(dir.join("vendor"));
        let _ = std::fs::create_dir_all(dir.join(".homeboy-build/component"));
        let _ = std::fs::create_dir_all(dir.join(".homeboy-bin"));
        let _ = std::fs::create_dir_all(dir.join("src"));

        std::fs::write(dir.join("node_modules/lib.js"), "x").unwrap();
        std::fs::write(dir.join("vendor/lib.php"), "x").unwrap();
        std::fs::write(dir.join(".homeboy-build/component/copied.rs"), "x").unwrap();
        std::fs::write(dir.join(".homeboy-bin/helper.rs"), "x").unwrap();
        std::fs::write(dir.join("src/main.rs"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.contains(&"lib.js".to_string()));
        assert!(!names.contains(&"lib.php".to_string()));
        assert!(!names.contains(&"copied.rs".to_string()));
        assert!(!names.contains(&"helper.rs".to_string()));
        // Homeboy keeps no state inside source trees, so it needs no skip rule.
        assert!(!ALWAYS_SKIP_DIRS.contains(&".homeboy"));
        assert!(!dir.join(".homeboy").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_root_only_skip_at_root() {
        let dir = std::env::temp_dir().join("homeboy_scan_root_skip_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("build"));
        let _ = std::fs::create_dir_all(dir.join("scripts/build"));

        std::fs::write(dir.join("build/output.rs"), "x").unwrap();
        std::fs::write(dir.join("scripts/build/setup.sh"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(
            names.contains(&"setup.sh".to_string()),
            "Should find scripts/build/setup.sh"
        );
        assert!(
            !names.contains(&"output.rs".to_string()),
            "Should skip root-level build/"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_extension_filter_only() {
        let dir = std::env::temp_dir().join("homeboy_scan_ext_only_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("main.rs"), "x").unwrap();
        std::fs::write(dir.join("style.css"), "x").unwrap();
        std::fs::write(dir.join("readme.md"), "x").unwrap();

        let config = ScanConfig {
            extensions: ExtensionFilter::Only(vec!["rs".to_string(), "md".to_string()]),
            ..Default::default()
        };
        let files = walk_files(&dir, &config);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"readme.md".to_string()));
        assert!(!names.contains(&"style.css".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_skip_hidden() {
        let dir = std::env::temp_dir().join("homeboy_scan_hidden_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join(".hidden"));

        std::fs::write(dir.join(".hidden/secret.rs"), "x").unwrap();
        std::fs::write(dir.join(".dotfile.rs"), "x").unwrap();
        std::fs::write(dir.join("visible.rs"), "x").unwrap();

        let config = ScanConfig {
            skip_hidden: true,
            ..Default::default()
        };
        let files = walk_files(&dir, &config);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"visible.rs".to_string()));
        assert!(!names.contains(&"secret.rs".to_string()));
        assert!(!names.contains(&".dotfile.rs".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_pycache_always_skipped() {
        let dir = std::env::temp_dir().join("homeboy_scan_pycache_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("src/__pycache__"));

        std::fs::write(dir.join("src/__pycache__/module.py"), "x").unwrap();
        std::fs::write(dir.join("src/main.py"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.py".to_string()));
        assert!(!names.contains(&"module.py".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
