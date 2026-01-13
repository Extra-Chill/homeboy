use crate::{Error, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::module::ModuleManifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingValueType {
    String,
    Number,
    Bool,
    Json,
}

pub struct ModuleSettingsValidator {
    module_id: String,
    allowed_ids: BTreeSet<String>,
    expected_types: BTreeMap<String, SettingValueType>,
}

impl ModuleSettingsValidator {
    pub fn new(module: &ModuleManifest) -> Self {
        let allowed_ids: BTreeSet<String> = module.settings.iter().map(|s| s.id.clone()).collect();
        let expected_types: BTreeMap<String, SettingValueType> = module
            .settings
            .iter()
            .map(|s| (s.id.clone(), expected_value_type(&s.setting_type)))
            .collect();

        Self {
            module_id: module.id.clone(),
            allowed_ids,
            expected_types,
        }
    }

    pub fn validate_settings_map(
        &self,
        scope: &str,
        settings: &HashMap<String, Value>,
    ) -> Result<()> {
        for (key, value) in settings {
            if !self.allowed_ids.contains(key) {
                return Err(Error::config_invalid_value(
                    format!("module.settings.{key}"),
                    Some(value.to_string()),
                    format!(
                        "Unknown setting '{key}' for module '{}' (from {scope} scope)",
                        self.module_id
                    ),
                ));
            }

            let expected = self
                .expected_types
                .get(key)
                .copied()
                .unwrap_or(SettingValueType::Json);

            if !value_matches_type(value, expected) {
                let expected_label = match expected {
                    SettingValueType::String => "string",
                    SettingValueType::Number => "number",
                    SettingValueType::Bool => "boolean",
                    SettingValueType::Json => "json",
                };

                return Err(Error::config_invalid_value(
                    format!("module.settings.{key}"),
                    Some(value.to_string()),
                    format!(
                        "Invalid type for module setting '{key}' (from {scope} scope); expected '{expected_label}'",
                    ),
                ));
            }
        }

        Ok(())
    }

    pub fn validate_json_object(
        &self,
        scope: &str,
        settings: &serde_json::Map<String, Value>,
    ) -> Result<()> {
        for (key, value) in settings {
            if !self.allowed_ids.contains(key) {
                return Err(Error::config_invalid_value(
                    format!("module.settings.{key}"),
                    Some(value.to_string()),
                    format!(
                        "Unknown setting '{key}' for module '{}' (from {scope} scope)",
                        self.module_id
                    ),
                ));
            }

            let expected = self
                .expected_types
                .get(key)
                .copied()
                .unwrap_or(SettingValueType::Json);

            if !value_matches_type(value, expected) {
                let expected_label = match expected {
                    SettingValueType::String => "string",
                    SettingValueType::Number => "number",
                    SettingValueType::Bool => "boolean",
                    SettingValueType::Json => "json",
                };

                return Err(Error::config_invalid_value(
                    format!("module.settings.{key}"),
                    Some(value.to_string()),
                    format!(
                        "Invalid type for module setting '{key}' (from {scope} scope); expected '{expected_label}'",
                    ),
                ));
            }
        }

        Ok(())
    }
}

fn expected_value_type(setting_type: &str) -> SettingValueType {
    match setting_type {
        "string" => SettingValueType::String,
        "number" => SettingValueType::Number,
        "boolean" => SettingValueType::Bool,
        _ => SettingValueType::Json,
    }
}

fn value_matches_type(value: &Value, expected: SettingValueType) -> bool {
    match expected {
        SettingValueType::String => value.is_string(),
        SettingValueType::Number => value.is_number(),
        SettingValueType::Bool => value.is_boolean(),
        SettingValueType::Json => true,
    }
}
