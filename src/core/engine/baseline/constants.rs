//! constants — extracted from baseline.rs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::error::{Error, Result};


pub(crate) const HOMEBOY_JSON: &str = "homeboy.json";

pub(crate) const BASELINES_KEY: &str = "baselines";
