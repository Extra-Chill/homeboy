//! rename_targeting — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


/// Optional file-targeting controls for rename operations.
#[derive(Debug, Clone)]
pub struct RenameTargeting {
    /// Include only files matching at least one glob. Empty = include all.
    pub include_globs: Vec<String>,
    /// Exclude files matching any glob.
    pub exclude_globs: Vec<String>,
    /// Whether file/directory renames should be generated/applied.
    pub rename_files: bool,
}

impl Default for RenameTargeting {
    fn default() -> Self {
        Self {
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            rename_files: true,
        }
    }
}
