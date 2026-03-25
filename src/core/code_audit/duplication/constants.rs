//! constants — extracted from duplication.rs.

use std::collections::HashMap;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;


/// Minimum number of locations for a function to count as duplicated.
pub(crate) const MIN_DUPLICATE_LOCATIONS: usize = 2;

/// A group of files containing an identical function.
///
/// The fixer uses this to keep the canonical copy and remove the rest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DuplicateGroup {
    /// The duplicated function name.
    pub function_name: String,
    /// File chosen to keep the function (canonical location).
    pub canonical_file: String,
    /// Files where the duplicate should be removed and replaced with an import.
    pub remove_from: Vec<String>,
}

/// Names that are too generic to flag as near-duplicates.
/// These appear in many files with completely unrelated implementations.
pub(crate) const GENERIC_NAMES: &[&str] = &[
    "run", "new", "default", "build", "list", "show", "set", "get", "delete", "remove", "clear",
    "create", "update", "status", "search", "find", "read", "write", "rename", "init", "test",
    "fmt", "from", "into", "clone", "drop", "display", "parse", "validate", "execute", "handle",
    "process", "merge", "resolve", "pin", "plan",
];

/// Minimum body line count — skip trivial functions (1-2 line bodies).
/// Functions like `fn default_true() -> bool { true }` are too small
/// to meaningfully refactor into shared code with a parameter.
pub(crate) const MIN_BODY_LINES: usize = 3;

/// Minimum number of non-blank, non-comment lines for a block to be
/// considered meaningful. Blocks shorter than this are too trivial to flag.
pub(crate) const MIN_INTRA_BLOCK_LINES: usize = 5;
