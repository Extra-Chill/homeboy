use crate::component;
use crate::error::{Error, Result};
use crate::http::ApiClient;
use crate::project::{self, Project};
use crate::ssh::{execute_local_command_in_dir, execute_local_command_interactive};
use crate::utils::{parser, template, validation};
use serde::Serialize;
use std::collections::HashMap;

use super::exec_context;
use super::manifest::{ActionConfig, ActionType, HttpMethod, ModuleManifest, RuntimeConfig};
use super::scope::ModuleScope;
use super::load_module;

/// Result of executing a module.
pub struct ModuleRunResult {
    pub exit_code: i32,
    pub project_id: Option<String>,
}

pub(crate) struct ModuleExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub(crate) struct ModuleExecutionOutcome {
    pub project_id: Option<String>,
    pub result: ModuleExecutionResult,
}

pub enum ModuleExecutionMode {
    Interactive,
    Captured,
}

/// Result of running module setup.
pub struct ModuleSetupResult {
    pub exit_code: i32,
}

struct ModuleExecutionContext {
    module_id: String,
    project_id: Option<String>,
    component_id: Option<String>,
    project: Option<Project>,
    settings: HashMap<String, serde_json::Value>,
}

/// Run a module's setup command (if defined).
pub fn run_setup(module_id: &str) -> Result<ModuleSetupResult> {
    let module = load_module(module_id)?;

    let runtime = match module.runtime.as_ref() {
        Some(r) => r,
        None => {
            return Ok(ModuleSetupResult { exit_code: 0 });
        }
    };

    let setup_command = match &runtime.setup_command {
        Some(cmd) => cmd,
        None => {
            return Ok(ModuleSetupResult { exit_code: 0 });
        }
    };

    let module_path = validation::require(
        module.module_path.as_ref(),
        "module",
        "module_path not set",
    )?;

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("module_path", module_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];

    let command = template::render(setup_command, &vars);
    let exit_code = execute_local_command_interactive(&command, Some(module_path), None);

    if exit_code != 0 {
        return Err(Error::other(format!(
            "Setup command failed with exit code {}",
            exit_code
        )));
    }

    Ok(ModuleSetupResult { exit_code })
}

/// Execute a module with optional project context.
pub fn run_module(
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    mode: ModuleExecutionMode,
) -> Result<ModuleRunResult> {
    let execution = execute_module_runtime(
        module_id,
        project_id,
        component_id,
        inputs,
        args,
        None,
        None,
        mode,
    )?;

    Ok(ModuleRunResult {
        exit_code: execution.result.exit_code,
        project_id: execution.project_id,
    })
}

/// Execute a module action (API call).
pub fn run_action(
    module_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
) -> Result<serde_json::Value> {
    execute_action(module_id, action_id, project_id, data, None)
}

