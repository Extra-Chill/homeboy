//! build_exec_env — extracted from execution.rs.

use crate::component::{self, Component};
use crate::engine::command::CapturedOutput;
use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::http::ApiClient;
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use std::collections::HashMap;
use std::path::Path;
use super::super::exec_context;
use super::super::load_extension;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::super::runtime_helper;
use super::super::scope::ExtensionScope;
use serde::Serialize;
use super::super::runner_contract::RunnerStepFilter;
use super::ExtensionExecutionContext;
use super::ExtensionExecutionMode;
use super::ExtensionExecutionResult;


/// Execute a extension action (API call).
pub fn run_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
) -> Result<serde_json::Value> {
    execute_action(extension_id, action_id, project_id, data, None)
}

pub fn execute_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let extension = load_extension(extension_id)?;

    if extension.actions.is_empty() {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!("Extension '{}' has no actions defined", extension_id),
            Some(extension_id.to_string()),
            None,
        ));
    }

    let action = extension
        .actions
        .iter()
        .find(|a| a.id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!(
                    "Action '{}' not found in extension '{}'",
                    action_id, extension_id
                ),
                Some(action_id.to_string()),
                None,
            )
        })?;

    let selected: Vec<serde_json::Value> = if let Some(data_str) = data {
        serde_json::from_str(data_str).map_err(|e| {
            Error::internal_json(e.to_string(), Some("parse action data".to_string()))
        })?
    } else {
        Vec::new()
    };

    match action.action_type {
        ActionType::Api => {
            let pid = validation::require(
                project_id,
                "project",
                "--project is required for API actions",
            )?;

            let project = project::load(pid)?;
            let client = ApiClient::new(pid, &project.api)?;

            if action.requires_auth.unwrap_or(false) && !client.is_authenticated() {
                return Err(Error::validation_invalid_argument(
                    "auth",
                    "Not authenticated",
                    None,
                    Some(vec!["Run 'homeboy auth login --project <id>' first.".to_string()]),
                ));
            }

            let endpoint = validation::require(
                action.endpoint.as_ref(),
                "endpoint",
                "API action missing 'endpoint'",
            )?;

            let method = action.method.as_ref().unwrap_or(&HttpMethod::Post);
            let project = project::load(pid)?;
            let settings = ExtensionScope::effective_settings(extension_id, Some(&project), None)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;

            match method {
                HttpMethod::Get => client.get(endpoint),
                HttpMethod::Post => client.post(endpoint, &payload),
                HttpMethod::Put => client.put(endpoint, &payload),
                HttpMethod::Patch => client.patch(endpoint, &payload),
                HttpMethod::Delete => client.delete(endpoint),
            }
        }
        ActionType::Builtin => Err(Error::validation_invalid_argument(
            "action_id",
            format!("Action '{}' is a builtin action. Builtin actions run in the Desktop app, not the CLI.", action_id),
            Some(action_id.to_string()),
            None,
        )),
        ActionType::Command => {
            let command_template = validation::require(
                action.command.as_ref(),
                "command",
                "Command action missing 'command'",
            )?;
            let project = project_id.and_then(|pid| project::load(pid).ok());
            let component = None;
            let settings = ExtensionScope::effective_settings(extension_id, project.as_ref(), component)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;
            let extension_path = extension.extension_path.as_deref().unwrap_or(".");
            let vars = vec![("extension_path", extension_path)];

            let project_base_path = project_id
                .and_then(|pid| project::load(pid).ok())
                .and_then(|proj| proj.base_path.clone());

            let working_dir =
                crate::engine::text::json_path_str(&payload, &["release", "local_path"]).unwrap_or(extension_path);

            let execution = execute_extension_command(
                command_template,
                &vars,
                Some(working_dir),
                &build_action_env(
                    extension_id,
                    project_id,
                    &payload,
                    Some(extension_path),
                    project_base_path.as_deref(),
                ),
                ExtensionExecutionMode::Captured,
            )?;
            Ok(serde_json::json!({
                "stdout": execution.output.stdout,
                "stderr": execution.output.stderr,
                "exitCode": execution.exit_code,
                "success": execution.success,
                "payload": payload
            }))
        }
    }
}

