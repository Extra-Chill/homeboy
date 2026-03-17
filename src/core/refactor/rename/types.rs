//! types — extracted from mod.rs.

use serde::Serialize;
use crate::core::refactor::*;


/// A case variant of a rename term.
#[derive(Debug, Clone, Serialize)]
pub struct CaseVariant {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// A single reference found in the codebase.
#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    /// File path relative to root.
    pub file: String,
    /// Line number (1-indexed).
    pub line: usize,
    /// Column number (1-indexed).
    pub column: usize,
    /// The matched text.
    pub matched: String,
    /// What it would be replaced with.
    pub replacement: String,
    /// The case variant label.
    pub variant: String,
    /// The full line content for context.
    pub context: String,
}

/// An edit to apply to a file's content.
#[derive(Debug, Clone, Serialize)]
pub struct FileEdit {
    /// File path relative to root.
    pub file: String,
    /// Number of replacements in this file.
    pub replacements: usize,
    /// New content after all replacements.
    #[serde(skip)]
    pub new_content: String,
}

/// A file or directory rename.
#[derive(Debug, Clone, Serialize)]
pub struct FileRename {
    /// Original path relative to root.
    pub from: String,
    /// New path relative to root.
    pub to: String,
}

/// A warning about a potential collision or issue.
#[derive(Debug, Clone, Serialize)]
pub struct RenameWarning {
    /// Warning category.
    pub kind: String,
    /// File path relative to root.
    pub file: String,
    /// Line number (if applicable).
    pub line: Option<usize>,
    /// Human-readable description.
    pub message: String,
}

/// The full result of a rename operation.
#[derive(Debug, Clone, Serialize)]
pub struct RenameResult {
    /// Case variants that were searched.
    pub variants: Vec<CaseVariant>,
    /// All references found.
    pub references: Vec<Reference>,
    /// File content edits to apply.
    pub edits: Vec<FileEdit>,
    /// File/directory renames to apply.
    pub file_renames: Vec<FileRename>,
    /// Warnings about potential collisions or issues.
    pub warnings: Vec<RenameWarning>,
    /// Total reference count.
    pub total_references: usize,
    /// Total files affected.
    pub total_files: usize,
    /// Whether changes were written to disk.
    pub applied: bool,
}
