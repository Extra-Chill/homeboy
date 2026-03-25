//! helpers — extracted from mod.rs.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;


/// Insert legacy commands into hooks map if the event key doesn't already exist.
pub(crate) fn merge_legacy_hook(hooks: &mut HashMap<String, Vec<String>>, event: &str, commands: Vec<String>) {
    if !commands.is_empty() && !hooks.contains_key(event) {
        hooks.insert(event.to_string(), commands);
    }
}

/// Normalize empty strings to None. Treats "", null, and field omission identically for consistent validation.
pub(crate) fn deserialize_empty_as_none<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}