pub(crate) fn execute_action(
    module_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let module = load_module(module_id)?;

    if module.actions.is_empty() {
        return Err(Error::other(format!(
            "Module '{}' has no actions defined",
            module_id
        )));
    }

    let action = module
        .actions
        .iter()
        .find(|a| a.id == action_id)
        .ok_or_else(|| {
            Error::other(format!(
                "Action '{}' not found in module '{}'",
                action_id, module_id
            ))
        })?;

    let selected: Vec<serde_json::Value> = if let Some(data_str) = data {
        serde_json::from_str(data_str)
            .map_err(|e| Error::other(format!("Invalid JSON data: {}", e)))?
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
                return Err(Error::other(
                    "Not authenticated. Run 'homeboy auth login --project <id>' first.",
                ));
            }

            let endpoint = validation::require(
                action.endpoint.as_ref(),
                "endpoint",
                "API action missing 'endpoint'",
            )?;

            let method = action.method.as_ref().unwrap_or(&HttpMethod::Post);
            let project = project::load(pid)?;
            let settings = ModuleScope::effective_settings(module_id, Some(&project), None)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;

            match method {
                HttpMethod::Get => client.get(endpoint),
                HttpMethod::Post => client.post(endpoint, &payload),
                HttpMethod::Put => client.put(endpoint, &payload),
                HttpMethod::Patch => client.patch(endpoint, &payload),
                HttpMethod::Delete => client.delete(endpoint),
            }
        }
        ActionType::Command => {
            let command_template = validation::require(
                action.command.as_ref(),
                "command",
                "Command action missing 'command'",
            )?;
            let project = project_id.and_then(|pid| project::load(pid).ok());
            let component = None;
            let settings = ModuleScope::effective_settings(module_id, project.as_ref(), component)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;
            let module_path = module.module_path.as_deref().unwrap_or(".");
            let vars = vec![("module_path", module_path)];

            let project_base_path = project_id
                .and_then(|pid| project::load(pid).ok())
                .and_then(|proj| proj.base_path.clone());

            let working_dir = parser::json_path_str(&payload, &["release", "local_path"])
                .unwrap_or(module_path);

            let execution = execute_module_command(
                command_template,
                &vars,
                Some(working_dir),
                &build_action_env(module_id, project_id, &payload, Some(module_path), project_base_path.as_deref()),
                ModuleExecutionMode::Captured,
            )?;
            Ok(serde_json::json!({
                "stdout": execution.stdout,
                "stderr": execution.stderr,
                "exitCode": execution.exit_code,
                "success": execution.success,
                "payload": payload
            }))
        }
    }
}

fn module_runtime(module: &ModuleManifest) -> Result<&RuntimeConfig> {
    module.runtime.as_ref().ok_or_else(|| {
        Error::other(format!(
            "Module '{}' does not have a runtime configuration and cannot be executed",
            module.id
        ))
    })
}

