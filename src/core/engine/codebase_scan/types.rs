//! types — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};


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

/// A filesystem entry found during walking.
#[derive(Debug, Clone)]
pub enum WalkEntry {
    File(PathBuf),
    Dir(PathBuf),
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
