use crate::component::{self, Component};
use crate::engine::command::CapturedOutput;
use crate::engine::shell;
use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::local_files;
use crate::project::{self, Project};
use crate::server::http::ApiClient;
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use super::exec_context;
use super::load_extension;
use super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::runner_contract::RunnerStepFilter;
use super::runtime_helper;
use super::scope::ExtensionScope;

/// Result of executing a extension.
pub struct ExtensionRunResult {
    pub exit_code: i32,
    pub project_id: Option<String>,
    pub output: Option<CapturedOutput>,
}

pub(crate) struct ExtensionExecutionResult {
    pub output: CapturedOutput,
    pub exit_code: i32,
    pub success: bool,
}

pub(crate) struct ExtensionExecutionOutcome {
    pub project_id: Option<String>,
    pub result: ExtensionExecutionResult,
}

pub enum ExtensionExecutionMode {
    Interactive,
    Captured,
}

/// Result of running extension setup.
pub struct ExtensionSetupResult {
    pub exit_code: i32,
}

struct ExtensionExecutionContext {
    extension_id: String,
    project_id: Option<String>,
    component_id: Option<String>,
    project: Option<Project>,
    settings: HashMap<String, serde_json::Value>,
}

/// Run a extension's setup command (if defined).
pub fn run_setup(extension_id: &str) -> Result<ExtensionSetupResult> {
    let extension = load_extension(extension_id)?;

    let runtime = match extension.runtime() {
        Some(r) => r,
        None => {
            return Ok(ExtensionSetupResult { exit_code: 0 });
        }
    };

    let setup_command = match &runtime.setup_command {
        Some(cmd) => cmd,
        None => {
            return Ok(ExtensionSetupResult { exit_code: 0 });
        }
    };

    let extension_path = validation::require(
        extension.extension_path.as_ref(),
        "extension",
        "extension_path not set",
    )?;

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("extension_path", extension_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];

    let command = template::render(setup_command, &vars);
    let exit_code = execute_local_command_interactive(&command, Some(extension_path), None);

    if exit_code != 0 {
        return Err(Error::internal_io(
            format!("Setup command failed with exit code {}", exit_code),
            Some("extension setup".to_string()),
        ));
    }

    Ok(ExtensionSetupResult { exit_code })
}

/// Backward-compatible alias for existing command API usage.
pub type ExtensionStepFilter = RunnerStepFilter;

/// Execute a extension with optional project context.
pub fn run_extension(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    mode: ExtensionExecutionMode,
    filter: ExtensionStepFilter,
) -> Result<ExtensionRunResult> {
    let is_captured = matches!(mode, ExtensionExecutionMode::Captured);
    let execution = execute_extension_runtime(
        extension_id,
        project_id,
        component_id,
        inputs,
        args,
        None,
        None,
        mode,
        &filter,
    )?;

    let output = if is_captured && !execution.result.output.is_empty() {
        Some(execution.result.output)
    } else {
        None
    };

    Ok(ExtensionRunResult {
        exit_code: execution.result.exit_code,
        project_id: execution.project_id,
        output,
    })
}

/// Execute a extension action (API call).
pub fn run_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
) -> Result<serde_json::Value> {
    execute_action(extension_id, action_id, project_id, data, None)
}

pub(crate) fn execute_action(
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

fn extension_runtime(extension: &ExtensionManifest) -> Result<&RuntimeConfig> {
    extension.runtime().ok_or_else(|| {
        Error::config(format!(
            "Extension '{}' does not have a runtime configuration and cannot be executed",
            extension.id
        ))
    })
}

fn build_args_string(
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

fn resolve_extension_context(
    extension: &ExtensionManifest,
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    run_command: &str,
) -> Result<ExtensionExecutionContext> {
    let requires_project = extension.requires.is_some()
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
        if let Ok(loaded_component) = component::resolve_effective(Some(cid), None, None) {
            component = Some(loaded_component);
            resolved_component_id = Some(cid.to_string());
        }
    }

    if requires_project {
        let pid = project_id.ok_or_else(|| {
            Error::config(format!(
                "Extension {} requires a project context, but no project ID was provided",
                extension.id
            ))
        })?;

        let loaded_project = project::load(pid)?;
        ExtensionScope::validate_project_compatibility(extension, &loaded_project)?;

        resolved_component_id =
            ExtensionScope::resolve_component_scope(extension, &loaded_project, component_id)?;

        if let Some(ref comp_id) = resolved_component_id {
            component = Some(
                component::resolve_effective(Some(comp_id), None, Some(&loaded_project)).map_err(
                    |_| {
                        Error::config(format!(
                            "Component {} required by extension {} is not configured",
                            comp_id, &extension.id
                        ))
                    },
                )?,
            );
        }

        resolved_project_id = Some(pid.to_string());
        project = Some(loaded_project);
    }

    let settings =
        ExtensionScope::effective_settings(extension_id, project.as_ref(), component.as_ref())?;

    Ok(ExtensionExecutionContext {
        extension_id: extension_id.to_string(),
        project_id: resolved_project_id,
        component_id: resolved_component_id,
        project,
        settings,
    })
}

fn serialize_settings(settings: &HashMap<String, serde_json::Value>) -> Result<String> {
    serde_json::to_string(settings).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize extension settings".to_string()),
        )
    })
}

