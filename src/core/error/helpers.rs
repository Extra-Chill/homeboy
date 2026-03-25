//! helpers — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use serde_json::Value;


pub(crate) fn format_suggestions(suggestions: &[String]) -> String {
    if suggestions.len() == 1 {
        format!("Did you mean: {}?", suggestions[0])
    } else {
        format!("Did you mean: {}?", suggestions.join(", "))
    }
}

/// Serialize a details struct to JSON Value, falling back to empty object on failure.
pub(crate) fn to_details(details: impl Serialize) -> Value {
    serde_json::to_value(details).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}
