//! helpers — extracted from execution.rs.

use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use std::collections::HashMap;
use super::super::load_extension;
use crate::component::{self, Component};
use crate::engine::command::CapturedOutput;
use crate::project::{self, Project};
use crate::server::http::ApiClient;
use serde::Serialize;
use std::path::Path;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::super::runner_contract::RunnerStepFilter;
use super::super::scope::ExtensionScope;
use super::ExtensionSetupResult;


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

pub(crate) fn serialize_settings(settings: &HashMap<String, serde_json::Value>) -> Result<String> {
    serde_json::to_string(settings).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize extension settings".to_string()),
        )
    })
}
