use crate::{Error, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpPayload<T> {
    pub op: String,
    pub data: T,
}

pub fn read_json_spec_to_string(spec: &str) -> Result<String> {
    if spec.trim() == "-" {
        let mut buf = String::new();
        std::io::stdin()
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

        return fs::read_to_string(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read json file spec '{}'", path)),
            )
        });
    }

    Ok(spec.to_string())
}

pub fn load_op_data<T: DeserializeOwned>(spec: &str, expected_op: &str) -> Result<T> {
    let raw = read_json_spec_to_string(spec)?;

    let payload: OpPayload<T> = serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse op payload".to_string())))?;

    if payload.op != expected_op {
        return Err(Error::validation_invalid_argument(
            "op",
            format!("Unexpected op '{}'", payload.op),
            Some(expected_op.to_string()),
            Some(vec![expected_op.to_string()]),
        ));
    }

    Ok(payload.data)
}

pub fn read_json_file(path: impl AsRef<Path>) -> Result<Value> {
    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read json file".to_string())))?;
    serde_json::from_str(&content)
        .map_err(|e| Error::internal_json(e.to_string(), Some("parse json file".to_string())))
}

pub fn read_json_file_typed<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read json file".to_string())))?;
    serde_json::from_str(&content)
        .map_err(|e| Error::internal_json(e.to_string(), Some("parse json file".to_string())))
}

pub fn write_json_file_pretty(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    let path = path.as_ref();
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))?;
    write_file_atomic(path, content.as_bytes())
}

pub fn write_json_file_pretty_typed<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    let path = path.as_ref();
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize json".to_string())))?;
    write_file_atomic(path, content.as_bytes())
}

pub fn scan_json_dir<T: DeserializeOwned>(dir: impl AsRef<Path>) -> Vec<(PathBuf, T)> {
    let dir = dir.as_ref();
    if !dir.exists() {
        return Vec::new();
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "json") {
                return None;
            }
            let content = fs::read_to_string(&path).ok()?;
            let parsed: T = serde_json::from_str(&content).ok()?;
            Some((path, parsed))
        })
        .collect()
}

pub fn set_json_pointer(root: &mut Value, pointer: &str, new_value: Value) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        *root = new_value;
        return Ok(());
    };

    let parent = ensure_pointer_container(root, &parent_ptr)?;
    set_child(parent, &token, new_value)
}

pub fn remove_json_pointer(root: &mut Value, pointer: &str) -> Result<()> {
    let pointer = normalize_pointer(pointer)?;
    let Some((parent_ptr, token)) = split_parent_pointer(&pointer) else {
        return Err(Error::validation_invalid_argument(
            "pointer",
            "Cannot remove the root JSON value",
            None,
            None,
        ));
    };

    let Some(parent) = root.pointer_mut(&parent_ptr) else {
        return Err(Error::validation_invalid_argument(
            "pointer",
            format!("JSON pointer parent path not found: {}", parent_ptr),
            None,
            None,
        ));
    };

    remove_child(parent, &token)
}

pub fn set_json_value_in_file(
    path: impl AsRef<Path>,
    pointer: &str,
    new_value: Value,
) -> Result<()> {
    let path = path.as_ref();
    let mut json = read_json_file(path)?;
    set_json_pointer(&mut json, pointer, new_value)?;
    write_json_file_pretty(path, &json)
}

fn write_file_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::validation_invalid_argument(
            "path",
            format!("Invalid path: {}", path.display()),
            None,
            None,
        )
    })?;

    let filename = path.file_name().ok_or_else(|| {
        Error::validation_invalid_argument(
            "path",
            format!("Invalid path: {}", path.display()),
            None,
            None,
        )
    })?;

    let tmp_path: PathBuf = parent.join(format!("{}.tmp", filename.to_string_lossy()));

    fs::write(&tmp_path, content)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write tmp json".to_string())))?;
    fs::rename(&tmp_path, path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("rename tmp json".to_string())))?;

    Ok(())
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

fn remove_child(parent: &mut Value, token: &str) -> Result<()> {
    match parent {
        Value::Object(map) => {
            map.remove(token);
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
            arr.remove(index);
            Ok(())
        }
        _ => Err(Error::config_invalid_value(
            "jsonPointer",
            Some(value_type_name(parent).to_string()),
            "Cannot remove child on non-container",
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
