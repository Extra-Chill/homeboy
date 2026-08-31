use homeboy_core::error::{Error, Result};
use homeboy_extension_contract::manifest_action_config::SettingConfig;
use std::collections::HashMap;

pub(super) fn serialize_settings(settings: &HashMap<String, serde_json::Value>) -> Result<String> {
    serde_json::to_string(settings).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize extension settings".to_string()),
        )
    })
}

pub(crate) fn build_settings_json(
    manifest_settings: &[SettingConfig],
    extension_settings: &[(String, serde_json::Value)],
    settings_overrides: &[(String, String)],
    settings_json_overrides: &[(String, serde_json::Value)],
) -> Result<String> {
    let mut settings = serde_json::json!({});

    if let serde_json::Value::Object(ref mut obj) = settings {
        for setting in manifest_settings {
            if let Some(default) = &setting.default {
                obj.insert(setting.id.clone(), default.clone());
            }
        }
    }

    // Apply component/project extension settings — preserves arrays, objects, etc.
    if let serde_json::Value::Object(ref mut obj) = settings {
        for (key, value) in extension_settings {
            obj.insert(key.clone(), value.clone());
        }

        // String overrides from `--setting key=value` (always strings).
        for (key, value) in settings_overrides {
            merge_string_setting_override(obj, key, value);
        }

        // Typed-JSON overrides from `--setting-json key=<json>` (preserves
        // object / array / typed-scalar). Applied AFTER string overrides
        // so `--setting-json` wins when both target the same key —
        // typed-JSON is strictly more expressive.
        for (key, value) in settings_json_overrides {
            obj.insert(key.clone(), value.clone());
        }
    }

    homeboy_core::config::to_json_string(&settings)
}

fn merge_string_setting_override(
    settings: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &str,
) {
    let Some((root, child_path)) = key.split_once('.') else {
        settings.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
        return;
    };

    if root.is_empty() || child_path.is_empty() || child_path.split('.').any(str::is_empty) {
        settings.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
        return;
    }

    let root_value = settings
        .entry(root.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !root_value.is_object() {
        *root_value = serde_json::Value::Object(serde_json::Map::new());
    }

    let mut current = root_value.as_object_mut().expect("root setting is object");
    let mut parts = child_path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            current.insert(
                part.to_string(),
                serde_json::Value::String(value.to_string()),
            );
            return;
        }

        let child = current
            .entry(part.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !child.is_object() {
            *child = serde_json::Value::Object(serde_json::Map::new());
        }
        current = child.as_object_mut().expect("nested setting is object");
    }
}
