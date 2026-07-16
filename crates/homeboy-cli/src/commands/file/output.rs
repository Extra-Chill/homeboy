use serde::Serialize;

use homeboy::core::project::files::{FileEntry, GrepMatch, LineChange};
use homeboy::core::server::transfer::TransferOutput;

#[derive(Serialize)]
pub struct FileOutput {
    pub(crate) command: String,
    pub(crate) project_id: String,
    pub(crate) base_path: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) old_path: Option<String>,
    pub(crate) new_path: Option<String>,
    pub(crate) recursive: Option<bool>,
    pub(crate) entries: Option<Vec<FileEntry>>,
    pub(crate) content: Option<String>,
    pub(crate) size: Option<i64>,
    pub(crate) bytes_written: Option<usize>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) action_required: Option<String>,
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
    pub(crate) exit_code: i32,
    pub(crate) success: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
pub struct FileFindOutput {
    pub(crate) command: String,
    pub(crate) project_id: String,
    pub(crate) base_path: Option<String>,
    pub(crate) path: String,
    pub(crate) pattern: Option<String>,
    pub(crate) matches: Vec<String>,
    pub(crate) match_count: usize,
}

#[derive(Serialize)]
pub struct FileGrepOutput {
    pub(crate) command: String,
    pub(crate) project_id: String,
    pub(crate) base_path: Option<String>,
    pub(crate) path: String,
    pub(crate) pattern: String,
    pub(crate) matches: Vec<GrepMatch>,
    pub(crate) match_count: usize,
}

#[derive(Serialize)]
pub struct FileEditOutput {
    pub(crate) command: String,
    pub(crate) project_id: String,
    pub(crate) base_path: Option<String>,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) dry_run: bool,
    pub(crate) changes_made: Vec<LineChange>,
    pub(crate) change_count: usize,
    pub(crate) success: bool,
    pub(crate) error: Option<String>,
}

#[derive(Serialize)]
pub struct FileDownloadOutput {
    pub(crate) command: String,
    pub(crate) project_id: String,
    pub(crate) remote_path: String,
    pub(crate) local_path: String,
    pub(crate) recursive: bool,
    pub(crate) success: bool,
    pub(crate) exit_code: i32,
    pub(crate) error: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum FileCommandOutput {
    Standard(FileOutput),
    Find(FileFindOutput),
    Grep(FileGrepOutput),
    Edit(FileEditOutput),
    Download(FileDownloadOutput),
    Transfer(TransferOutput),
    Raw(String),
}
