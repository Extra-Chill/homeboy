//! json_parsing_utilities — extracted from config.rs.

use crate::engine::local_files::{self, FileSystem};
use crate::error::Error;
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::Result;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use heck::ToSnakeCase;
use std::io::Read;
use super::BulkIdsInput;


/// Parse JSON string into typed value.
pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse json".to_string()), None))
}

/// Serialize value to pretty-printed JSON string.
pub fn to_string_pretty<T: Serialize>(data: &T) -> Result<String> {
    serde_json::to_string_pretty(data)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))
}

/// Serialize value to compact JSON string with proper error handling.
pub fn to_json_string<T: Serialize>(data: &T) -> Result<String> {
    serde_json::to_string(data)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))
}

/// Serialize an entity to JSON and inject an `id` field.
///
/// Many entities use `#[serde(skip_serializing)]` on their `id` field, but
/// `create_single_from_json()` requires the id to be present. This helper
/// serializes the entity, injects the id, then returns a compact JSON string.
pub fn serialize_with_id<T: Serialize>(entity: &T, id: &str) -> Result<String> {
    let mut value = serde_json::to_value(entity)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize entity".to_string())))?;
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    }
    to_json_string(&value)
}

/// Read JSON spec from string, file (@path), or stdin (-).
pub fn read_json_spec_to_string(spec: &str) -> Result<String> {
    use std::io::IsTerminal;

    if spec.trim() == "-" {
        let mut buf = String::new();
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err(Error::validation_invalid_argument(
                "json",
                "Cannot read JSON from stdin when stdin is a TTY",
                None,
                None,
            ));
        }
        stdin
            .read_to_string(&mut buf)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read stdin".to_string())))?;
        return Ok(buf);
    }

    if let Some(path) = spec.strip_prefix('@') {
        if path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "json",
                "Invalid JSON spec '@' (missing file path)",
                None,
                None,
            ));
        }

        return local_files::local().read(Path::new(path));
    }

    Ok(spec.to_string())
}

/// Detect if input is JSON object (starts with '{').
pub fn is_json_input(input: &str) -> bool {
    input.trim_start().starts_with('{')
}

/// Detect if input is JSON array (starts with '[').
pub fn is_json_array(input: &str) -> bool {
    input.trim_start().starts_with('[')
}

/// Parse JSON spec into a BulkIdsInput.
pub fn parse_bulk_ids(json_spec: &str) -> Result<BulkIdsInput> {
    let raw = read_json_spec_to_string(json_spec)?;
    if let Ok(ids) = serde_json::from_str::<Vec<String>>(&raw) {
        return Ok(BulkIdsInput { component_ids: ids });
    }

    serde_json::from_str::<BulkIdsInput>(&raw)
        .map_err(|e| {
            Error::validation_invalid_json(
                e,
                Some("parse bulk IDs input".to_string()),
                Some(raw.chars().take(200).collect::<String>()),
            )
        })
        .map_err(|err| {
            err.with_hint(
                "Expected JSON array: '[\"component-a\",\"component-b\"]' OR JSON object: '{\"component_ids\":[\"component-a\",\"component-b\"]}'",
            )
        })
}
