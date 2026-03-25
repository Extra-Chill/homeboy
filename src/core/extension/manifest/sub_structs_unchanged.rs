//! sub_structs_unchanged — extracted from manifest.rs.

use crate::config::ConfigEntity;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;


pub(crate) fn default_test_prefix() -> String {
    "test_".to_string()
}

pub(crate) fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}
