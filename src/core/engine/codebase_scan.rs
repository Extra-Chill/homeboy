//! Generic codebase file walker and content search utility.
//!
//! Provides a shared infrastructure for walking source files and searching content
//! across the codebase. Used by refactor rename, code audit, docs audit, and others.
//!
//! Zero domain knowledge — all language-specific behavior comes from callers.

use std::path::{Path, PathBuf};

// ============================================================================
// Skip directory configuration
// ============================================================================

/// Directories to always skip at any depth (VCS, dependencies, caches).
pub const ALWAYS_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    ".git",
    ".svn",
    ".hg",
    "__pycache__",
];

/// Directories to skip only at root level (build output).
/// At deeper levels (e.g., `scripts/build/`) they may contain source files.
pub const ROOT_ONLY_SKIP_DIRS: &[&str] = &["build", "dist", "target", "cache", "tmp"];

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

// ============================================================================
// Search with early return
// ============================================================================

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

fn any_file_matches_recursive<F>(
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

// ============================================================================
// Entry walking (files + directories)
// ============================================================================

/// A filesystem entry found during walking.
#[derive(Debug, Clone)]
pub enum WalkEntry {
    File(PathBuf),
    Dir(PathBuf),
}

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

fn walk_entries_recursive<F>(
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

// ============================================================================
// Shared skip-dir logic
// ============================================================================

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
// Content search — boundary-aware matching
// ============================================================================

/// Check if a byte is a word boundary character (not alphanumeric, not underscore).
fn is_boundary_char(c: u8) -> bool {
    !c.is_ascii_alphanumeric() && c != b'_'
}

/// A match found in file content.
#[derive(Debug, Clone)]
pub struct Match {
    /// Relative file path from root.
    pub file: String,
    /// 1-indexed line number.
    pub line: usize,
    /// 1-indexed column (byte offset within line).
    pub column: usize,
    /// The actual text that matched.
    pub matched: String,
    /// The full line of text containing the match.
    pub context: String,
}

/// Find all occurrences of `term` in `text` at sensible word boundaries.
///
/// Boundary rules:
/// - Left: start of string, non-alphanumeric, underscore, or camelCase/acronym boundary
/// - Right: end of string, non-alphanumeric, underscore, or uppercase letter
///
/// Handles: word boundaries, camelCase joins, snake_case compounds, UPPER_SNAKE,
/// consecutive-uppercase acronym boundaries (WPAgent → WP|Agent).
pub fn find_boundary_matches(text: &str, term: &str) -> Vec<usize> {
    let text_bytes = text.as_bytes();
    let term_bytes = term.as_bytes();
    let term_len = term_bytes.len();
    let text_len = text_bytes.len();
    let mut matches = Vec::new();

    if term_len == 0 || term_len > text_len {
        return matches;
    }

    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        let end = abs + term_len;

        // Left boundary
        let left_ok = abs == 0
            || is_boundary_char(text_bytes[abs - 1])
            || text_bytes[abs - 1] == b'_'
            // camelCase boundary: lowercase/digit → uppercase
            || (text_bytes[abs].is_ascii_uppercase()
                && (text_bytes[abs - 1].is_ascii_lowercase()
                    || text_bytes[abs - 1].is_ascii_digit()))
            // Consecutive-uppercase boundary: uppercase → uppercase+lowercase
            // e.g., 'P' before 'A' in "WPAgent"
            || (abs >= 2
                && text_bytes[abs].is_ascii_uppercase()
                && text_bytes[abs - 1].is_ascii_uppercase()
                && term_len > 1
                && term_bytes[1].is_ascii_lowercase());

        // Right boundary
        let right_ok = end >= text_len || {
            let next = text_bytes[end];
            is_boundary_char(next) || next.is_ascii_uppercase() || next == b'_'
        };

        if left_ok && right_ok {
            matches.push(abs);
        }

        start = abs + 1;
    }

    matches
}

/// Find all occurrences of `term` in `text` using exact substring matching.
/// No boundary detection — every occurrence is returned.
pub fn find_literal_matches(text: &str, term: &str) -> Vec<usize> {
    let mut matches = Vec::new();
    let term_len = term.len();
    if term_len == 0 {
        return matches;
    }
    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        matches.push(abs);
        start = abs + 1;
    }
    matches
}

