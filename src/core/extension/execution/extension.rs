//! extension — extracted from execution.rs.

use crate::component::{self, Component};
use crate::engine::local_files;
use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use std::path::Path;
use super::super::load_extension;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use crate::engine::command::CapturedOutput;
use crate::server::http::ApiClient;
use serde::Serialize;
use std::collections::HashMap;
use super::super::runner_contract::RunnerStepFilter;
use super::super::scope::ExtensionScope;
use super::ExtensionStepFilter;
use super::ExtensionExecutionMode;
use super::ExtensionReadyStatus;
use super::ExtensionRunResult;


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

pub(crate) fn extension_runtime(extension: &ExtensionManifest) -> Result<&RuntimeConfig> {
    extension.runtime().ok_or_else(|| {
        Error::config(format!(
            "Extension '{}' does not have a runtime configuration and cannot be executed",
            extension.id
        ))
    })
}

pub fn load_extension_manifest_from_dir(extension_path: &Path) -> Result<serde_json::Value> {
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
