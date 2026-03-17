//! rename_scope — extracted from mod.rs.

use crate::core::refactor::*;


/// What scope to apply renames to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameScope {
    /// Source files only.
    Code,
    /// Config files only (homeboy.json, component configs).
    Config,
    /// Everything.
    All,
}