pub fn build_capability_env(
    extension_name: &str,
    component_id: &str,
    extension_path: &Path,
    component_path: &Path,
    settings_json: &str,
    extra_env: &[(String, String)],
) -> Vec<(String, String)> {
    let component_path = component_path.to_string_lossy();
    let mut env = build_exec_env(
        extension_name,
        None,
        Some(component_id),
        settings_json,
        Some(&extension_path.to_string_lossy()),
        None,
        None,
        Some(&component_path),
    );
    env.extend(extra_env.iter().cloned());
    env
}

pub(crate) fn build_runtime_env(
    runtime: &RuntimeConfig,
    context: &ExtensionExecutionContext,
    vars: &[(&str, &str)],
    settings_json: &str,
    extension_path: &str,
) -> Vec<(String, String)> {
    let project_base_path = context
        .project
        .as_ref()
        .and_then(|p| p.base_path.as_deref());

    let mut env = build_exec_env(
        &context.extension_id,
        context.project_id.as_deref(),
        context.component_id.as_deref(),
        settings_json,
        Some(extension_path),
        project_base_path,
        Some(&context.settings),
        None, // no path override in runtime context
    );

    if let Some(ref extension_env) = runtime.env {
        for (key, value) in extension_env {
            let rendered_value = template::render(value, vars);
            env.push((key.clone(), rendered_value));
        }
    }

    env
}

pub(crate) fn build_action_env(
    extension_id: &str,
    project_id: Option<&str>,
    payload: &serde_json::Value,
    extension_path: Option<&str>,
    project_base_path: Option<&str>,
) -> Vec<(String, String)> {
    let settings_json = payload.to_string();
    build_exec_env(
        extension_id,
        project_id,
        None,
        &settings_json,
        extension_path,
        project_base_path,
        None,
        None, // no path override in action context
    )
}

pub(crate) fn execute_extension_command(
    command_template: &str,
    vars: &[(&str, &str)],
    working_dir: Option<&str>,
    env_pairs: &[(String, String)],
    mode: ExtensionExecutionMode,
) -> Result<ExtensionExecutionResult> {
    let command = template::render(command_template, vars);
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    match mode {
        ExtensionExecutionMode::Interactive => {
            let exit_code =
                execute_local_command_interactive(&command, working_dir, Some(&env_refs));
            Ok(ExtensionExecutionResult {
                output: CapturedOutput::default(),
                exit_code,
                success: exit_code == 0,
            })
        }
        ExtensionExecutionMode::Captured => {
            let cmd_output = execute_local_command_in_dir(&command, working_dir, Some(&env_refs));
            Ok(ExtensionExecutionResult {
                output: CapturedOutput::new(cmd_output.stdout, cmd_output.stderr),
                exit_code: cmd_output.exit_code,
                success: cmd_output.success,
            })
        }
    }
}

