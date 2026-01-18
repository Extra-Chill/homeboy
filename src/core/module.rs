use crate::config::{self, from_str, ConfigEntity};
use crate::error::{Error, Result};
use crate::local_files::{self, FileSystem};
use crate::output::MergeOutput;
use crate::paths;
use crate::slugify;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Unified module manifest that can provide platform behavior AND/OR executable tools.
/// All fields are optional - modules include only what they need.
#[derive(Debug, Clone, Serialize, Deserialize)]

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
    pub deploy_override: Vec<DeployOverride>,
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

pub struct RequirementsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

// Platform behavior configs

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct DatabaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<DatabaseCliConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct DatabaseCliConfig {
    pub tables_command: String,
    pub describe_command: String,
    pub query_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct CliConfig {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir_template: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub settings_flags: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

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

pub struct DeployVerification {
    pub path_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_error_message: Option<String>,
}

fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct DeployOverride {
    pub path_pattern: String,
    #[serde(default = "default_staging_path")]
    pub staging_path: String,
    pub install_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_command: Option<String>,
    #[serde(default)]
    pub skip_permissions_fix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VersionPatternConfig {
    pub extension: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct BuildConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_script: Option<String>,
}

// Executable tool configs (from former modules)

#[derive(Debug, Clone, Serialize, Deserialize)]

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

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

/// Find manifest path for {id}.json in module directory.
fn find_manifest_path(module_dir: &Path, id: &str) -> Option<PathBuf> {
    let manifest_path = manifest_path_for_module(module_dir, id);
    if manifest_path.exists() {
        Some(manifest_path)
    } else {
        None
    }
}

pub fn load_module(id: &str) -> Option<ModuleManifest> {
    let module_dir = paths::module(id).ok()?;
    let manifest_path = find_manifest_path(&module_dir, id)?;

    let content = local_files::local().read(&manifest_path).ok()?;
    let mut manifest: ModuleManifest = from_str(&content).ok()?;
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
                if let Ok(mut manifest) = from_str::<ModuleManifest>(&content) {
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

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<ModuleManifest>(id, json_spec, replace_fields)
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
    /// Filesystem path to the module directory.
    pub const MODULE_PATH: &str = "HOMEBOY_MODULE_PATH";
    /// Filesystem path to the project directory (base_path).
    pub const PROJECT_PATH: &str = "HOMEBOY_PROJECT_PATH";

    /// Current version of the exec context protocol.
    pub const CURRENT_VERSION: &str = "1";
}

// ============================================================================
// Module Execution API
// ============================================================================

use crate::component::{self, Component};
use crate::http::ApiClient;
use crate::project::{self, Project};
use crate::ssh::{execute_local_command_in_dir, execute_local_command_interactive};
use crate::template;
use std::collections::HashMap;

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

pub(crate) enum ModuleExecutionMode {
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
) -> Result<ModuleRunResult> {
    let execution = execute_module_runtime(
        module_id,
        project_id,
        component_id,
        inputs,
        args,
        None,
        None,
        ModuleExecutionMode::Interactive,
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
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;

            if method == "GET" {
                client.get(endpoint)
            } else {
                client.post(endpoint, &payload)
            }
        }
        "command" => {
            let command_template = action
                .command
                .as_ref()
                .ok_or_else(|| Error::other("Command action missing 'command'"))?;
            let settings = get_module_settings(module_id, project_id)?;
            let payload = interpolate_action_payload(action, &selected, &settings, payload)?;
            let module_path = module.module_path.as_deref().unwrap_or(".");
            let vars = vec![("module_path", module_path)];

            let project_base_path = if let Some(pid) = project_id {
                if let Ok(proj) = project::load(pid) {
                    proj.base_path.as_deref().map(|s| s.to_string())
                } else {
                    None
                }
            } else {
                None
            };

            let working_dir = payload
                .get("release")
                .and_then(|r| r.get("local_path"))
                .and_then(|p| p.as_str())
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
            if let Some(scoped) = project.modules.as_ref() {
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

    if requires_project {
        let pid = project_id.ok_or_else(|| {
            Error::other("This module requires a project; pass --project <id>".to_string())
        })?;

        let loaded_project = project::load(pid)?;
        ModuleScope::validate_project_compatibility(module, &loaded_project)?;

        resolved_component_id =
            ModuleScope::resolve_component_scope(module, &loaded_project, component_id)?;

        if let Some(ref comp_id) = resolved_component_id {
            component = Some(component::load(comp_id).map_err(|_| {
                Error::config(format!(
                    "Component '{}' required by module '{}' is not configured",
                    comp_id, module.id
                ))
            })?);
        }

        resolved_project_id = Some(pid.to_string());
        project = Some(loaded_project);
    }

    let settings = ModuleScope::effective_settings(module_id, project.as_ref(), component.as_ref());

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
    let module = load_module(module_id)
        .ok_or_else(|| Error::other(format!("Module '{}' not found", module_id)))?;
    let runtime = module_runtime(&module)?;
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
            if let Some(project_modules) = project.modules.as_ref() {
                if let Some(project_config) = project_modules.get(module_id) {
                    settings.extend(project_config.settings.clone());
                }
            }
        }

        if let Some(component) = component {
            if let Some(component_modules) = component.modules.as_ref() {
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

    let manifest_path = manifest_path_for_module(&source, &module_id);
    if !manifest_path.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("No {}.json found at {}", module_id, source.display()),
            Some(source_path.to_string()),
            None,
        ));
    }

    // Validate manifest is parseable
    let manifest_content = local_files::local().read(&manifest_path)?;
    let _manifest: ModuleManifest = from_str(&manifest_content)?;

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
