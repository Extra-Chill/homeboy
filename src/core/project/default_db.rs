//! default_db — extracted from mod.rs.

use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;


pub(crate) fn default_db_host() -> String {
    "localhost".to_string()
}

pub(crate) fn default_db_port() -> u16 {
    3306
}
