use crate::core::error::Error;
use crate::core::Result;
use heck::ToSnakeCase;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};

/// Normalize top-level JSON object keys to snake_case.
/// Allows callers to use camelCase, PascalCase, or snake_case interchangeably
/// for struct-field keys on the patch root (e.g. `componentOverrides` ->
/// `component_overrides`).
///
/// **Only the top level is normalized.** Nested keys are preserved verbatim
/// because they may be user-provided `HashMap<String, _>` lookup keys -
/// component IDs, hook names, variable names, etc. - where a hyphen, dot, or
/// mixed case is semantically meaningful and must not be rewritten. See
/// <https://github.com/Extra-Chill/homeboy/issues/1169> for the bug this guards
/// against.
///
/// If a nested struct field needs case flexibility, the caller should pass it
/// in its canonical `snake_case` form, or the target struct should declare a
/// `#[serde(rename = "...")]` attribute so serde handles the alias.
fn normalize_top_level_keys_to_snake_case(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let normalized: Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k.to_snake_case(), v))
                .collect();
            Value::Object(normalized)
        }
        other => other,
    }
}

/// Internal result from merge_config (no ID, caller adds it).
#[derive(Debug)]
pub(crate) struct MergeFields {
    pub updated_fields: Vec<String>,
}

/// Merge a JSON patch into any serializable config type.
pub(crate) fn merge_config<T: Serialize + DeserializeOwned>(
    existing: &mut T,
    patch: Value,
    replace_fields: &[String],
) -> Result<MergeFields> {
    // Normalize top-level keys to snake_case (accepts camelCase, PascalCase,
    // etc. for struct-field keys on the patch root). Nested keys are left
    // verbatim so HashMap lookup keys - component IDs, hook names, etc. -
    // survive intact. See issue #1169.
    let patch = normalize_top_level_keys_to_snake_case(patch);

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
    // non-trivial but still disappeared - those are truly unknown fields.
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
                return false; // Key survived - it's known
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

/// Internal result from remove_config (no ID, caller adds it).
pub(crate) struct RemoveFields {
    pub removed_from: Vec<String>,
}

/// Remove items from arrays in any serializable config type.
pub(crate) fn remove_config<T: Serialize + DeserializeOwned>(
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

fn deep_remove(base: &mut Value, spec: Value, removed_from: &mut Vec<String>, path: String) {
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
fn is_serialization_zero(val: &Value) -> bool {
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

fn should_replace(path: &str, replace_fields: &[String]) -> bool {
    replace_fields
        .iter()
        .any(|field| path == field || path.starts_with(&format!("{}.", field)))
}

fn deep_merge(base: &mut Value, patch: Value, replace_fields: &[String], path: String) {
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

fn array_union(base: &mut Vec<Value>, patch: Vec<Value>) {
    for item in patch {
        if !base.contains(&item) {
            base.push(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test struct mimicking Component's skip_serializing_if patterns.
    #[derive(Debug, Clone, Serialize, serde::Deserialize, Default)]
    struct TestConfig {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub tags: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub extensions: Option<std::collections::HashMap<String, serde_json::Value>>,
    }

    #[test]
    fn merge_config_rejects_unknown_fields() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({"extension": "wordpress"});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let problem = err.details["problem"].as_str().unwrap_or("");
        assert!(
            problem.contains("Unknown field(s)"),
            "Expected unknown field error, got: {}",
            problem
        );
        assert!(
            problem.contains("'extension'"),
            "Expected 'extension' in error, got: {}",
            problem
        );
    }

    #[test]
    fn merge_config_accepts_known_fields() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({"description": "hello"});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok());
        assert_eq!(config.description, Some("hello".to_string()));
    }

    #[test]
    fn merge_config_allows_zero_value_for_known_fields() {
        // Setting a known field to an empty/zero value should not be rejected,
        // even though skip_serializing_if will omit it from output.
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        // tags starts empty; patching with empty array is a valid zero-value
        // that skip_serializing_if would omit, but it's still a known field.
        let patch = serde_json::json!({"tags": []});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn merge_config_accepts_modules_plural() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({"extensions": {"wordpress": {}}});
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok());
        assert!(config.extensions.is_some());
    }

    /// Regression for #1169: `normalize_keys_to_snake_case` used to recurse
    /// into every object, rewriting `HashMap<String, _>` lookup keys that
    /// happened to contain hyphens or mixed case. The fix normalizes only the
    /// top level. Nested keys (including component IDs like `simple-dark-mode`)
    /// must now survive verbatim.
    #[test]
    fn merge_config_preserves_hyphenated_nested_keys() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        // Patch has a top-level camelCase key (should normalize to snake_case)
        // and a nested hyphenated key (must stay verbatim).
        let patch = serde_json::json!({
            "extensions": {
                "simple-dark-mode": {
                    "cli_path": "lando wp"
                }
            }
        });
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok(), "merge failed: {:?}", result.err());
        let ext = config.extensions.expect("extensions present");
        assert!(
            ext.contains_key("simple-dark-mode"),
            "hyphenated key was mangled; got keys: {:?}",
            ext.keys().collect::<Vec<_>>()
        );
        assert!(
            !ext.contains_key("simple_dark_mode"),
            "hyphenated key was silently rewritten to snake_case; got keys: {:?}",
            ext.keys().collect::<Vec<_>>()
        );
    }

    /// Top-level camelCase / PascalCase keys still normalize to snake_case so
    /// the existing UX (accepting `componentOverrides` as an alias for
    /// `component_overrides`) keeps working.
    #[test]
    fn merge_config_still_normalizes_top_level_camel_case() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        // Camel-cased top-level key for the `extensions` field.
        let patch = serde_json::json!({
            "Extensions": {
                "wordpress": {}
            }
        });
        let result = merge_config(&mut config, patch, &[]);
        assert!(
            result.is_ok(),
            "top-level PascalCase should still normalize; got: {:?}",
            result.err()
        );
        assert!(config.extensions.is_some());
    }

    /// Dots, spaces, and mixed case inside nested map keys must all be
    /// preserved. Covers hook names, variable names, and other user-defined
    /// identifiers that may legitimately contain characters that `to_snake_case`
    /// would mangle.
    #[test]
    fn merge_config_preserves_arbitrary_nested_key_shapes() {
        let mut config = TestConfig {
            name: "test".to_string(),
            ..Default::default()
        };
        let patch = serde_json::json!({
            "extensions": {
                "with.dot":       { "cli_path": "a" },
                "with space":     { "cli_path": "b" },
                "MixedCaseID":    { "cli_path": "c" },
                "kebab-case-id":  { "cli_path": "d" }
            }
        });
        let result = merge_config(&mut config, patch, &[]);
        assert!(result.is_ok(), "merge failed: {:?}", result.err());
        let ext = config.extensions.expect("extensions present");
        for key in ["with.dot", "with space", "MixedCaseID", "kebab-case-id"] {
            assert!(
                ext.contains_key(key),
                "nested key {:?} was mangled; got keys: {:?}",
                key,
                ext.keys().collect::<Vec<_>>()
            );
        }
    }
}