/// Find all occurrences of `term` in `text` using case-insensitive matching,
/// returning the actual text that was found (preserving original casing).
///
/// This is used for variant discovery — when a generated variant like `WpAgent`
/// has 0 matches, this function finds `WPAgent` (the actual casing in the codebase).
pub fn find_case_insensitive_matches(text: &str, term: &str) -> Vec<(usize, String)> {
    let text_lower = text.to_lowercase();
    let term_lower = term.to_lowercase();
    let term_len = term.len();
    let mut matches = Vec::new();

    if term_len == 0 || term_len > text.len() {
        return matches;
    }

    let mut start = 0;
    while let Some(pos) = text_lower[start..].find(&term_lower) {
        let abs = start + pos;
        let actual = &text[abs..abs + term_len];
        matches.push((abs, actual.to_string()));
        start = abs + 1;
    }

    matches
}

// ============================================================================
// High-level search: walk files + search content
// ============================================================================

/// Search mode for content scanning.
#[derive(Debug, Clone)]
pub enum SearchMode {
    /// Boundary-aware matching (respects word boundaries, camelCase, snake_case).
    Boundary,
    /// Exact substring matching (no boundary detection).
    Literal,
    /// Case-insensitive matching (returns actual casing found).
    CaseInsensitive,
}

/// Search for a term across all files in a directory tree.
///
/// Returns matches with file path, line number, column, matched text, and context.
pub fn search(root: &Path, term: &str, mode: &SearchMode, config: &ScanConfig) -> Vec<Match> {
    let files = walk_files(root, config);
    let mut results = Vec::new();

    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let relative = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        for (line_num, line) in content.lines().enumerate() {
            match mode {
                SearchMode::Boundary => {
                    for pos in find_boundary_matches(line, term) {
                        results.push(Match {
                            file: relative.clone(),
                            line: line_num + 1,
                            column: pos + 1,
                            matched: term.to_string(),
                            context: line.to_string(),
                        });
                    }
                }
                SearchMode::Literal => {
                    for pos in find_literal_matches(line, term) {
                        results.push(Match {
                            file: relative.clone(),
                            line: line_num + 1,
                            column: pos + 1,
                            matched: term.to_string(),
                            context: line.to_string(),
                        });
                    }
                }
                SearchMode::CaseInsensitive => {
                    for (pos, actual) in find_case_insensitive_matches(line, term) {
                        results.push(Match {
                            file: relative.clone(),
                            line: line_num + 1,
                            column: pos + 1,
                            matched: actual,
                            context: line.to_string(),
                        });
                    }
                }
            }
        }
    }

    results
}

