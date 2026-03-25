//! rename_scope — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


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
