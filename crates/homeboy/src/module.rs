use crate::paths;
use crate::files::{self, FileSystem};
use crate::json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Unified module manifest that can provide platform behavior AND/OR executable tools.
/// All fields are optional - modules include only what they need.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleManifest {
    // Required metadata
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: String,

    // Optional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    // Platform behavior
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_logs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploy: Vec<DeployVerification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_patterns: Vec<VersionPatternConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,

    // Executable tools (from former modules)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionConfig>,

    // Shared
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<SettingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequirementsConfig>,

    // Internal path (not serialized)
    #[serde(skip)]
    pub module_path: Option<String>,
}

impl ModuleManifest {
    pub fn has_cli(&self) -> bool {
        self.cli.is_some()
    }

    pub fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }
}

// Requirements configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

// Platform behavior configs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<DatabaseCliConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseCliConfig {
    pub tables_command: String,
    pub describe_command: String,
    pub query_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliConfig {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryConfig {
    pub find_command: String,
    pub base_path_transform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name_command: Option<String>,
}

impl DiscoveryConfig {
    pub fn transform_to_base_path(&self, path: &str) -> String {
        match self.base_path_transform.as_str() {
            "dirname" => std::path::Path::new(path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string()),
            _ => path.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployVerification {
    pub path_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionPatternConfig {
    pub extension: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_template: Option<String>,
}

// Executable tool configs (from former modules)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfig {
    /// Shell command to execute when running the module.
    /// Template variables: {{entrypoint}}, {{args}}, {{modulePath}}, plus project context vars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,

    /// Shell command to set up the module (e.g., create venv, install deps).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,

    /// Shell command to check if module is ready. Exit 0 = ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_check: Option<String>,

    /// Environment variables to set when running the module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,

    /// Entry point file (used in template substitution).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,

    /// Default args template (used in template substitution).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,

    /// Default site for this module (used by some CLI modules).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_site: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub input_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOption>>,
    pub arg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfig {
    pub schema: OutputSchema,
    pub display: String,
    pub selectable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionConfig {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_auth: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub setting_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

// Module loader functions

pub fn load_module(id: &str) -> Option<ModuleManifest> {
    let module_dir = paths::module(id).ok()?;
    let manifest_path = module_dir.join("homeboy.json");

    if !manifest_path.exists() {
        return None;
    }

    let content = files::local().read(&manifest_path).ok()?;
    let mut manifest: ModuleManifest = json::from_str(&content).ok()?;
    manifest.module_path = Some(module_dir.to_string_lossy().to_string());
    Some(manifest)
}

pub fn load_all_modules() -> Vec<ModuleManifest> {
    let Ok(modules_dir) = paths::modules() else {
        return Vec::new();
    };
    if !modules_dir.exists() {
        return Vec::new();
    }

    let Ok(entries) = fs::read_dir(&modules_dir) else {
        return Vec::new();
    };

    let mut modules = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let manifest_path = path.join("homeboy.json");
            if let Ok(content) = files::local().read(&manifest_path) {
                if let Ok(mut manifest) = json::from_str::<ModuleManifest>(&content) {
                    manifest.module_path = Some(path.to_string_lossy().to_string());
                    modules.push(manifest);
                }
            }
        }
    }

    modules.sort_by(|a, b| a.id.cmp(&b.id));
    modules
}

pub fn find_module_by_tool(tool: &str) -> Option<ModuleManifest> {
    load_all_modules()
        .into_iter()
        .find(|m| m.cli.as_ref().is_some_and(|c| c.tool == tool))
}

pub fn module_path(id: &str) -> PathBuf {
    paths::module(id).unwrap_or_else(|_| PathBuf::from(id))
}

pub fn available_module_ids() -> Vec<String> {
    load_all_modules().into_iter().map(|m| m.id).collect()
}

/// Environment variable names for module execution context.
/// Modules receive these variables when executed via `homeboy module run`.
pub mod exec_context {
    /// Version of the exec context protocol. Modules can check this for compatibility.
    pub const VERSION: &str = "HOMEBOY_EXEC_CONTEXT_VERSION";
    /// ID of the module being executed.
    pub const MODULE_ID: &str = "HOMEBOY_MODULE_ID";
    /// JSON-serialized settings (merged from app, project, and component levels).
    pub const SETTINGS_JSON: &str = "HOMEBOY_SETTINGS_JSON";
    /// Project ID (only set when module requires project context).
    pub const PROJECT_ID: &str = "HOMEBOY_PROJECT_ID";
    /// Component ID (only set when module requires component context).
    pub const COMPONENT_ID: &str = "HOMEBOY_COMPONENT_ID";

    /// Current version of the exec context protocol.
    pub const CURRENT_VERSION: &str = "1";
}

// ============================================================================
// Module Execution API
// ============================================================================

use crate::component::{self, Component};
use crate::project::{self, Project};
use crate::http::ApiClient;
use crate::ssh::execute_local_command_interactive;
use crate::template;
use crate::error::{Error, Result};
use std::collections::HashMap;

/// Result of executing a module.
pub struct ModuleRunResult {
    pub exit_code: i32,
    pub project_id: Option<String>,
}

/// Execute a module with optional project context.
pub fn run_module(
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
) -> Result<ModuleRunResult> {
    let module = load_module(module_id)
        .ok_or_else(|| Error::other(format!("Module '{}' not found", module_id)))?;

    let runtime = module.runtime.as_ref().ok_or_else(|| {
        Error::other(format!(
            "Module '{}' does not have a runtime configuration and cannot be executed",
            module_id
        ))
    })?;

    let run_command = runtime.run_command.as_ref().ok_or_else(|| {
        Error::other(format!(
            "Module '{}' does not have a runCommand defined",
            module_id
        ))
    })?;

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| Error::other("module_path not set".to_string()))?;

    let input_values: HashMap<String, String> = inputs.into_iter().collect();

    // Build args string from inputs and trailing args
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
    let args_str = argv.join(" ");

    // Check if project context is required
    let requires_project = module.requires.is_some()
        || template::is_present(run_command, "projectId")
        || template::is_present(run_command, "sitePath")
        || template::is_present(run_command, "cliPath")
        || template::is_present(run_command, "domain");

    let mut resolved_project_id: Option<String> = None;
    let mut resolved_component_id: Option<String> = None;
    let mut project_config: Option<Project> = None;
    let mut component_config = None;

    if requires_project {
        let pid = project_id.ok_or_else(|| {
            Error::other("This module requires a project; pass --project <id>".to_string())
        })?;

        let loaded_project = project::load(pid)?;
        ModuleScope::validate_project_compatibility(&module, &loaded_project)?;

        resolved_component_id =
            ModuleScope::resolve_component_scope(&module, &loaded_project, component_id)?;

        if let Some(ref comp_id) = resolved_component_id {
            component_config = Some(component::load(comp_id).map_err(|_| {
                Error::config(format!(
                    "Component '{}' required by module '{}' is not configured",
                    comp_id, module.id
                ))
            })?);
        }

        resolved_project_id = Some(pid.to_string());
        project_config = Some(loaded_project);
    }

    let effective_settings = ModuleScope::effective_settings(
        module_id,
        project_config.as_ref(),
        component_config.as_ref(),
    );

    let settings_json =
        serde_json::to_string(&effective_settings).map_err(|e| Error::other(e.to_string()))?;

    // Build template variables
    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let domain: String;
    let site_path: String;

    let vars: Vec<(&str, &str)> = if let Some(ref proj) = project_config {
        domain = proj.domain.clone();
        site_path = proj.base_path.clone().unwrap_or_default();

        vec![
            ("modulePath", module_path.as_str()),
            ("entrypoint", entrypoint.as_str()),
            ("args", args_str.as_str()),
            ("projectId", resolved_project_id.as_deref().unwrap_or("")),
            ("domain", domain.as_str()),
            ("sitePath", site_path.as_str()),
        ]
    } else {
        vec![
            ("modulePath", module_path.as_str()),
            ("entrypoint", entrypoint.as_str()),
            ("args", args_str.as_str()),
        ]
    };

    let command = template::render(run_command, &vars);

    // Build environment with module-defined env vars + exec context
    let mut env = build_exec_env(
        module_id,
        resolved_project_id.as_deref(),
        resolved_component_id.as_deref(),
        &settings_json,
    );
    if let Some(ref module_env) = runtime.env {
        for (key, value) in module_env {
            let rendered_value = template::render(value, &vars);
            env.push((key.clone(), rendered_value));
        }
    }
    let env_pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let exit_code = execute_local_command_interactive(&command, Some(module_path), Some(&env_pairs));

    Ok(ModuleRunResult {
        exit_code,
        project_id: resolved_project_id,
    })
}

/// Execute a module action (API call).
pub fn run_action(
    module_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    data: Option<&str>,
) -> Result<serde_json::Value> {
    let module = load_module(module_id)
        .ok_or_else(|| Error::other(format!("Module '{}' not found", module_id)))?;

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

    match action.action_type.as_str() {
        "api" => {
            let pid = project_id
                .ok_or_else(|| Error::other("--project is required for API actions"))?;

            let project = project::load(pid)?;
            let client = ApiClient::new(pid, &project.api)?;

            if action.requires_auth.unwrap_or(false) && !client.is_authenticated() {
                return Err(Error::other(
                    "Not authenticated. Run 'homeboy auth login --project <id>' first.",
                ));
            }

            let endpoint = action
                .endpoint
                .as_ref()
                .ok_or_else(|| Error::other("API action missing 'endpoint'"))?;

            let method = action.method.as_deref().unwrap_or("POST");
            let settings = get_module_settings(module_id, Some(pid))?;
            let payload = interpolate_action_payload(action, &selected, &settings)?;

            if method == "GET" {
                client.get(endpoint)
            } else {
                client.post(endpoint, &payload)
            }
        }
        other => Err(Error::other(format!("Unknown action type: {}", other))),
    }
}

/// Get effective module settings from project config.
pub fn get_module_settings(
    module_id: &str,
    project_id: Option<&str>,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut settings = HashMap::new();

    if let Some(pid) = project_id {
        if let Ok(project) = project::load(pid) {
            if let Some(scoped) = project.scoped_modules.as_ref() {
                if let Some(module_scope) = scoped.get(module_id) {
                    for (k, v) in &module_scope.settings {
                        settings.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }

    Ok(settings)
}

/// Build execution environment variables for a module.
pub fn build_exec_env(
    module_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    settings_json: &str,
) -> Vec<(String, String)> {
    let mut env = vec![
        (exec_context::VERSION.to_string(), exec_context::CURRENT_VERSION.to_string()),
        (exec_context::MODULE_ID.to_string(), module_id.to_string()),
        (exec_context::SETTINGS_JSON.to_string(), settings_json.to_string()),
    ];

    if let Some(pid) = project_id {
        env.push((exec_context::PROJECT_ID.to_string(), pid.to_string()));
    }

    if let Some(cid) = component_id {
        env.push((exec_context::COMPONENT_ID.to_string(), cid.to_string()));
    }

    env
}

/// Check if a module is ready (setup complete).
pub fn is_module_ready(module: &ModuleManifest) -> bool {
    let Some(runtime) = module.runtime.as_ref() else {
        return true;
    };

    if let Some(ref ready_check) = runtime.ready_check {
        if let Some(ref module_path) = module.module_path {
            let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
            let vars: Vec<(&str, &str)> = vec![
                ("modulePath", module_path.as_str()),
                ("entrypoint", entrypoint.as_str()),
            ];
            let command = template::render(ready_check, &vars);
            let exit_code = execute_local_command_interactive(&command, Some(module_path), None);
            return exit_code == 0;
        }
        return false;
    }

    true
}

/// Check if a module is compatible with a project.
pub fn is_module_compatible(module: &ModuleManifest, project: Option<&Project>) -> bool {
    let Some(project) = project else {
        return true;
    };

    let Some(ref requires) = module.requires else {
        return true;
    };

    for required_module in &requires.modules {
        if !project.has_module(required_module) {
            return false;
        }
    }

    for component in &requires.components {
        if !project.component_ids.contains(component) {
            return false;
        }
    }

    true
}

/// Check if a module is a symlink (linked, not installed).
pub fn is_module_linked(module_id: &str) -> bool {
    paths::module(module_id)
        .map(|p| p.is_symlink())
        .unwrap_or(false)
}

fn interpolate_action_payload(
    action: &ActionConfig,
    selected: &[serde_json::Value],
    settings: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let payload_template = match &action.payload {
        Some(p) => p,
        None => return Ok(serde_json::Value::Object(serde_json::Map::new())),
    };

    let mut result = serde_json::Map::new();
    for (key, value) in payload_template {
        let interpolated = interpolate_payload_value(value, selected, settings)?;
        result.insert(key.clone(), interpolated);
    }

    Ok(serde_json::Value::Object(result))
}

fn interpolate_payload_value(
    value: &serde_json::Value,
    selected: &[serde_json::Value],
    settings: &HashMap<String, serde_json::Value>,
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
            } else {
                Ok(serde_json::Value::String(template.clone()))
            }
        }
        serde_json::Value::Array(arr) => {
            let interpolated: Result<Vec<serde_json::Value>> = arr
                .iter()
                .map(|v| interpolate_payload_value(v, selected, settings))
                .collect();
            Ok(serde_json::Value::Array(interpolated?))
        }
        serde_json::Value::Object(obj) => {
            let mut result = serde_json::Map::new();
            for (k, v) in obj {
                result.insert(k.clone(), interpolate_payload_value(v, selected, settings)?);
            }
            Ok(serde_json::Value::Object(result))
        }
        _ => Ok(value.clone()),
    }
}

// ============================================================================
// Module Scope - Settings resolution for modules with project/component context
// ============================================================================

pub struct ModuleScope;

impl ModuleScope {
    pub fn effective_settings(
        module_id: &str,
        project: Option<&Project>,
        component: Option<&Component>,
    ) -> HashMap<String, serde_json::Value> {
        let mut settings = HashMap::new();

        if let Some(project) = project {
            if let Some(project_modules) = project.scoped_modules.as_ref() {
                if let Some(project_config) = project_modules.get(module_id) {
                    settings.extend(project_config.settings.clone());
                }
            }
        }

        if let Some(component) = component {
            if let Some(component_modules) = component.scoped_modules.as_ref() {
                if let Some(component_config) = component_modules.get(module_id) {
                    settings.extend(component_config.settings.clone());
                }
            }
        }

        settings
    }

    pub fn effective_settings_validated(
        module: &ModuleManifest,
        project: Option<&Project>,
        component: Option<&Component>,
    ) -> Result<HashMap<String, serde_json::Value>> {
        use crate::module_settings::ModuleSettingsValidator;

        let module_id = module.id.as_str();

        let mut out: HashMap<String, serde_json::Value> = HashMap::new();
        for setting in &module.settings {
            let Some(default_value) = setting.default.clone() else {
                continue;
            };

            out.insert(setting.id.clone(), default_value);
        }

        let project_settings = project
            .and_then(|p| p.scoped_modules.as_ref())
            .and_then(|m| m.get(module_id))
            .map(|c| c.settings.clone())
            .unwrap_or_default();
        let component_settings = component
            .and_then(|c| c.scoped_modules.as_ref())
            .and_then(|m| m.get(module_id))
            .map(|c| c.settings.clone())
            .unwrap_or_default();

        let validator = ModuleSettingsValidator::new(module);
        validator.validate_settings_map("project", &project_settings)?;
        validator.validate_settings_map("component", &component_settings)?;

        for settings in [&project_settings, &component_settings] {
            for (key, value) in settings {
                out.insert(key.clone(), value.clone());
            }
        }

        Ok(out)
    }

    pub fn validate_project_compatibility(
        module: &ModuleManifest,
        project: &Project,
    ) -> Result<()> {
        let Some(requires) = module.requires.as_ref() else {
            return Ok(());
        };

        for required_module in &requires.modules {
            if !project.has_module(required_module) {
                return Err(Error::validation_invalid_argument(
                    "project.modules",
                    format!(
                        "Module '{}' requires module '{}', but project does not have it enabled",
                        module.id, required_module
                    ),
                    None,
                    None,
                ));
            }
        }

        for required in &requires.components {
            if !project.component_ids.iter().any(|c| c == required) {
                return Err(Error::validation_invalid_argument(
                    "project.componentIds",
                    format!(
                        "Module '{}' requires component '{}', but project does not include it",
                        module.id, required
                    ),
                    None,
                    None,
                ));
            }
        }

        Ok(())
    }

    pub fn resolve_component_scope(
        module: &ModuleManifest,
        project: &Project,
        component_id: Option<&str>,
    ) -> Result<Option<String>> {
        let required_components = module
            .requires
            .as_ref()
            .map(|r| &r.components)
            .filter(|c| !c.is_empty());

        let Some(required_components) = required_components else {
            return Ok(component_id.map(str::to_string));
        };

        let matching_component_ids: Vec<String> = required_components
            .iter()
            .filter(|required_id| project.component_ids.iter().any(|id| id == *required_id))
            .cloned()
            .collect();

        if matching_component_ids.is_empty() {
            return Err(Error::validation_invalid_argument(
                "project.componentIds",
                format!(
                    "Module '{}' requires components {:?}; none are configured for this project",
                    module.id, required_components
                ),
                None,
                None,
            ));
        }

        if let Some(component_id) = component_id {
            if !matching_component_ids.iter().any(|c| c == component_id) {
                return Err(Error::validation_invalid_argument(
                    "component",
                    format!(
                        "Module '{}' only supports project components {:?}; --component '{}' is not compatible",
                        module.id, matching_component_ids, component_id
                    ),
                    Some(component_id.to_string()),
                    None,
                ));
            }

            return Ok(Some(component_id.to_string()));
        }

        if matching_component_ids.len() == 1 {
            return Ok(Some(matching_component_ids[0].clone()));
        }

        Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Module '{}' matches multiple project components {:?}; pass --component <id>",
                module.id, matching_component_ids
            ),
            None,
            None,
        ))
    }
}