pub(crate) fn load_extension_manifest_from_dir(extension_path: &Path) -> Result<serde_json::Value> {
    let extension_name = extension_path
        .file_name()
        .ok_or_else(|| Error::internal_io("Extension path has no file name".to_string(), None))?
        .to_string_lossy();
    let manifest_path = extension_path.join(format!("{}.json", extension_name));

    if !manifest_path.exists() {
        return Err(Error::internal_io(
            format!("Extension manifest not found: {}", manifest_path.display()),
            None,
        ));
    }

    let content =
        local_files::read_file(&manifest_path, &format!("read {}", manifest_path.display()))?;

    serde_json::from_str(&content)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse manifest".to_string()), None))
}

pub(crate) fn build_settings_json_from_manifest(
    manifest: &serde_json::Value,
    extension_settings: &[(String, String)],
    settings_overrides: &[(String, String)],
) -> Result<String> {
    let mut settings = serde_json::json!({});

    if let Some(manifest_settings) = manifest.get("settings") {
        if let Some(settings_array) = manifest_settings.as_array() {
            if let serde_json::Value::Object(ref mut obj) = settings {
                for setting in settings_array {
                    if let (Some(id), Some(default)) = (
                        setting.get("id").and_then(|v| v.as_str()),
                        setting.get("default").and_then(|v| v.as_str()),
                    ) {
                        obj.insert(
                            id.to_string(),
                            serde_json::Value::String(default.to_string()),
                        );
                    }
                }
            }
        }
    }

    if let serde_json::Value::Object(ref mut obj) = settings {
        for (key, value) in extension_settings {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }

        for (key, value) in settings_overrides {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
    }

    crate::config::to_json_string(&settings)
}

pub(crate) fn validate_capability_script_exists(
    extension_path: &Path,
    script_path: &str,
    capability: super::ExtensionCapability,
) -> Result<()> {
    let script_path = extension_path.join(script_path);
    if !script_path.exists() {
        let label = match capability {
            super::ExtensionCapability::Lint => "lint",
            super::ExtensionCapability::Test => "test",
            super::ExtensionCapability::Build => "build",
        };

        return Err(Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension at {} does not have {} infrastructure (missing {})",
                extension_path.display(),
                label,
                script_path.display()
            ),
            None,
            None,
        ));
    }
    Ok(())
}

