//! baseline_config — extracted from baseline.rs.

use std::path::{Path, PathBuf};
use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::error::{Error, Result};


pub struct BaselineConfig {
    root: PathBuf,
    key: String,
}
