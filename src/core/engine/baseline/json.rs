//! json — extracted from baseline.rs.

use std::path::{Path, PathBuf};
use serde_json::Value;
use crate::error::{Error, Result};
use std::collections::HashSet;
use serde::{Deserialize, Serialize};


pub(crate) fn read_json_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }

    let content = std::fs::read_to_string(path).map_err(|error| {
        Error::internal_io(
            format!("Failed to read {}: {}", path.display(), error),
            Some("baseline.read_json".to_string()),
        )
    })?;

    if content.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }

    serde_json::from_str(&content).map_err(|error| {
        Error::internal_io(
            format!("Failed to parse {}: {}", path.display(), error),
            Some("baseline.read_json".to_string()),
        )
    })
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value).map_err(|error| {
        Error::internal_io(
            format!("Failed to serialize {}: {}", path.display(), error),
            Some("baseline.write_json".to_string()),
        )
    })?;

    std::fs::write(path, content).map_err(|error| {
        Error::internal_io(
            format!("Failed to write {}: {}", path.display(), error),
            Some("baseline.write_json".to_string()),
        )
    })
}