/// Discover the actual casing of a term in the codebase.
///
/// Given a term like `WpAgent`, searches case-insensitively and returns
/// all distinct casings found (e.g., `WPAgent`). Useful for variant discovery
/// when a generated variant has 0 boundary matches.
pub fn discover_casing(root: &Path, term: &str, config: &ScanConfig) -> Vec<(String, usize)> {
    let matches = search(root, term, &SearchMode::CaseInsensitive, config);

    // Group by actual matched text, count occurrences
    let mut casing_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for m in &matches {
        *casing_counts.entry(m.matched.clone()).or_insert(0) += 1;
    }

    let mut result: Vec<(String, usize)> = casing_counts.into_iter().collect();
    result.sort_by(|a, b| b.1.cmp(&a.1)); // Most frequent first
    result
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
        let _ = std::fs::create_dir_all(dir.join("src"));

        std::fs::write(dir.join("node_modules/lib.js"), "x").unwrap();
        std::fs::write(dir.join("vendor/lib.php"), "x").unwrap();
        std::fs::write(dir.join("src/main.rs"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.contains(&"lib.js".to_string()));
        assert!(!names.contains(&"lib.php".to_string()));

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

    // --- Boundary matching tests ---

    #[test]
    fn boundary_matches_word_boundaries() {
        assert_eq!(find_boundary_matches("pub mod widget;", "widget"), vec![8]);
        assert_eq!(find_boundary_matches("load_widget", "widget"), vec![5]);
        assert_eq!(find_boundary_matches("WIDGET_DIR", "WIDGET"), vec![0]);
    }

    #[test]
    fn boundary_rejects_partial_match() {
        assert!(find_boundary_matches("widgetry", "widget").is_empty());
        assert!(find_boundary_matches("rewidget", "widget").is_empty());
    }

    #[test]
    fn boundary_matches_camel_case() {
        assert_eq!(find_boundary_matches("WidgetManifest", "Widget"), vec![0]);
        assert_eq!(find_boundary_matches("loadWidget", "Widget"), vec![4]);
    }

    #[test]
    fn boundary_matches_consecutive_uppercase() {
        // WPAgent → WP at 0, Agent at 2
        assert_eq!(find_boundary_matches("WPAgent", "Agent"), vec![2]);
        assert_eq!(find_boundary_matches("WPAgent", "WP"), vec![0]);
        assert_eq!(find_boundary_matches("XMLParser", "XML"), vec![0]);
        assert_eq!(find_boundary_matches("XMLParser", "Parser"), vec![3]);
    }

    // --- Literal matching tests ---

    #[test]
    fn literal_matches_exact_substring() {
        assert_eq!(find_literal_matches("the-widget-plugin", "widget"), vec![4]);
        // Literal matches inside words (no boundaries)
        assert_eq!(find_literal_matches("widgetry", "widget"), vec![0]);
    }

    // --- Case-insensitive matching tests ---

    #[test]
    fn case_insensitive_finds_different_casing() {
        let matches = find_case_insensitive_matches("namespace WPAgent;", "wpagent");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 10); // position
        assert_eq!(matches[0].1, "WPAgent"); // actual casing preserved
    }

    #[test]
    fn case_insensitive_finds_multiple_casings() {
        let text = "WPAgent and wpagent and Wpagent";
        let matches = find_case_insensitive_matches(text, "wpagent");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].1, "WPAgent");
        assert_eq!(matches[1].1, "wpagent");
        assert_eq!(matches[2].1, "Wpagent");
    }

    // --- discover_casing tests ---

    #[test]
    fn discover_casing_finds_actual_convention() {
        let dir = std::env::temp_dir().join("homeboy_scan_discover_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.php"),
            "namespace WPAgent;\nuse WPAgent\\Tools;\nclass WPAgent_Loader {}\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let casings = discover_casing(&dir, "wpagent", &config);

        assert!(!casings.is_empty());
        // Most frequent casing should be "WPAgent"
        assert_eq!(casings[0].0, "WPAgent");
        assert_eq!(casings[0].1, 3); // 3 occurrences

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- High-level search tests ---

    #[test]
    fn search_boundary_mode() {
        let dir = std::env::temp_dir().join("homeboy_scan_search_boundary_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "fn widget_init() {}\nlet widgetry = true;\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let results = search(&dir, "widget", &SearchMode::Boundary, &config);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line, 1);
        assert_eq!(results[0].matched, "widget");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_literal_mode() {
        let dir = std::env::temp_dir().join("homeboy_scan_search_literal_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "fn widget_init() {}\nlet widgetry = true;\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let results = search(&dir, "widget", &SearchMode::Literal, &config);

        // Literal matches both: widget in widget_init AND widget in widgetry
        assert_eq!(results.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_case_insensitive_mode() {
        let dir = std::env::temp_dir().join("homeboy_scan_search_ci_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.php"),
            "namespace WPAgent;\nconst WP_AGENT = true;\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let results = search(&dir, "wpagent", &SearchMode::CaseInsensitive, &config);

        assert_eq!(results.len(), 1); // Only "WPAgent" matches (WP_AGENT has an underscore)
        assert_eq!(results[0].matched, "WPAgent");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
