//! types — extracted from test.rs.

use serde::Serialize;
use std::collections::HashSet;
use regex::Regex;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};


/// A public method/function extracted from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedMethod {
    pub name: String,
    pub visibility: String,
    pub is_static: bool,
    pub line: usize,
    pub params: String,
}

/// Extracted class/struct info from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedClass {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub methods: Vec<ExtractedMethod>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldResult {
    pub source_file: String,
    pub test_file: String,
    pub stub_count: usize,
    pub content: String,
    pub written: bool,
    pub skipped: bool,
    pub classes: Vec<ExtractedClass>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldBatchResult {
    pub results: Vec<ScaffoldResult>,
    pub total_stubs: usize,
    pub total_written: usize,
    pub total_skipped: usize,
}

pub(crate) const MAX_AUTO_SCAFFOLD_STUBS: usize = 12;
