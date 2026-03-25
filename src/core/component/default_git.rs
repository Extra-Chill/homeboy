//! default_git — extracted from mod.rs.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;


pub(crate) fn default_git_remote() -> String {
    "origin".to_string()
}

pub(crate) fn default_git_branch() -> String {
    "main".to_string()
}
