//! rename_context — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


/// Syntactic context filter for rename matches.
///
/// Restricts which occurrences of a term get renamed based on their
/// syntactic position in the source code. Useful for selective renames
/// where only certain usages should change (e.g., rename an array key
/// but not a variable with the same name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameContext {
    /// Only match inside string literals (`'term'`, `"term"`) and
    /// property access (`.term`, `->term`, `::term`).
    Key,
    /// Only match variable references (`$term` in PHP, standalone identifiers
    /// NOT inside strings or property access).
    Variable,
    /// Only match function parameter definitions (inside parentheses
    /// following a function/fn keyword).
    Parameter,
    /// Match everything — current default behavior.
    All,
}
