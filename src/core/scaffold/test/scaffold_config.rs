//! scaffold_config — extracted from test.rs.

use std::collections::HashSet;
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};


/// Configuration for scaffold generation.
#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    pub base_class: String,
    pub base_class_import: String,
    pub test_prefix: String,
    pub incomplete_body: String,
    pub language: String,
}
