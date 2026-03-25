//! build — extracted from execution.rs.

use crate::component::{self, Component};
use crate::error::{Error, Result};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use std::collections::HashMap;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use crate::engine::command::CapturedOutput;
use crate::project::{self, Project};
use crate::server::http::ApiClient;
use serde::Serialize;
use std::path::Path;
use super::super::runner_contract::RunnerStepFilter;
use super::super::scope::ExtensionScope;
use super::ExtensionExecutionContext;


pub(crate) fn build_args_string(
    extension: &ExtensionManifest,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
) -> String {
    let input_values: HashMap<String, String> = inputs.into_iter().collect();
    let mut argv = Vec::new();
    for input in extension.inputs() {
        if let Some(value) = input_values.get(&input.id) {
            if !value.is_empty() {
                argv.push(input.arg.clone());
                argv.push(value.clone());
            }
        }
    }
    argv.extend(args);
    argv.join(" ")
}

pub fn build_settings_json_from_manifest(
    manifest: &serde_json::Value,
    extension_settings: &[(String, serde_json::Value)],
    settings_overrides: &[(String, String)],
) -> Result<String> {
    let mut settings = serde_json::json!({});

    // Load defaults from manifest — preserve original JSON types.
    if let Some(manifest_settings) = manifest.get("settings") {
        if let Some(settings_array) = manifest_settings.as_array() {
            if let serde_json::Value::Object(ref mut obj) = settings {
                for setting in settings_array {
                    if let Some(id) = setting.get("id").and_then(|v| v.as_str()) {
                        if let Some(default) = setting.get("default") {
                            obj.insert(id.to_string(), default.clone());
                        }
                    }
                }
            }
        }
    }

    // Apply component/project extension settings — preserves arrays, objects, etc.
    if let serde_json::Value::Object(ref mut obj) = settings {
        for (key, value) in extension_settings {
            obj.insert(key.clone(), value.clone());
        }

        // CLI overrides are always strings (from --setting key=value).
        for (key, value) in settings_overrides {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
    }

    crate::config::to_json_string(&settings)
}

pub fn build_capability_execution_context(
    execution_context: &super::ExtensionExecutionContext,
    component: Component,
    path_override: Option<&str>,
) -> super::ExtensionExecutionContext {
    let mut execution = execution_context.clone();
    execution.component = component;

    if let Some(path) = path_override {
        execution.component.local_path = path.to_string();
    }

    execution
}
