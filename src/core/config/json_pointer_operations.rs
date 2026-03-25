//! json_pointer_operations — extracted from config.rs.

use crate::error::Error;
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::Result;
use serde_json::{Map, Value};
use crate::engine::local_files::{self, FileSystem};
use heck::ToSnakeCase;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use super::ConfigEntity;


pub fn set_json_pointer(root: &mut Value, pointer: &str, new_value: Value) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        *root = new_value;
        return Ok(());
    };

    let parent = ensure_pointer_container(root, &parent_ptr)?;
    set_child(parent, &token, new_value)
}

/// Remove the value at a JSON pointer path.
pub fn remove_json_pointer(root: &mut Value, pointer: &str) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        return Err(Error::validation_invalid_argument(
            "pointer",
            "Cannot remove root element",
            None,
            None,
        ));
    };

    let parent = navigate_pointer(root, &parent_ptr)?;
    remove_child(parent, &token)
}

/// Navigate to the value at a JSON pointer without creating intermediate objects.
/// Returns an error if any segment along the path is missing.
pub(crate) fn navigate_pointer<'a>(root: &'a mut Value, pointer: &str) -> Result<&'a mut Value> {
    if pointer.is_empty() {
        return Ok(root);
    }

    let tokens: Vec<String> = pointer.split('/').skip(1).map(unescape_token).collect();
    let mut current = root;

    for token in &tokens {
        current = match current {
            Value::Object(map) => map.get_mut(token.as_str()).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "pointer",
                    format!("Key '{}' not found", token),
                    None,
                    None,
                )
            })?,
            Value::Array(arr) => {
                let index = parse_array_index(token)?;
                let len = arr.len();
                if index >= len {
                    return Err(Error::validation_invalid_argument(
                        "pointer",
                        format!("Array index {} out of bounds (length {})", index, len),
                        None,
                        None,
                    ));
                }
                &mut arr[index]
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "pointer",
                    format!("Cannot navigate through non-object at path: {}", pointer),
                    None,
                    None,
                ))
            }
        };
    }

    Ok(current)
}

pub(crate) fn remove_child(parent: &mut Value, token: &str) -> Result<()> {
    match parent {
        Value::Object(map) => {
            if map.remove(token).is_none() {
                return Err(Error::validation_invalid_argument(
                    "pointer",
                    format!("Key '{}' not found", token),
                    None,
                    None,
                ));
            }
            Ok(())
        }
        Value::Array(arr) => {
            let index = parse_array_index(token)?;
            if index >= arr.len() {
                return Err(Error::validation_invalid_argument(
                    "pointer",
                    format!("Array index {} out of bounds (length {})", index, arr.len()),
                    None,
                    None,
                ));
            }
            arr.remove(index);
            Ok(())
        }
        _ => Err(Error::validation_invalid_argument(
            "pointer",
            "Cannot remove from non-container type",
            None,
            None,
        )),
    }
}

pub(crate) fn normalize_pointer(pointer: &str) -> Result<String> {
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

pub(crate) fn split_parent_pointer(pointer: &str) -> Option<(String, String)> {
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

pub(crate) fn ensure_pointer_container<'a>(root: &'a mut Value, pointer: &str) -> Result<&'a mut Value> {
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
                let index = parse_array_index(&token)?;
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

pub(crate) fn set_child(parent: &mut Value, token: &str, value: Value) -> Result<()> {
    match parent {
        Value::Object(map) => {
            map.insert(token.to_string(), value);
            Ok(())
        }
        Value::Array(arr) => {
            let index = parse_array_index(token)?;
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

pub(crate) fn parse_array_index(token: &str) -> Result<usize> {
    token.parse::<usize>().map_err(|_| {
        Error::validation_invalid_argument(
            "arrayIndex",
            "Invalid array index token",
            Some(token.to_string()),
            None,
        )
    })
}

pub(crate) fn unescape_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

pub(crate) fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Unified merge that auto-detects single vs bulk operations.
/// Array input triggers batch merge, object input triggers single merge.
pub fn merge<T: ConfigEntity>(
    id: Option<&str>,
    json_spec: &str,
    replace_fields: &[String],
) -> Result<MergeOutput> {
    let raw = read_json_spec_to_string(json_spec)?;

    if is_json_array(&raw) {
        return Ok(MergeOutput::Bulk(merge_batch_from_json::<T>(&raw)?));
    }

    Ok(MergeOutput::Single(merge_from_json::<T>(
        id,
        &raw,
        replace_fields,
    )?))
}
