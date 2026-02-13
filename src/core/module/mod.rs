mod manifest;
mod execution;
mod scope;
mod lifecycle;
mod runner;

pub mod exec_context;

// Re-export runner types
pub use runner::{ModuleRunner, RunnerOutput};

// Re-export manifest types
pub use manifest::{
    ActionConfig, ActionType, BuildConfig, CliConfig, DatabaseCliConfig, DatabaseConfig,
    DeployOverride, DeployVerification, DiscoveryConfig, HttpMethod, InputConfig, LintConfig,
    ModuleManifest, OutputConfig, OutputSchema, RequirementsConfig, RuntimeConfig, SelectOption,
    SettingConfig, TestConfig, VersionPatternConfig,
};

// Re-export execution types and functions
pub use execution::{
    build_exec_env, is_module_compatible, is_module_ready, module_ready_status,
    ModuleExecutionMode, ModuleReadyStatus, ModuleRunResult, ModuleSetupResult,
    run_action, run_module, run_setup,
};
pub(crate) use execution::execute_action;

// Re-export scope types
pub use scope::ModuleScope;

// Re-export lifecycle types and functions
pub use lifecycle::{
    check_update_available, derive_id_from_url, install, is_git_url, slugify_id, uninstall, update,
    InstallResult, UpdateAvailable, UpdateResult,
};

// Module loader functions

use crate::config;
use crate::error::Result;
use crate::output::MergeOutput;
use crate::paths;
use std::path::PathBuf;

pub fn load_module(id: &str) -> Result<ModuleManifest> {
    let mut manifest = config::load::<ModuleManifest>(id)?;
    let module_dir = paths::module(id)?;
    manifest.module_path = Some(module_dir.to_string_lossy().to_string());
    Ok(manifest)
}

pub fn load_all_modules() -> Result<Vec<ModuleManifest>> {
    let modules = config::list::<ModuleManifest>()?;
    let mut modules_with_paths = Vec::new();
    for mut module in modules {
        let module_dir = paths::module(&module.id)?;
        module.module_path = Some(module_dir.to_string_lossy().to_string());
        modules_with_paths.push(module);
    }
    Ok(modules_with_paths)
}

pub fn find_module_by_tool(tool: &str) -> Option<ModuleManifest> {
    load_all_modules()
        .ok()
        .and_then(|modules| modules.into_iter().find(|m| m.cli.as_ref().is_some_and(|c| c.tool == tool)))
}

pub fn module_path(id: &str) -> PathBuf {
    paths::module(id).unwrap_or_else(|_| PathBuf::from(id))
}

pub fn available_module_ids() -> Vec<String> {
    config::list_ids::<ModuleManifest>().unwrap_or_default()
}

pub fn save_manifest(manifest: &ModuleManifest) -> Result<()> {
    config::save(manifest)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<ModuleManifest>(id, json_spec, replace_fields)
}

/// Check if a module is a symlink (linked, not installed).
pub fn is_module_linked(module_id: &str) -> bool {
    paths::module(module_id)
        .map(|p| p.is_symlink())
        .unwrap_or(false)
}

/// Check if any of the component's linked modules provide build configuration.
/// When true, the component's explicit build_command becomes optional.
pub fn module_provides_build(component: &crate::component::Component) -> bool {
    let modules = match &component.modules {
        Some(m) => m,
        None => return false,
    };

    for module_id in modules.keys() {
        if let Ok(module) = load_module(module_id) {
            if module.has_build() {
                return true;
            }
        }
    }
    false
}