pub(crate) fn build_capability_env(
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

pub(crate) fn execute_capability_script(
    extension_path: &Path,
    script_path: &str,
    script_args: &[String],
    env_vars: &[(String, String)],
    working_dir: Option<&str>,
    command_override: Option<&str>,
) -> Result<CommandOutput> {
    let command = if let Some(cmd) = command_override {
        cmd.to_string()
    } else {
        let resolved = extension_path.join(script_path);
        let mut cmd = shell::quote_path(&resolved.to_string_lossy());
        if !script_args.is_empty() {
            cmd.push(' ');
            cmd.push_str(&shell::quote_args(script_args));
        }
        cmd
    };

    let env_refs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let env_opt = if env_refs.is_empty() {
        None
    } else {
        Some(env_refs.as_slice())
    };

    if let Some(dir) = working_dir {
        Ok(execute_local_command_in_dir(&command, Some(dir), env_opt))
    } else {
        Ok(execute_local_command_passthrough(&command, None, env_opt))
    }
}

pub(crate) struct PreparedCapabilityRun {
    pub execution: super::ExtensionExecutionContext,
    pub settings_json: String,
}

pub(crate) fn resolve_capability_component(
    execution_context: &super::ExtensionExecutionContext,
    pre_loaded_component: Option<&Component>,
    path_override: Option<&str>,
) -> Result<Component> {
    let mut comp = if let Some(pre_loaded) = pre_loaded_component {
        pre_loaded.clone()
    } else {
        component::resolve_effective(Some(&execution_context.component.id), path_override, None)?
    };

    if let Some(path) = path_override {
        comp.local_path = path.to_string();
    }

    Ok(comp)
}

pub(crate) fn build_capability_execution_context(
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

pub(crate) fn prepare_capability_run(
    execution_context: &super::ExtensionExecutionContext,
    pre_loaded_component: Option<&Component>,
    path_override: Option<&str>,
    settings_overrides: &[(String, String)],
    skip_script_validation: bool,
) -> Result<PreparedCapabilityRun> {
    let component =
        resolve_capability_component(execution_context, pre_loaded_component, path_override)?;
    let execution = build_capability_execution_context(execution_context, component, path_override);

    // Skip validation when a command_override is provided (e.g., Build with command_template)
    // since the script_path may be empty or not point to an actual file.
    if !skip_script_validation && !execution.script_path.is_empty() {
        validate_capability_script_exists(
            &execution.extension_path,
            &execution.script_path,
            execution.capability,
        )?;
    }

    let manifest = load_extension_manifest_from_dir(&execution.extension_path)?;
    let settings_json =
        build_settings_json_from_manifest(&manifest, &execution.settings, settings_overrides)?;

    Ok(PreparedCapabilityRun {
        execution,
        settings_json,
    })
}

fn build_template_vars<'a>(
    extension_path: &'a str,
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
            ("extension_path", extension_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
            ("projectId", project_id.as_deref().unwrap_or("")),
            ("domain", domain),
            ("sitePath", site_path),
        ]
    } else {
        vec![
            ("extension_path", extension_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
        ]
    }
}

fn build_runtime_env(
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

fn build_action_env(
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

fn execute_extension_command(
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

#[allow(clippy::too_many_arguments)]
fn execute_extension_runtime(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    payload: Option<&serde_json::Value>,
    working_dir: Option<&str>,
    mode: ExtensionExecutionMode,
    filter: &ExtensionStepFilter,
) -> Result<ExtensionExecutionOutcome> {
    // Shell execution is required for extension runtime commands by design:
    // - Runtime commands execute bash scripts (set -euo pipefail, arrays, jq)
    // - Scripts use bash features (arrays, variable expansion, subshells)
    // - Commands like "{{extensionPath}}/scripts/publish-github.sh" need shell
    // - Environment variable passing requires shell environment
    // - Direct execution cannot handle bash scripts or shell features
    // See executor.rs for detailed execution strategy decision tree
    let extension = load_extension(extension_id)?;
    let runtime = extension_runtime(&extension)?;
    let run_command = runtime.run_command.as_ref().ok_or_else(|| {
        Error::config(format!(
            "Extension '{}' does not have a runCommand defined",
            extension_id
        ))
    })?;

    let extension_path = validation::require(
        extension.extension_path.as_ref(),
        "extension",
        "extension_path not set",
    )?;

    let args_str = build_args_string(&extension, inputs, args);
    let context = resolve_extension_context(
        &extension,
        extension_id,
        project_id,
        component_id,
        run_command,
    )?;

    let settings_json = if let Some(payload) = payload {
        payload.to_string()
    } else {
        serialize_settings(&context.settings)?
    };

    let vars = build_template_vars(
        extension_path,
        &args_str,
        runtime,
        context.project.as_ref(),
        &context.project_id,
    );
    let mut env_pairs = build_runtime_env(runtime, &context, &vars, &settings_json, extension_path);

    env_pairs.extend(filter.to_env_pairs());

    let execution = execute_extension_command(
        run_command,
        &vars,
        working_dir.or(Some(extension_path.as_str())),
        &env_pairs,
        mode,
    )?;

    Ok(ExtensionExecutionOutcome {
        project_id: context.project_id,
        result: execution,
    })
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

    if let Ok(helper_path) = runtime_helper::ensure_runner_steps_helper() {
        env.push((
            runtime_helper::RUNNER_STEPS_ENV.to_string(),
            helper_path.to_string_lossy().to_string(),
        ));
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
pub struct ExtensionReadyStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn extension_ready_status(extension: &ExtensionManifest) -> ExtensionReadyStatus {
    let Some(runtime) = extension.runtime() else {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(ready_check) = runtime.ready_check.as_ref() else {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(extension_path) = extension.extension_path.as_ref() else {
        return ExtensionReadyStatus {
            ready: false,
            reason: Some("missing_extension_path".to_string()),
            detail: Some("ready_check configured but extension_path is missing".to_string()),
        };
    };

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("extension_path", extension_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];
    let command = template::render(ready_check, &vars);
    let output = execute_local_command_in_dir(&command, Some(extension_path), None);

    if output.success {
        return ExtensionReadyStatus {
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

    ExtensionReadyStatus {
        ready: false,
        reason: Some("ready_check_failed".to_string()),
        detail: Some(detail),
    }
}

/// Check if a extension is compatible with a project.
pub fn is_extension_compatible(extension: &ExtensionManifest, project: Option<&Project>) -> bool {
    let Some(ref requires) = extension.requires else {
        return true;
    };

    // Required extensions must be installed globally
    for required_extension in &requires.extensions {
        if load_extension(required_extension).is_err() {
            return false;
        }
    }

    // Required components must be linked to the project (if project context exists)
    if let Some(project) = project {
        for component in &requires.components {
            if !crate::project::has_component(project, component) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_exec_env_includes_runtime_runner_helper_path() {
        let env = build_exec_env("rust", None, None, "{}", Some("/tmp/ext"), None, None, None);

        let helper = env
            .iter()
            .find(|(k, _)| k == runtime_helper::RUNNER_STEPS_ENV)
            .map(|(_, v)| v.clone());

        assert!(helper.is_some());
        assert!(helper.unwrap().ends_with("runner-steps.sh"));
    }

    #[test]
    fn build_exec_env_preserves_step_filter_contract() {
        let filter = RunnerStepFilter {
            step: Some("lint,test".to_string()),
            skip: Some("lint".to_string()),
        };

        let mut env = build_exec_env("rust", None, None, "{}", Some("/tmp/ext"), None, None, None);
        env.extend(filter.to_env_pairs());

        assert!(env
            .iter()
            .any(|(k, v)| k == "HOMEBOY_STEP" && v == "lint,test"));
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "lint"));
    }
}
