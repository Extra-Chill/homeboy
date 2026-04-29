use crate::component::{resolve_effective, Component};
use crate::error::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Read a `homeboy.json` portable config from a repo directory.
pub fn read_portable_config(repo_path: &Path) -> Result<Option<Value>> {
    let config_path = repo_path.join("homeboy.json");
    if !config_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", config_path.display())),
        )
    })?;

    let value: Value = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse homeboy.json".to_string()),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(Some(value))
}

fn portable_component_id_from_value(portable: &Value, dir: &Path) -> Option<String> {
    // If "id" key exists, it must be non-empty. A blank id in homeboy.json is an error,
    // not a fallback signal. (#801: blank ids caused split-brain between project/component
    // discovery and the component registry.)
    if let Some(id_value) = portable.get("id") {
        if let Some(id_str) = id_value.as_str() {
            if id_str.trim().is_empty() {
                // Blank id is present — log a warning and reject (return None so
                // discover_from_portable returns None, forcing explicit registration).
                crate::log_status!(
                    "warning",
                    "homeboy.json at {} has a blank 'id' field — skipping. Fix the file or run `homeboy component create`",
                    dir.display()
                );
                return None;
            }
            return crate::engine::identifier::slugify_id(id_str, "component_id").ok();
        }
    }

    // No "id" key at all — infer from directory name (backward compat for minimal configs)
    let dir_name = dir.file_name()?.to_string_lossy();
    crate::engine::identifier::slugify_id(&dir_name, "component_id").ok()
}

pub fn infer_portable_component_id(dir: &Path) -> Result<String> {
    let portable = read_portable_config(dir)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "local_path",
            format!("No homeboy.json found at {}", dir.display()),
            None,
            None,
        )
    })?;

    portable_component_id_from_value(&portable, dir).ok_or_else(|| {
        Error::validation_invalid_argument(
            "id",
            format!("Could not derive component ID from {}", dir.display()),
            None,
            None,
        )
    })
}

pub fn portable_json(component: &Component) -> Result<Value> {
    // Reject blank ids before serialization (#801)
    if component.id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "id",
            "Cannot write portable config with a blank component ID",
            None,
            Some(vec![
                "Set a valid ID: homeboy component create --local-path <path>".to_string(),
            ]),
        ));
    }

    let mut value = serde_json::to_value(component).map_err(|error| {
        Error::validation_invalid_argument(
            "component",
            "Failed to serialize component to portable config",
            Some(error.to_string()),
            None,
        )
    })?;

    let obj = value.as_object_mut().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            "Portable component config must serialize to an object",
            None,
            None,
        )
    })?;

    obj.insert("id".to_string(), Value::String(component.id.clone()));
    obj.remove("aliases");
    obj.remove("local_path");

    Ok(value)
}

/// Write component data to the repo-local homeboy.json, preserving unknown fields.
///
/// Uses a read-modify-write pattern: reads the existing JSON first, merges the
/// component's known fields on top, and writes the result. This preserves fields
/// like `baselines`, `audit_rules`, and custom local metadata that the Component
/// struct doesn't model but other subsystems or users read/write directly.
///
/// If no existing file exists, writes from scratch (no fields to preserve).
pub fn write_portable_config(dir: &Path, component: &Component) -> Result<()> {
    let path = dir.join("homeboy.json");
    let portable = portable_json(component)?;

    // Read existing file to preserve unknown fields
    let merged = if path.is_file() {
        if let Ok(Some(existing)) = read_portable_config(dir) {
            merge_preserving_unknown(existing, portable)
        } else {
            portable
        }
    } else {
        portable
    };

    let content = crate::config::to_string_pretty(&merged)?;
    crate::engine::local_files::write_file_atomic(
        &path,
        &content,
        &format!("write {}", path.display()),
    )
}

/// Merge component fields into existing JSON, preserving keys the Component struct doesn't know about.
///
/// Strategy: start with the existing JSON, overlay all keys from the new component JSON.
/// Keys in the existing JSON that are NOT in the new JSON are preserved.
/// Keys in the new JSON overwrite existing values.
fn merge_preserving_unknown(existing: Value, component: Value) -> Value {
    match (existing, component) {
        (Value::Object(mut base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                // Skip null values from the component — don't overwrite existing data with nulls
                if value.is_null() {
                    continue;
                }
                // Skip empty strings for remote_path — don't blank a real value
                if key == "remote_path" {
                    if let Some(s) = value.as_str() {
                        if s.is_empty() {
                            // Only write empty remote_path if no existing value
                            if !base.contains_key("remote_path") {
                                base.insert(key, value);
                            }
                            continue;
                        }
                    }
                }
                base.insert(key, value);
            }
            Value::Object(base)
        }
        // Fallback: if either isn't an object, prefer the component value
        (_, component) => component,
    }
}

pub fn has_portable_config(path: &Path) -> bool {
    read_portable_config(path).ok().flatten().is_some()
}

pub fn mutate_portable<F>(id: &str, mutator: F) -> Result<Component>
where
    F: FnOnce(&mut Component) -> Result<()>,
{
    let mut component = resolve_effective(Some(id), None, None)?;
    let local_path = PathBuf::from(&component.local_path);

    if !has_portable_config(&local_path) {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Component '{}' does not have repo-owned homeboy.json. Initialize the repo first with `homeboy component create --local-path {}`",
                id,
                component.local_path
            ),
            Some(id.to_string()),
            None,
        ));
    }

    mutator(&mut component)?;

    // Defensive: if the mutator wiped `id` (for example via `config::merge_config`,
    // which round-trips through serde and `RawComponent.id` is `skip_serializing`),
    // restore it from the caller-provided id. Legitimate rename mutations set a
    // non-empty new id inside the closure, so this check does not clobber them. (#1140)
    if component.id.trim().is_empty() {
        component.id = id.to_string();
    }

    write_portable_config(&local_path, &component)?;
    Ok(component)
}

