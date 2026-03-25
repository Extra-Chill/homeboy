//! types — extracted from file.rs.

use super::super::CmdResult;
use clap::{Args, Subcommand};
use homeboy::project::files::{self, FileEntry, GrepMatch, LineChange};
use serde::Serialize;

#[derive(Args)]
pub struct FileArgs {
    #[command(subcommand)]
    command: FileCommand,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum FileCommand {
    /// List directory contents
    List {
        /// Project ID
        project_id: String,
        /// Remote directory path
        path: String,
    },
    /// Read file content
    Read {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
        /// Output raw content only (no JSON wrapper)
        #[arg(long)]
        raw: bool,
    },
    /// Write content to file (from stdin)
    Write {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
    },
    /// Delete a file or directory
    Delete {
        /// Project ID
        project_id: String,
        /// Remote path to delete
        path: String,
        /// Delete directories recursively
        #[arg(short, long)]
        recursive: bool,
    },
    /// Rename or move a file
    Rename {
        /// Project ID
        project_id: String,
        /// Current path
        old_path: String,
        /// New path
        new_path: String,
    },
    /// Find files by name pattern
    Find {
        /// Project ID
        project_id: String,
        /// Directory path to search
        path: String,
        /// Filename pattern (glob, e.g., "*.php")
        #[arg(long)]
        name: Option<String>,
        /// File type: f (file), d (directory), l (symlink)
        #[arg(long, name = "type")]
        file_type: Option<String>,
        /// Maximum directory depth
        #[arg(long)]
        max_depth: Option<u32>,
    },
    /// Search file contents
    Grep {
        /// Project ID
        project_id: String,
        /// Directory path to search
        path: String,
        /// Search pattern
        pattern: String,
        /// Filter files by name pattern (e.g., "*.php")
        #[arg(long)]
        name: Option<String>,
        /// Maximum directory depth
        #[arg(long)]
        max_depth: Option<u32>,
        /// Case insensitive search
        #[arg(short = 'i', long)]
        ignore_case: bool,
    },
    /// Download a file or directory from remote server
    Download {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
        /// Local destination path (defaults to current directory)
        #[arg(default_value = ".")]
        local_path: String,
        /// Download directories recursively
        #[arg(short, long)]
        recursive: bool,
    },
    /// Edit file with line-based or pattern-based operations
    Edit(EditArgs),
}

#[derive(Args)]
pub(crate) struct EditArgs {
    /// Project ID
    project_id: String,
    /// Remote file path
    file_path: String,
    /// Show changes without applying
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Apply even if multiple pattern matches (warns by default)
    #[arg(short, long)]
    force: bool,
    #[command(flatten)]
    line_ops: LineOperations,
    #[command(flatten)]
    pattern_ops: PatternOperations,
    #[command(flatten)]
    file_mods: FileModifications,
}

#[derive(Args, Default)]
pub(crate) struct LineOperations {
    #[arg(long)]
    replace_line: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "replace_line")]
    replace_line_content: Option<String>,
    #[arg(long)]
    insert_after: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "insert_after")]
    insert_after_content: Option<String>,
    #[arg(long)]
    insert_before: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "insert_before")]
    insert_before_content: Option<String>,
    #[arg(long)]
    delete_line: Option<usize>,
    #[arg(long, value_names = ["START", "END"])]
    delete_lines: Option<Vec<usize>>,
}

#[derive(Args, Default)]
pub(crate) struct PatternOperations {
    #[arg(long, value_name = "PATTERN")]
    replace_pattern: Option<String>,
    #[arg(long, value_name = "CONTENT", requires = "replace_pattern")]
    replace_pattern_content: Option<String>,
    #[arg(long)]
    replace_all_pattern: Option<String>,
    #[arg(long, value_name = "CONTENT", requires = "replace_all_pattern")]
    replace_all_content: Option<String>,
    #[arg(long, value_name = "PATTERN")]
    delete_pattern: Option<String>,
}

#[derive(Args, Default)]
pub(crate) struct FileModifications {
    #[arg(long, value_name = "CONTENT")]
    append: Option<String>,
    #[arg(long, value_name = "CONTENT")]
    prepend: Option<String>,
}

#[derive(Serialize)]

pub struct FileOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: Option<String>,
    old_path: Option<String>,
    new_path: Option<String>,
    recursive: Option<bool>,
    entries: Option<Vec<FileEntry>>,
    content: Option<String>,
    bytes_written: Option<usize>,
    stdout: Option<String>,
    stderr: Option<String>,
    exit_code: i32,
    success: bool,
}

#[derive(Serialize)]

pub struct FileFindOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: String,
    pattern: Option<String>,
    matches: Vec<String>,
    match_count: usize,
}

#[derive(Serialize)]

pub struct FileGrepOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: String,
    pattern: String,
    matches: Vec<GrepMatch>,
    match_count: usize,
}

#[derive(Serialize)]

pub struct FileEditOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: String,
    changes_made: Vec<LineChange>,
    change_count: usize,
    success: bool,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct FileDownloadOutput {
    command: String,
    project_id: String,
    remote_path: String,
    local_path: String,
    recursive: bool,
    success: bool,
    exit_code: i32,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum FileCommandOutput {
    Standard(FileOutput),
    Find(FileFindOutput),
    Grep(FileGrepOutput),
    Edit(FileEditOutput),
    Download(FileDownloadOutput),
    Raw(String),
}