/// Build execution environment variables for a extension.
///
/// This is the single canonical env builder for all extension execution contexts
/// (test, lint, build, extension run, deploy hooks, action handlers).
///
/// When `component_path_override` is provided, it is used as the component path
/// instead of loading the component from storage. This supports `--path` overrides
/// in commands like `homeboy test --path /alt/path`.
#[allow(clippy::too_many_arguments)]
pub fn build_exec_env(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    settings_json: &str,
    extension_path: Option<&str>,
    project_base_path: Option<&str>,
    settings: Option<&HashMap<String, serde_json::Value>>,
    component_path_override: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = vec![
        (
            exec_context::VERSION.to_string(),
            exec_context::CURRENT_VERSION.to_string(),
        ),
        (
            exec_context::EXTENSION_ID.to_string(),
            extension_id.to_string(),
        ),
        (
            exec_context::SETTINGS_JSON.to_string(),
            settings_json.to_string(),
        ),
    ];

    if let Some(pid) = project_id {
        env.push((exec_context::PROJECT_ID.to_string(), pid.to_string()));
    }

    if let Some(cid) = component_id {
        env.push((exec_context::COMPONENT_ID.to_string(), cid.to_string()));

        // Use override path if provided, otherwise load from storage
        let component_path = if let Some(override_path) = component_path_override {
            override_path.to_string()
        } else {
            match component::resolve_effective(Some(cid), None, None) {
                Ok(component) => component.local_path,
                Err(e) => {
                    env.push(("HOMEBOY_COMPONENT_LOAD_ERROR".to_string(), e.to_string()));
                    format!("/debug/component-not-found/{}", cid)
                }
            }
        };
        env.push((exec_context::COMPONENT_PATH.to_string(), component_path));
    }

    if let Some(mp) = extension_path {
        env.push((exec_context::EXTENSION_PATH.to_string(), mp.to_string()));
    }

    if let Ok(helper_pairs) = runtime_helper::ensure_all_helpers() {
        env.extend(helper_pairs);
    }

    if let Some(pbp) = project_base_path {
        env.push((exec_context::PROJECT_PATH.to_string(), pbp.to_string()));
    }

    if let Some(settings_map) = settings {
        for (key, value) in settings_map {
            let env_key = format!("HOMEBOY_SETTINGS_{}", key.to_uppercase());
            let env_value = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            env.push((env_key, env_value));
        }
    }

    env
}

pub(crate) fn interpolate_action_payload(
    action: &ActionConfig,
    selected: &[serde_json::Value],
    settings: &HashMap<String, serde_json::Value>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let payload_template = match &action.payload {
        Some(p) => p,
        None => {
            if let Some(payload) = payload {
                return Ok(payload.clone());
            }
            return Ok(serde_json::Value::Object(serde_json::Map::new()));
        }
    };

    let mut result = serde_json::Map::new();
    for (key, value) in payload_template {
        let interpolated = interpolate_payload_value(value, selected, settings, payload)?;
        result.insert(key.clone(), interpolated);
    }

    Ok(serde_json::Value::Object(result))
}

pub(crate) fn interpolate_payload_value(
    value: &serde_json::Value,
    selected: &[serde_json::Value],
    settings: &HashMap<String, serde_json::Value>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::String(template) => {
            if template == "{{selected}}" {
                Ok(serde_json::Value::Array(selected.to_vec()))
            } else if template.starts_with("{{settings.") && template.ends_with("}}") {
                let key = &template[11..template.len() - 2];
                Ok(settings
                    .get(key)
                    .cloned()
                    .unwrap_or(serde_json::Value::String(String::new())))
            } else if template.starts_with("{{payload.") && template.ends_with("}}") {
                let key = &template[10..template.len() - 2];
                Ok(payload
                    .and_then(|payload| payload.get(key))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            } else if template.starts_with("{{release.") && template.ends_with("}}") {
                let key = &template[10..template.len() - 2];
                Ok(payload
                    .and_then(|p| p.get("release"))
                    .and_then(|r| r.get(key))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            } else {
                Ok(serde_json::Value::String(template.clone()))
            }
        }
        serde_json::Value::Array(arr) => {
            let interpolated: Result<Vec<serde_json::Value>> = arr
                .iter()
                .map(|v| interpolate_payload_value(v, selected, settings, payload))
                .collect();
            Ok(serde_json::Value::Array(interpolated?))
        }
        serde_json::Value::Object(obj) => {
            let mut result = serde_json::Map::new();
            for (k, v) in obj {
                result.insert(
                    k.clone(),
                    interpolate_payload_value(v, selected, settings, payload)?,
                );
            }
            Ok(serde_json::Value::Object(result))
        }
        _ => Ok(value.clone()),
    }
}
