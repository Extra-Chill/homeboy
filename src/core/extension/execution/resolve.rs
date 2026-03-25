//! resolve — extracted from execution.rs.

use crate::component::{self, Component};
use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::super::scope::ExtensionScope;
use crate::engine::command::CapturedOutput;
use crate::server::http::ApiClient;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use super::super::runner_contract::RunnerStepFilter;
use super::ExtensionExecutionContext;


pub(crate) fn resolve_extension_context(
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

pub fn resolve_capability_component(
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
