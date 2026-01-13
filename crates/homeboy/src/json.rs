use crate::error::{Error, Result};
use crate::local_files::{self, FileSystem};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

// === Pure Formatting Functions ===

/// Parse JSON string into typed value
pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse json".to_string())))
}

/// Serialize value to pretty-printed JSON string
pub fn to_string_pretty<T: Serialize>(data: &T) -> Result<String> {
    serde_json::to_string_pretty(data)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))
}

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

// === JSON Pointer Operations ===

pub fn set_json_pointer(root: &mut Value, pointer: &str, new_value: Value) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        *root = new_value;
        return Ok(());
    };

    let parent = ensure_pointer_container(root, &parent_ptr)?;
    set_child(parent, &token, new_value)
}

fn normalize_pointer(pointer: &str) -> Result<String> {
    if pointer.is_empty() {
        return Ok(String::new());
    }

    if pointer == "/" {
        return Err(Error::validation_invalid_argument(
            "pointer",
            "Invalid JSON pointer '/'",
            None,
            None,
        ));
    }

    if !pointer.starts_with('/') {
        return Err(Error::validation_invalid_argument(
            "pointer",
            format!("JSON pointer must start with '/': {}", pointer),
            None,
            None,
        ));
    }

    Ok(pointer.to_string())
}

fn split_parent_pointer(pointer: &str) -> Option<(String, String)> {
    if pointer.is_empty() {
        return None;
    }

    let mut parts = pointer.rsplitn(2, '/');
    let token = parts.next()?.to_string();
    let parent = parts.next().unwrap_or("");

    let parent_ptr = if parent.is_empty() {
        String::new()
    } else {
        parent.to_string()
    };

    Some((parent_ptr, unescape_token(&token)))
}

fn ensure_pointer_container<'a>(root: &'a mut Value, pointer: &str) -> Result<&'a mut Value> {
    if pointer.is_empty() {
        return Ok(root);
    }

    let tokens: Vec<String> = pointer.split('/').skip(1).map(unescape_token).collect();

    let mut current = root;

    for token in tokens {
        let next = match current {
            Value::Object(map) => map
                .entry(token)
                .or_insert_with(|| Value::Object(serde_json::Map::new())),
            Value::Null => {
                *current = Value::Object(serde_json::Map::new());
                if let Value::Object(map) = current {
                    map.entry(token)
                        .or_insert_with(|| Value::Object(serde_json::Map::new()))
                } else {
                    unreachable!()
                }
            }
            Value::Array(arr) => {
                let index = parse_index(&token)?;
                if index >= arr.len() {
                    return Err(Error::config_invalid_value(
                        pointer,
                        None,
                        "Array index out of bounds while creating path",
                    ));
                }
                &mut arr[index]
            }
            _ => {
                return Err(Error::config_invalid_value(
                    pointer,
                    Some(value_type_name(current).to_string()),
                    "Expected object/array at pointer",
                ))
            }
        };

        current = next;
    }

    Ok(current)
}

fn set_child(parent: &mut Value, token: &str, value: Value) -> Result<()> {
    match parent {
        Value::Object(map) => {
            map.insert(token.to_string(), value);
            Ok(())
        }
        Value::Array(arr) => {
            let index = parse_index(token)?;
            if index >= arr.len() {
                return Err(Error::config_invalid_value(
                    "arrayIndex",
                    Some(index.to_string()),
                    "Array index out of bounds",
                ));
            }
            arr[index] = value;
            Ok(())
        }
        _ => Err(Error::config_invalid_value(
            "jsonPointer",
            Some(value_type_name(parent).to_string()),
            "Cannot set child on non-container",
        )),
    }
}

fn parse_index(token: &str) -> Result<usize> {
    token.parse::<usize>().map_err(|_| {
        Error::validation_invalid_argument(
            "arrayIndex",
            "Invalid array index token",
            Some(token.to_string()),
            None,
        )
    })
}

fn unescape_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// === Bulk Operations ===

/// Detect if input is JSON (starts with '{') or a plain ID
pub fn is_json_input(input: &str) -> bool {
    input.trim_start().starts_with('{')
}

/// Standardized bulk execution result
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkResult<T: Serialize> {
    pub action: String,
    pub results: Vec<ItemOutcome<T>>,
    pub summary: BulkSummary,
}

/// Outcome for a single item in a bulk operation
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemOutcome<T: Serialize> {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Summary of bulk operation results
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

/// Simple bulk input with just component IDs
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdsInput {
    pub component_ids: Vec<String>,
}

/// Parse JSON spec into a BulkIdsInput
pub fn parse_bulk_ids(json_spec: &str) -> Result<BulkIdsInput> {
    let raw = read_json_spec_to_string(json_spec)?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse bulk IDs input".to_string())))
}

// === Config Merge Operations ===

/// Result of a config merge operation
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeResult {
    pub updated_fields: Vec<String>,
}

/// Merge a JSON patch into any serializable config type.
pub(crate) fn merge_config<T: Serialize + DeserializeOwned>(
    existing: &mut T,
    patch: Value,
) -> Result<MergeResult> {
    let patch_obj = match &patch {
        Value::Object(obj) => obj,
        _ => {
            return Err(Error::validation_invalid_argument(
                "merge",
                "Merge patch must be a JSON object",
                None,
                None,
            ))
        }
    };

    let updated_fields: Vec<String> = patch_obj.keys().cloned().collect();

    if updated_fields.is_empty() {
        return Err(Error::validation_invalid_argument(
            "merge",
            "Merge patch cannot be empty",
            None,
            None,
        ));
    }

    let mut base = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;

    deep_merge(&mut base, patch);

    *existing = serde_json::from_value(base)
        .map_err(|e| Error::validation_invalid_json(e, Some("merge config".to_string())))?;

    Ok(MergeResult { updated_fields })
}

fn deep_merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base_obj), Value::Object(patch_obj)) => {
            for (key, value) in patch_obj {
                if value.is_null() {
                    base_obj.remove(&key);
                } else {
                    deep_merge(base_obj.entry(key).or_insert(Value::Null), value);
                }
            }
        }
        (base, patch) => *base = patch,
    }
}
