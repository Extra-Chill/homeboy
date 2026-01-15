use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::json;
use crate::local_files::{self, FileSystem};
use crate::paths;
use crate::slugify;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Unified module manifest that can provide platform behavior AND/OR executable tools.
/// All fields are optional - modules include only what they need.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleManifest {
    // ID derived from filename at runtime, not stored in JSON
    #[serde(default, skip_serializing)]
    pub id: String,

    // Required metadata
    pub name: String,
    pub version: String,

    // Optional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,

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

    // Extensibility: preserve unknown fields for external consumers (GUI, workflows)
    #[serde(
        flatten,
        default,
        skip_serializing_if = "std::collections::HashMap::is_empty"
    )]
    pub extra: std::collections::HashMap<String, serde_json::Value>,

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

impl ConfigEntity for ModuleManifest {
    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn config_path(id: &str) -> Result<PathBuf> {
        paths::module_manifest(id)
    }
    fn config_dir() -> Result<PathBuf> {
        paths::modules()
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::module_not_found(id, suggestions)
    }
    fn entity_type() -> &'static str {
        "module"
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

/// Returns the path to a module's manifest file: {module_dir}/{id}.json
fn manifest_path_for_module(module_dir: &Path, id: &str) -> PathBuf {
    module_dir.join(format!("{}.json", id))
}

/// Find manifest path, supporting both new ({id}.json) and legacy (homeboy.json) naming.
fn find_manifest_path(module_dir: &Path, id: &str) -> Option<PathBuf> {
    let new_path = manifest_path_for_module(module_dir, id);
    if new_path.exists() {
        return Some(new_path);
    }

    // Fallback for legacy modules
    let old_path = module_dir.join("homeboy.json");
    if old_path.exists() {
        return Some(old_path);
    }

    None
}

pub fn load_module(id: &str) -> Option<ModuleManifest> {
    let module_dir = paths::module(id).ok()?;
    let manifest_path = find_manifest_path(&module_dir, id)?;

    let content = local_files::local().read(&manifest_path).ok()?;
    let mut manifest: ModuleManifest = json::from_str(&content).ok()?;
    manifest.id = id.to_string();
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
            // Derive ID from directory name
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(manifest_path) = find_manifest_path(&path, id) else {
                continue;
            };
            if let Ok(content) = local_files::local().read(&manifest_path) {
                if let Ok(mut manifest) = json::from_str::<ModuleManifest>(&content) {
                    manifest.id = id.to_string();
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

// Module config operations (via ConfigEntity trait)

pub fn save_manifest(manifest: &ModuleManifest) -> Result<()> {
    config::save(manifest)
}

pub fn merge(id: Option<&str>, json_spec: &str) -> Result<config::MergeOutput> {
    config::merge::<ModuleManifest>(id, json_spec)
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
use crate::http::ApiClient;
use crate::project::{self, Project};
use crate::ssh::execute_local_command_interactive;
use crate::template;
use std::collections::HashMap;

/// Result of executing a module.
pub struct ModuleRunResult {
    pub exit_code: i32,
    pub project_id: Option<String>,
}

/// Result of running module setup.
pub struct ModuleSetupResult {
    pub exit_code: i32,
}

/// Run a module's setup command (if defined).
pub fn run_setup(module_id: &str) -> Result<ModuleSetupResult> {
    let module = load_module(module_id)
        .ok_or_else(|| Error::other(format!("Module '{}' not found", module_id)))?;

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

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| Error::other("module_path not set".to_string()))?;

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("modulePath", module_path.as_str()),
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
        domain = proj.domain.clone().unwrap_or_default();
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

    let exit_code =
        execute_local_command_interactive(&command, Some(module_path), Some(&env_pairs));

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
            let pid =
                project_id.ok_or_else(|| Error::other("--project is required for API actions"))?;

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
    let Some(ref requires) = module.requires else {
        return true;
    };

    // Required modules must be installed globally
    for required_module in &requires.modules {
        if load_module(required_module).is_none() {
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

    pub fn validate_project_compatibility(
        module: &ModuleManifest,
        project: &Project,
    ) -> Result<()> {
        let Some(requires) = module.requires.as_ref() else {
            return Ok(());
        };

        // Required modules must be installed globally
        for required_module in &requires.modules {
            if load_module(required_module).is_none() {
                return Err(Error::validation_invalid_argument(
                    "modules",
                    format!(
                        "Module '{}' requires module '{}', but it is not installed",
                        module.id, required_module
                    ),
                    None,
                    None,
                ));
            }
        }

        // Required components must be linked to the project
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

// ============================================================================
// Module Lifecycle Operations
// ============================================================================

use crate::git;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub module_id: String,
    pub url: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub module_id: String,
    pub url: String,
    pub path: PathBuf,
}

pub fn slugify_id(value: &str) -> Result<String> {
    slugify::slugify_id(value, "module_id")
}

/// Derive a module ID from a git URL.
pub fn derive_id_from_url(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    let segment = trimmed
        .split('/')
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");

    slugify_id(segment)
}

/// Check if a string looks like a git URL (vs a local path).
pub fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
}

/// Check if a git working directory is clean (no uncommitted changes).
fn is_workdir_clean(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(output) => output.status.success() && output.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Install a module from a git URL or link a local directory.
/// Automatically detects whether source is a URL (git clone) or local path (symlink).
pub fn install(source: &str, id_override: Option<&str>) -> Result<InstallResult> {
    if is_git_url(source) {
        install_from_url(source, id_override)
    } else {
        install_from_path(source, id_override)
    }
}

/// Install a module by cloning from a git repository URL.
fn install_from_url(url: &str, id_override: Option<&str>) -> Result<InstallResult> {
    let module_id = match id_override {
        Some(id) => slugify_id(id)?,
        None => derive_id_from_url(url)?,
    };

    let module_dir = paths::module(&module_id)?;
    if module_dir.exists() {
        return Err(Error::validation_invalid_argument(
            "module_id",
            format!("Module '{}' already exists", module_id),
            Some(module_id),
            None,
        ));
    }

    local_files::ensure_app_dirs()?;
    git::clone_repo(url, &module_dir)?;

    // Migrate manifest to {id}.json naming and add sourceUrl
    let old_manifest_path = module_dir.join("homeboy.json");
    let new_manifest_path = manifest_path_for_module(&module_dir, &module_id);

    if old_manifest_path.exists() {
        let content = local_files::local().read(&old_manifest_path)?;
        let mut manifest: serde_json::Value = json::from_str(&content)?;

        // Add sourceUrl for update tracking
        manifest["sourceUrl"] = serde_json::Value::String(url.to_string());

        // Remove redundant id field (now derived from filename)
        if let Some(obj) = manifest.as_object_mut() {
            obj.remove("id");
        }

        let updated = json::to_string_pretty(&manifest)?;
        local_files::local().write(&new_manifest_path, &updated)?;

        // Remove old homeboy.json
        let _ = std::fs::remove_file(&old_manifest_path);
    }

    // Auto-run setup if module defines a setup_command
    if let Some(module) = load_module(&module_id) {
        if module
            .runtime
            .as_ref()
            .is_some_and(|r| r.setup_command.is_some())
        {
            let _ = run_setup(&module_id);
        }
    }

    Ok(InstallResult {
        module_id,
        url: url.to_string(),
        path: module_dir,
    })
}

/// Install a module by symlinking a local directory.
fn install_from_path(source_path: &str, id_override: Option<&str>) -> Result<InstallResult> {
    let source = Path::new(source_path);

    // Resolve to absolute path
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::internal_io(e.to_string(), Some("get current dir".to_string())))?
            .join(source)
    };

    if !source.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("Path does not exist: {}", source.display()),
            Some(source_path.to_string()),
            None,
        ));
    }

    // Derive module ID from directory name or override
    let dir_name = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source",
            "Could not determine directory name",
            Some(source_path.to_string()),
            None,
        )
    })?;

    let module_id = match id_override {
        Some(id) => slugify_id(id)?,
        None => slugify_id(dir_name)?,
    };

    // Check for manifest: prefer {id}.json, fall back to homeboy.json
    let manifest_path = find_manifest_path(&source, &module_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source",
            format!(
                "No {}.json or homeboy.json found at {}",
                module_id,
                source.display()
            ),
            Some(source_path.to_string()),
            None,
        )
    })?;

    // Validate manifest is parseable
    let manifest_content = local_files::local().read(&manifest_path)?;
    let _manifest: ModuleManifest = json::from_str(&manifest_content)?;

    let module_dir = paths::module(&module_id)?;
    if module_dir.exists() {
        return Err(Error::validation_invalid_argument(
            "module_id",
            format!(
                "Module '{}' already exists at {}",
                module_id,
                module_dir.display()
            ),
            Some(module_id),
            None,
        ));
    }

    local_files::ensure_app_dirs()?;

    // Create symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, &module_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&source, &module_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    Ok(InstallResult {
        module_id,
        url: source.to_string_lossy().to_string(),
        path: module_dir,
    })
}

