//! types — extracted from files.rs.

use serde::Serialize;
use crate::paths::{self as base_path, resolve_path_string};
use std::io::{self, Read};
use crate::error::{Error, Result};
use std::path::Path;
use std::process::Command;


#[derive(Debug, Clone, Serialize)]

pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: Option<i64>,
    pub permissions: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct ListResult {
    pub base_path: Option<String>,
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize)]

pub struct ReadResult {
    pub base_path: Option<String>,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct WriteResult {
    pub base_path: Option<String>,
    pub path: String,
    pub bytes_written: usize,
}

#[derive(Debug, Clone, Serialize)]

pub struct DeleteResult {
    pub base_path: Option<String>,
    pub path: String,
    pub recursive: bool,
}

#[derive(Debug, Clone, Serialize)]

pub struct RenameResult {
    pub base_path: Option<String>,
    pub old_path: String,
    pub new_path: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct FindResult {
    pub base_path: Option<String>,
    pub path: String,
    pub pattern: Option<String>,
    pub matches: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]

pub struct GrepMatch {
    pub file: String,
    pub line: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct GrepResult {
    pub base_path: Option<String>,
    pub path: String,
    pub pattern: String,
    pub matches: Vec<GrepMatch>,
}

#[derive(Debug, Clone, Serialize)]

pub struct EditResult {
    pub base_path: Option<String>,
    pub path: String,
    pub original_lines: Vec<String>,
    pub modified_lines: Vec<String>,
    pub changes_made: Vec<LineChange>,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]

pub struct LineChange {
    pub line_number: usize,
    pub original: String,
    pub modified: String,
    pub operation: String,
}

pub struct DownloadResult {
    pub remote_path: String,
    pub local_path: String,
    pub recursive: bool,
    pub success: bool,
    pub exit_code: i32,
    pub error: Option<String>,
}
