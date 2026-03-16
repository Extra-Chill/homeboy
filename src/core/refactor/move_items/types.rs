//! types — extracted from move_items.rs.

use std::path::PathBuf;

use super::ItemKind;


/// Result of a move operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveResult {
    /// Items that were moved.
    pub items_moved: Vec<MovedItem>,
    /// The source file items were extracted from.
    pub from_file: String,
    /// The destination file items were moved to.
    pub to_file: String,
    /// Whether the destination file was created (vs. appended to).
    pub file_created: bool,
    /// Number of import references updated across the codebase.
    pub imports_updated: usize,
    /// Absolute paths of caller files whose imports were rewritten.
    /// Used by decompose rollback to restore these files if the move is reverted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caller_files_modified: Vec<PathBuf>,
    /// Related tests that were moved alongside items.
    pub tests_moved: Vec<MovedItem>,
    /// Whether changes were written to disk.
    pub applied: bool,
    /// Warnings generated during the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A single item that was moved.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MovedItem {
    /// Name of the item (function, struct, etc.).
    pub name: String,
    /// What kind of item.
    pub kind: ItemKind,
    /// Line range in the source file (1-indexed, inclusive).
    pub source_lines: (usize, usize),
    /// Number of lines (including doc comments and attributes).
    pub line_count: usize,
}

/// A submodule entry for module index generation.
#[derive(Debug, Clone)]
pub struct ModuleIndexEntry {
    /// Module name (e.g., "types", "unreleased").
    pub name: String,
    /// Public items that should be re-exported. Empty = glob re-export.
    pub pub_items: Vec<String>,
}

/// A single import rewrite in a caller file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportRewrite {
    /// Line number (1-indexed) in the file.
    pub line: usize,
    /// Original line text.
    pub original: String,
    /// Replacement line text.
    pub replacement: String,
}

/// Result of a whole-file move operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveFileResult {
    /// The source file that was moved.
    pub from_file: String,
    /// The destination file.
    pub to_file: String,
    /// Number of import references updated across the codebase.
    pub imports_updated: usize,
    /// Files whose imports were rewritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caller_files_modified: Vec<String>,
    /// Whether changes were written to disk.
    pub applied: bool,
    /// Warnings generated during the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Whether mod declarations were updated.
    pub mod_declarations_updated: bool,
}
