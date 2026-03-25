//! pinned_remote_log — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use std::collections::HashMap;
use std::path::PathBuf;


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]

pub struct PinnedRemoteLog {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_tail_lines")]
    pub tail_lines: u32,
}
