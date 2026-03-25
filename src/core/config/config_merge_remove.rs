//! config_merge_remove — extracted from config.rs.

use crate::error::Error;
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::Result;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use crate::engine::local_files::{self, FileSystem};
use heck::ToSnakeCase;
use std::io::Read;
use std::path::{Path, PathBuf};
use super::RemoveFields;
use super::MergeFields;


/// Normalize JSON object keys to snake_case recursively.
/// Allows callers to use camelCase, PascalCase, or snake_case interchangeably.
pub(crate) fn normalize_keys_to_snake_case(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let normalized: Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k.to_snake_case(), normalize_keys_to_snake_case(v)))
                .collect();
            Value::Object(normalized)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(normalize_keys_to_snake_case).collect())
        }
        other => other,
    }
}

/// Merge a JSON patch into any serializable config type.
pub fn merge_config<T: Serialize + DeserializeOwned>(
    existing: &mut T,
    patch: Value,
    replace_fields: &[String],
) -> Result<MergeFields> {
    // Normalize keys to snake_case (accepts camelCase, PascalCase, etc.)
    let patch = normalize_keys_to_snake_case(patch);

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

    // Detect unknown fields by round-tripping through the typed struct.
    // After deep-merging the patch into the serialized base, deserialize back
    // into T. Serde silently drops unknown keys. We detect this by comparing
    // the merged JSON against the re-serialized struct output.
    let mut base = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;

    // Snapshot patch values before merge (for zero-value detection)
    let patch_values: Map<String, Value> = patch.as_object().cloned().unwrap_or_default();

    deep_merge(&mut base, patch, replace_fields, String::new());

    *existing = serde_json::from_value(base)
        .map_err(|e| Error::validation_invalid_json(e, Some("merge config".to_string()), None))?;

    // Re-serialize and check which patch keys survived the round-trip.
    // Fields with skip_serializing_if may vanish when set to zero values
    // (empty vec, None, false), so we only flag keys whose patch value was
    // non-trivial but still disappeared — those are truly unknown fields.
    let after_roundtrip = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;
    let surviving_keys: std::collections::HashSet<String> = after_roundtrip
        .as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();

    let dropped: Vec<&String> = updated_fields
        .iter()
        .filter(|key| {
            if surviving_keys.contains(key.as_str()) {
                return false; // Key survived — it's known
            }
            // Key disappeared. Check if the patch value was a "zero value"
            // that skip_serializing_if would legitimately omit.
            match patch_values.get(key.as_str()) {
                None => false, // Shouldn't happen, but be safe
                Some(val) => !is_serialization_zero(val),
            }
        })
        .collect();

    if !dropped.is_empty() {
        let field_list = dropped
            .iter()
            .map(|k| format!("'{}'", k))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::validation_invalid_argument(
            "merge",
            format!(
                "Unknown field(s): {}. Check field names with the entity's config schema.",
                field_list
            ),
            None,
            None,
        ));
    }

    Ok(MergeFields { updated_fields })
}

/// Remove items from arrays in any serializable config type.
pub fn remove_config<T: Serialize + DeserializeOwned>(
    existing: &mut T,
    spec: Value,
) -> Result<RemoveFields> {
    let spec_obj = match &spec {
        Value::Object(obj) => obj,
        _ => {
            return Err(Error::validation_invalid_argument(
                "remove",
                "Remove spec must be a JSON object",
                None,
                None,
            ))
        }
    };

    let fields: Vec<String> = spec_obj.keys().cloned().collect();

    if fields.is_empty() {
        return Err(Error::validation_invalid_argument(
            "remove",
            "Remove spec cannot be empty",
            None,
            None,
        ));
    }

    let mut base = serde_json::to_value(&*existing)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize config".to_string())))?;

    let mut removed_from = Vec::new();
    deep_remove(&mut base, spec, &mut removed_from, String::new());

    *existing = serde_json::from_value(base)
        .map_err(|e| Error::validation_invalid_json(e, Some("remove config".to_string()), None))?;

    Ok(RemoveFields { removed_from })
}

pub(crate) fn deep_remove(base: &mut Value, spec: Value, removed_from: &mut Vec<String>, path: String) {
    match (base, spec) {
        (Value::Object(base_obj), Value::Object(spec_obj)) => {
            for (key, value) in spec_obj {
                let field_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };
                if let Some(base_value) = base_obj.get_mut(&key) {
                    deep_remove(base_value, value, removed_from, field_path);
                }
            }
        }
        (Value::Array(base_arr), Value::Array(spec_arr)) => {
            let original_len = base_arr.len();
            base_arr.retain(|item| !spec_arr.contains(item));
            if base_arr.len() < original_len {
                removed_from.push(path);
            }
        }
        _ => {}
    }
}

/// Returns true if a JSON value is a "zero value" that skip_serializing_if
/// would legitimately omit (empty array, empty string, null, false, 0).
pub(crate) fn is_serialization_zero(val: &Value) -> bool {
    match val {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(arr) => arr.is_empty(),
        Value::Object(obj) => obj.is_empty(),
        _ => false,
    }
}

/// Collect top-level array field names from a JSON object.
/// Used by `set` commands to auto-replace arrays instead of merging.
pub fn collect_array_fields(value: &Value) -> Vec<String> {
    match value {
        Value::Object(obj) => obj
            .iter()
            .filter(|(_, v)| v.is_array())
            .map(|(k, _)| k.clone())
            .collect(),
        _ => vec![],
    }
}

pub(crate) fn should_replace(path: &str, replace_fields: &[String]) -> bool {
    replace_fields
        .iter()
        .any(|field| path == field || path.starts_with(&format!("{}.", field)))
}

pub(crate) fn deep_merge(base: &mut Value, patch: Value, replace_fields: &[String], path: String) {
    match (base, patch) {
        (Value::Object(base_obj), Value::Object(patch_obj)) => {
            for (key, value) in patch_obj {
                let field_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };
                if value.is_null() {
                    base_obj.remove(&key);
                } else {
                    let entry = base_obj.entry(key).or_insert(Value::Null);
                    deep_merge(entry, value, replace_fields, field_path);
                }
            }
        }
        (Value::Array(base_arr), Value::Array(patch_arr)) => {
            if should_replace(&path, replace_fields) {
                *base_arr = patch_arr;
            } else {
                array_union(base_arr, patch_arr);
            }
        }
        (base, patch) => *base = patch,
    }
}

pub(crate) fn array_union(base: &mut Vec<Value>, patch: Vec<Value>) {
    for item in patch {
        if !base.contains(&item) {
            base.push(item);
        }
    }
}
