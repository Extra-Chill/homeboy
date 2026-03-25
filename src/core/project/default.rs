//! default — extracted from mod.rs.

use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;


pub(crate) fn default_tail_lines() -> u32 {
    100
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_post_method() -> String {
    "POST".to_string()
}
