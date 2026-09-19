use crate::error::Error;
use crate::Result;
use serde_json::Value;

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

/// Look up a value at a JSON pointer without creating intermediate objects.
///
/// `None` means the pointer is well-formed but does not exist in `root`.
pub fn get_json_pointer<'a>(root: &'a Value, pointer: &str) -> Result<Option<&'a Value>> {
    let pointer = normalize_pointer(pointer)?;
    if pointer.is_empty() {
        return Ok(Some(root));
    }

    let mut current = root;
    for token in pointer.split('/').skip(1).map(unescape_token) {
        current = match current {
            Value::Object(map) => match map.get(&token) {
                Some(value) => value,
                None => return Ok(None),
            },
            Value::Array(array) => match token.parse::<usize>() {
                Ok(index) => match array.get(index) {
                    Some(value) => value,
                    None => return Ok(None),
                },
                Err(_) => return Ok(None),
            },
            _ => return Ok(None),
        };
    }

    Ok(Some(current))
}

/// Navigate to the value at a JSON pointer without creating intermediate objects.
/// Returns an error if any segment along the path is missing.
fn navigate_pointer<'a>(root: &'a mut Value, pointer: &str) -> Result<&'a mut Value> {
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

fn remove_child(parent: &mut Value, token: &str) -> Result<()> {
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

/// Canonical JSON pointer for a config selector.
///
/// Operators address config as dotted paths (`retention.limit`) or JSON
/// pointers (`/retention/limit`). Both spellings resolve to the same pointer.
pub fn config_pointer(selector: &str) -> Result<String> {
    normalize_pointer(selector)
}

fn normalize_pointer(pointer: &str) -> Result<String> {
    let pointer = pointer.trim();
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

    if pointer.starts_with('/') {
        return Ok(pointer.to_string());
    }

    dotted_path_to_pointer(pointer)
}

fn dotted_path_to_pointer(path: &str) -> Result<String> {
    if path.starts_with('.') || path.ends_with('.') || path.contains("..") {
        return Err(Error::validation_invalid_argument(
            "pointer",
            "Dotted config path must be a non-empty sequence of keys, such as retention.limit",
            Some(path.to_string()),
            Some(vec![
                "Use a dotted path such as retention, or a JSON pointer such as /retention."
                    .to_string(),
            ]),
        ));
    }

    Ok(format!(
        "/{}",
        path.split('.')
            .map(|segment| segment.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/")
    ))
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

fn set_child(parent: &mut Value, token: &str, value: Value) -> Result<()> {
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

fn parse_array_index(token: &str) -> Result<usize> {
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

pub fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dotted_paths_select_the_same_subtree_as_json_pointers() {
        let config = json!({
            "retention": {
                "reconstructable_artifact_reserve_bytes": 20
            }
        });

        let dotted = get_json_pointer(&config, "retention").expect("dotted path");
        let pointer = get_json_pointer(&config, "/retention").expect("json pointer");
        assert_eq!(dotted, pointer);
        assert_eq!(
            get_json_pointer(&config, "retention.reconstructable_artifact_reserve_bytes")
                .expect("nested dotted path"),
            Some(&json!(20))
        );
    }

    #[test]
    fn dotted_paths_canonicalize_to_json_pointers() {
        assert_eq!(
            config_pointer("retention").expect("top-level path"),
            "/retention"
        );
        assert_eq!(
            config_pointer("retention.limit").expect("nested path"),
            "/retention/limit"
        );
        assert_eq!(
            config_pointer("/retention/limit").expect("pointer is unchanged"),
            "/retention/limit"
        );
    }

    #[test]
    fn empty_dotted_segments_are_rejected() {
        assert!(config_pointer("retention.").is_err());
        assert!(config_pointer(".retention").is_err());
        assert!(config_pointer("retention..limit").is_err());
    }
}
