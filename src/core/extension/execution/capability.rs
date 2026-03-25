//! capability — extracted from execution.rs.

use crate::component::{self, Component};
use crate::error::{Error, Result};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use std::path::Path;
use crate::engine::command::CapturedOutput;
use crate::project::{self, Project};
use crate::server::http::ApiClient;
use serde::Serialize;
use std::collections::HashMap;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::super::runner_contract::RunnerStepFilter;
use super::super::scope::ExtensionScope;
use super::load_extension_manifest_from_dir;
use super::PreparedCapabilityRun;
use super::ExtensionExecutionContext;
use super::resolve_capability_component;


pub fn validate_capability_script_exists(
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

pub fn prepare_capability_run(
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