fn build_args_string(
    module: &ModuleManifest,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
) -> String {
    let input_values: HashMap<String, String> = inputs.into_iter().collect();
    let mut argv = Vec::new();
    for input in &module.inputs {
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

fn resolve_module_context(
    module: &ModuleManifest,
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    run_command: &str,
) -> Result<ModuleExecutionContext> {
    let requires_project = module.requires.is_some()
        || template::is_present(run_command, "projectId")
        || template::is_present(run_command, "sitePath")
        || template::is_present(run_command, "cliPath")
        || template::is_present(run_command, "domain");

    let mut project = None;
    let mut component = None;
    let mut resolved_project_id = None;
    let mut resolved_component_id = None;

    // Handle component-only execution (no project required)
    if let Some(cid) = component_id {
        if let Ok(loaded_component) = component::load(cid) {
            component = Some(loaded_component);
            resolved_component_id = Some(cid.to_string());
        }
    }

    if requires_project {
        let pid = project_id.ok_or_else(|| {
            Error::config(format!(
                "Module {} requires a project context, but no project ID was provided",
                module.id
            ))
        })?;

        let loaded_project = project::load(pid)?;
        ModuleScope::validate_project_compatibility(module, &loaded_project)?;

        resolved_component_id =
            ModuleScope::resolve_component_scope(module, &loaded_project, component_id)?;

        if let Some(ref comp_id) = resolved_component_id {
            component = Some(component::load(comp_id).map_err(|_| {
                Error::config(format!("Component {} required by module {} is not configured", comp_id, &module.id))
            })?);
        }

        resolved_project_id = Some(pid.to_string());
        project = Some(loaded_project);
    }

    let settings = ModuleScope::effective_settings(module_id, project.as_ref(), component.as_ref())?;

    Ok(ModuleExecutionContext {
        module_id: module_id.to_string(),
        project_id: resolved_project_id,
        component_id: resolved_component_id,
        project,
        settings,
    })
}

fn serialize_settings(settings: &HashMap<String, serde_json::Value>) -> Result<String> {
    serde_json::to_string(settings).map_err(|e| Error::other(e.to_string()))
}

fn build_template_vars<'a>(
    module_path: &'a str,
    args_str: &'a str,
    runtime: &'a RuntimeConfig,
    project: Option<&'a Project>,
    project_id: &'a Option<String>,
) -> Vec<(&'a str, &'a str)> {
    let entrypoint = runtime.entrypoint.as_deref().unwrap_or("");

    if let Some(proj) = project {
        let domain = proj.domain.as_deref().unwrap_or("");
        let site_path = proj.base_path.as_deref().unwrap_or("");
        vec![
            ("module_path", module_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
            ("projectId", project_id.as_deref().unwrap_or("")),
            ("domain", domain),
            ("sitePath", site_path),
        ]
    } else {
        vec![
            ("module_path", module_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
        ]
    }
}

fn build_runtime_env(
    runtime: &RuntimeConfig,
    context: &ModuleExecutionContext,
    vars: &[(&str, &str)],
    settings_json: &str,
    module_path: &str,
) -> Vec<(String, String)> {
    let project_base_path = context
        .project
        .as_ref()
        .and_then(|p| p.base_path.as_deref());

    let mut env = build_exec_env(
        &context.module_id,
        context.project_id.as_deref(),
        context.component_id.as_deref(),
        settings_json,
        Some(module_path),
        project_base_path,
        Some(&context.settings),
    );

    if let Some(ref module_env) = runtime.env {
        for (key, value) in module_env {
            let rendered_value = template::render(value, vars);
            env.push((key.clone(), rendered_value));
        }
    }

    env
}

fn build_action_env(
    module_id: &str,
    project_id: Option<&str>,
    payload: &serde_json::Value,
    module_path: Option<&str>,
    project_base_path: Option<&str>,
) -> Vec<(String, String)> {
    let settings_json = payload.to_string();
    build_exec_env(module_id, project_id, None, &settings_json, module_path, project_base_path, None)
}

fn execute_module_command(
    command_template: &str,
    vars: &[(&str, &str)],
    working_dir: Option<&str>,
    env_pairs: &[(String, String)],
    mode: ModuleExecutionMode,
) -> Result<ModuleExecutionResult> {
    let command = template::render(command_template, vars);
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    match mode {
        ModuleExecutionMode::Interactive => {
            let exit_code =
                execute_local_command_interactive(&command, working_dir, Some(&env_refs));
            Ok(ModuleExecutionResult {
                exit_code,
                stdout: String::new(),
                stderr: String::new(),
                success: exit_code == 0,
            })
        }
        ModuleExecutionMode::Captured => {
            let output = execute_local_command_in_dir(&command, working_dir, Some(&env_refs));
            Ok(ModuleExecutionResult {
                exit_code: output.exit_code,
                stdout: output.stdout,
                stderr: output.stderr,
                success: output.success,
            })
        }
    }
}

fn execute_module_runtime(
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    payload: Option<&serde_json::Value>,
    working_dir: Option<&str>,
    mode: ModuleExecutionMode,
) -> Result<ModuleExecutionOutcome> {
    // Shell execution is required for module runtime commands by design:
    // - Runtime commands execute bash scripts (set -euo pipefail, arrays, jq)
    // - Scripts use bash features (arrays, variable expansion, subshells)
    // - Commands like "{{modulePath}}/scripts/publish-github.sh" need shell
    // - Environment variable passing requires shell environment
    // - Direct execution cannot handle bash scripts or shell features
    // See executor.rs for detailed execution strategy decision tree
    let module = load_module(module_id)?;
    let runtime = module_runtime(&module)?;
    let run_command = runtime.run_command.as_ref().ok_or_else(|| {
        Error::other(format!(
            "Module '{}' does not have a runCommand defined",
            module_id
        ))
    })?;

    let module_path = validation::require(
        module.module_path.as_ref(),
        "module",
        "module_path not set",
    )?;

    let args_str = build_args_string(&module, inputs, args);
    let context =
        resolve_module_context(&module, module_id, project_id, component_id, run_command)?;

    let settings_json = if let Some(payload) = payload {
        payload.to_string()
    } else {
        serialize_settings(&context.settings)?
    };

    let vars = build_template_vars(
        module_path,
        &args_str,
        runtime,
        context.project.as_ref(),
        &context.project_id,
    );
    let env_pairs = build_runtime_env(runtime, &context, &vars, &settings_json, module_path);

    let execution = execute_module_command(
        run_command,
        &vars,
        working_dir.or(Some(module_path.as_str())),
        &env_pairs,
        mode,
    )?;

    Ok(ModuleExecutionOutcome {
        project_id: context.project_id,
        result: execution,
    })
}

pub(crate) fn run_module_runtime(
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    payload: Option<&serde_json::Value>,
    working_dir: Option<&str>,
) -> Result<ModuleExecutionOutcome> {
    execute_module_runtime(
        module_id,
        project_id,
        component_id,
        inputs,
        args,
        payload,
        working_dir,
        ModuleExecutionMode::Captured,
    )
}

/// Build execution environment variables for a module.
pub fn build_exec_env(
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    settings_json: &str,
    module_path: Option<&str>,
    project_base_path: Option<&str>,
    settings: Option<&HashMap<String, serde_json::Value>>,
) -> Vec<(String, String)> {
    let mut env = vec![
        (
            exec_context::VERSION.to_string(),
            exec_context::CURRENT_VERSION.to_string(),
        ),
        (exec_context::MODULE_ID.to_string(), module_id.to_string()),
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

        // Resolve and set component path
        match component::load(cid) {
            Ok(component) => {
                env.push(("HOMEBOY_COMPONENT_PATH".to_string(), component.local_path));
            }
            Err(e) => {
                // For debugging: if component loading fails, still set a placeholder path
                env.push(("HOMEBOY_COMPONENT_PATH".to_string(), format!("/debug/component-not-found/{}", cid)));
                env.push(("HOMEBOY_COMPONENT_LOAD_ERROR".to_string(), e.to_string()));
            }
        }
    }

    if let Some(mp) = module_path {
        env.push((exec_context::MODULE_PATH.to_string(), mp.to_string()));
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

#[derive(Debug, Clone, Serialize)]
pub struct ModuleReadyStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn module_ready_status(module: &ModuleManifest) -> ModuleReadyStatus {
    let Some(runtime) = module.runtime.as_ref() else {
        return ModuleReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(ready_check) = runtime.ready_check.as_ref() else {
        return ModuleReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(module_path) = module.module_path.as_ref() else {
        return ModuleReadyStatus {
            ready: false,
            reason: Some("missing_module_path".to_string()),
            detail: Some("ready_check configured but module_path is missing".to_string()),
        };
    };

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("module_path", module_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];
    let command = template::render(ready_check, &vars);
    let output = execute_local_command_in_dir(&command, Some(module_path), None);

    if output.success {
        return ModuleReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    }

    let detail_output = if output.stderr.trim().is_empty() {
        output.stdout
    } else {
        output.stderr
    };
    let detail = detail_output.trim();
    let detail = if detail.is_empty() {
        format!(
            "ready_check '{}' failed with exit code {}",
            command, output.exit_code
        )
    } else {
        format!(
            "ready_check '{}' failed with exit code {}: {}",
            command, output.exit_code, detail
        )
    };

    ModuleReadyStatus {
        ready: false,
        reason: Some("ready_check_failed".to_string()),
        detail: Some(detail),
    }
}

/// Check if a module is ready (setup complete).
pub fn is_module_ready(module: &ModuleManifest) -> bool {
    module_ready_status(module).ready
}

/// Check if a module is compatible with a project.
pub fn is_module_compatible(module: &ModuleManifest, project: Option<&Project>) -> bool {
    let Some(ref requires) = module.requires else {
        return true;
    };

    // Required modules must be installed globally
    for required_module in &requires.modules {
        if load_module(required_module).is_err() {
            return false;
        }
    }

    // Required components must be linked to the project (if project context exists)
    if let Some(project) = project {
        for component in &requires.components {
            if !project.component_ids.contains(component) {
                return false;
            }
        }
    }

    true
}

fn interpolate_action_payload(
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

fn interpolate_payload_value(
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