/// Update an installed module by pulling latest changes.
pub fn update(module_id: &str, force: bool) -> Result<UpdateResult> {
    let module_dir = paths::module(module_id)?;
    if !module_dir.exists() {
        return Err(Error::module_not_found(module_id.to_string(), vec![]));
    }

    // Linked modules are managed externally
    if is_module_linked(module_id) {
        return Err(Error::validation_invalid_argument(
            "module_id",
            format!(
                "Module '{}' is linked. Update the source directory directly.",
                module_id
            ),
            Some(module_id.to_string()),
            None,
        ));
    }

    if !force && !is_workdir_clean(&module_dir) {
        return Err(Error::validation_invalid_argument(
            "module_id",
            "Module has uncommitted changes; update may overwrite them. Use --force to proceed.",
            Some(module_id.to_string()),
            None,
        ));
    }

    let module = load_module(module_id)
        .ok_or_else(|| Error::module_not_found(module_id.to_string(), vec![]))?;

    let source_url = module.source_url.ok_or_else(|| {
        Error::validation_invalid_argument(
            "module_id",
            format!(
                "Module '{}' has no sourceUrl. Reinstall with 'homeboy module install <url>'.",
                module_id
            ),
            Some(module_id.to_string()),
            None,
        )
    })?;

    git::pull_repo(&module_dir)?;

    // Auto-run setup if module defines a setup_command
    if let Some(module) = load_module(module_id) {
        if module
            .runtime
            .as_ref()
            .is_some_and(|r| r.setup_command.is_some())
        {
            let _ = run_setup(module_id);
        }
    }

    Ok(UpdateResult {
        module_id: module_id.to_string(),
        url: source_url,
        path: module_dir,
    })
}

/// Uninstall a module. Automatically detects symlinks vs cloned directories.
/// - Symlinked modules: removes symlink only (source preserved)
/// - Cloned modules: removes directory entirely
pub fn uninstall(module_id: &str) -> Result<PathBuf> {
    let module_dir = paths::module(module_id)?;
    if !module_dir.exists() {
        return Err(Error::module_not_found(module_id.to_string(), vec![]));
    }

    if module_dir.is_symlink() {
        // Symlinked module: just remove the symlink, source directory is preserved
        std::fs::remove_file(&module_dir)
            .map_err(|e| Error::internal_io(e.to_string(), Some("remove symlink".to_string())))?;
    } else {
        // Cloned module: remove the directory
        std::fs::remove_dir_all(&module_dir).map_err(|e| {
            Error::internal_io(e.to_string(), Some("remove module directory".to_string()))
        })?;
    }

    Ok(module_dir)
}
