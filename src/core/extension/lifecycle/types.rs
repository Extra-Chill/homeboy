//! types — extracted from lifecycle.rs.

use std::path::{Path, PathBuf};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use std::process::Command;
use super::super::manifest::ExtensionManifest;


#[derive(Debug, Clone)]
pub struct InstallResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UpdateAvailable {
    pub extension_id: String,
    pub installed_version: String,
    pub behind_count: usize,
}
