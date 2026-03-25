//! constants — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};


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
    "bash", "py", "rb", "go", "swift", "kt", "java", "c", "cpp", "h", "lock",
];