/// Create a virtual (unregistered) Component from a directory's `homeboy.json`.
///
/// If the directory is a git repo and `remote_url` isn't set in the portable config,
/// auto-detects it from `git remote get-url origin`.
pub fn discover_from_portable(dir: &Path) -> Option<Component> {
    let portable = read_portable_config(dir).ok()??;

    let id = portable_component_id_from_value(&portable, dir)?;
    let local_path = dir.to_string_lossy().to_string();

    let mut json = portable;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("id".to_string(), Value::String(id));
        obj.insert("local_path".to_string(), Value::String(local_path));
        obj.entry("remote_path".to_string())
            .or_insert(Value::String(String::new()));

        // Auto-detect remote_url from git if not already set
        if !obj.contains_key("remote_url") {
            if let Some(url) = crate::deploy::release_download::detect_remote_url(dir) {
                obj.insert("remote_url".to_string(), Value::String(url));
            }
        }
    }

    serde_json::from_value::<Component>(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn write_preserves_unknown_fields() {
        let dir = TempDir::new().expect("temp dir");

        // Write initial homeboy.json with extra fields the Component struct doesn't know
        let initial = serde_json::json!({
            "id": "test-comp",
            "remote_path": "wp-content/plugins/test",
            "baselines": { "audit": { "item_count": 42 } },
            "custom_field": "preserve-me"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&initial).unwrap(),
        )
        .unwrap();

        // Create a component and write it — simulating a mutate_portable operation
        let component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            "wp-content/plugins/test".to_string(),
            None,
        );
        write_portable_config(dir.path(), &component).expect("write should succeed");

        // Read back and verify unknown fields are preserved
        let content = fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let result: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            result
                .get("baselines")
                .and_then(|v| v.get("audit"))
                .and_then(|v| v.get("item_count"))
                .and_then(|v| v.as_i64()),
            Some(42),
            "baselines should be preserved"
        );
        assert_eq!(
            result.get("custom_field").and_then(|v| v.as_str()),
            Some("preserve-me"),
            "custom fields should be preserved"
        );
        assert_eq!(
            result.get("id").and_then(|v| v.as_str()),
            Some("test-comp"),
            "id should be present"
        );
    }

    #[test]
    fn write_does_not_blank_remote_path() {
        let dir = TempDir::new().expect("temp dir");

        // Write homeboy.json with a real remote_path
        let initial = serde_json::json!({
            "id": "test-comp",
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&initial).unwrap(),
        )
        .unwrap();

        // Write a component with empty remote_path (simulating discover_from_portable default)
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            String::new(), // empty remote_path
            None,
        );
        component.remote_path = String::new();
        write_portable_config(dir.path(), &component).expect("write should succeed");

        // Read back — remote_path should NOT be blanked
        let content = fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let result: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            result.get("remote_path").and_then(|v| v.as_str()),
            Some("wp-content/plugins/test"),
            "remote_path should not be blanked by an empty component value"
        );
    }

    #[test]
    fn blank_id_rejected_by_portable_json() {
        let component = Component::new(
            String::new(), // blank id
            "/tmp".to_string(),
            "/remote".to_string(),
            None,
        );
        let result = portable_json(&component);
        assert!(result.is_err(), "blank id should be rejected");
    }

    #[test]
    fn merge_config_roundtrip_wipes_id_regression() {
        // Regression test for #1140: `Component` serializes via `RawComponent`
        // which has `#[serde(skip_serializing)]` on `id`. A `merge_config` round
        // trip (serialize → deep_merge → deserialize) therefore drops `id`,
        // leaving the component with a blank string. Callers like
        // `mutate_portable` must restore the caller-provided id before writing
        // to homeboy.json, or `portable_json` will refuse the write with
        // "Cannot write portable config with a blank component ID".
        let mut component = Component::new(
            "intelligence".to_string(),
            "/tmp/intelligence".to_string(),
            "wp-content/plugins/intelligence".to_string(),
            None,
        );

        let patch = serde_json::json!({ "local_path": "/new/path" });
        crate::config::merge_config(&mut component, patch, &[]).expect("merge should succeed");

        assert!(
            component.id.trim().is_empty(),
            "documented failure mode — remove this assert and the restore hack in mutate_portable if serde behavior changes"
        );
        assert_eq!(component.local_path, "/new/path");
    }

    #[test]
    fn blank_id_in_homeboy_json_returns_none_from_discover() {
        let dir = TempDir::new().expect("temp dir");
        let json = serde_json::json!({
            "id": "",
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        // discover_from_portable should return None for blank id
        let result = discover_from_portable(dir.path());
        assert!(
            result.is_none(),
            "blank id should cause discover to return None"
        );
    }

    #[test]
    fn merge_preserving_unknown_keeps_existing_keys() {
        let existing = serde_json::json!({
            "id": "old",
            "baselines": { "audit": {} },
            "remote_path": "real/path"
        });
        let component = serde_json::json!({
            "id": "new",
            "auto_cleanup": false
        });

        let merged = merge_preserving_unknown(existing, component);

        assert_eq!(merged.get("id").and_then(|v| v.as_str()), Some("new"));
        assert!(merged.get("baselines").is_some(), "baselines preserved");
        assert_eq!(
            merged.get("remote_path").and_then(|v| v.as_str()),
            Some("real/path")
        );
        assert_eq!(
            merged.get("auto_cleanup").and_then(|v| v.as_bool()),
            Some(false)
        );
    }
}
