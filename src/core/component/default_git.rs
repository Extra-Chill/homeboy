//! default_git — extracted from mod.rs.

use std::collections::HashMap;
use serde::{Deserialize, Deserializer, Serialize};


/// Insert legacy commands into hooks map if the event key doesn't already exist.
pub(crate) fn merge_legacy_hook(hooks: &mut HashMap<String, Vec<String>>, event: &str, commands: Vec<String>) {
    if !commands.is_empty() && !hooks.contains_key(event) {
        hooks.insert(event.to_string(), commands);
    }
}

pub(crate) fn default_git_remote() -> String {
    "origin".to_string()
}

pub(crate) fn default_git_branch() -> String {
    "main".to_string()
}
