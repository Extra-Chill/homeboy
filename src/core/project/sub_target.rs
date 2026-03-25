//! sub_target — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use std::collections::HashMap;
use std::path::PathBuf;


#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct SubTarget {
    pub name: String,
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i32>,
    #[serde(default)]
    pub is_default: bool,
}
