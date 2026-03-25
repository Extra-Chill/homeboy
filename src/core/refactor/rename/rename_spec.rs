//! rename_spec — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::core::refactor::rename::CaseVariant;
use crate::core::refactor::rename::RenameScope;
use crate::core::refactor::rename::RenameContext;


/// A rename specification with all generated case variants.
#[derive(Debug, Clone)]
pub struct RenameSpec {
    pub from: String,
    pub to: String,
    pub scope: RenameScope,
    pub variants: Vec<CaseVariant>,
    /// When true, use exact string matching (no boundary detection).
    pub literal: bool,
    /// Syntactic context filter — restricts which occurrences get renamed.
    pub rename_context: RenameContext,
}
