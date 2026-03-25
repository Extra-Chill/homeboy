//! projects — extracted from component.rs.

use serde_json::Value;
use std::path::Path;
use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use super::super::{CmdResult, DynamicSetArgs};
use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::EntityCrudOutput;
use super::ComponentExtra;
use super::ComponentOutput;


/// Suggest a project for a newly created component based on sibling components.
///
/// Checks whether any existing project has components whose local_path shares the
/// same parent directory as the new component's path. If a project is found with
/// siblings in the same workspace directory, it's the most likely target.
pub(crate) fn suggest_project_for_path(local_path: &str) -> Option<String> {
    let new_parent = Path::new(local_path).parent()?;
    let projects = project::list().ok()?;

    for project in &projects {
        for attachment in &project.components {
            if let Some(existing_parent) = Path::new(&attachment.local_path).parent() {
                if existing_parent == new_parent {
                    return Some(project.id.clone());
                }
            }
        }
    }

    None
}

pub(crate) fn list() -> CmdResult<ComponentOutput> {
    let components: Vec<Value> = component::inventory()?
        .into_iter()
        .map(|component| {
            let mut value = serde_json::to_value(&component).map_err(|error| {
                homeboy::Error::validation_invalid_argument(
                    "component",
                    "Failed to serialize component",
                    Some(error.to_string()),
                    None,
                )
            })?;
            if let Value::Object(ref mut map) = value {
                map.insert("id".to_string(), Value::String(component.id.clone()));
                // Always surface remote_owner so missing config is visible (#602).
                // Serde skips None fields, but for list output we want explicit null
                // so users can audit which components are missing this critical config.
                map.entry("remote_owner".to_string()).or_insert(Value::Null);
            }
            Ok(value)
        })
        .collect::<homeboy::Result<Vec<Value>>>()?;

    Ok((
        ComponentOutput {
            command: "component.list".to_string(),
            entities: components,
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn projects(id: &str) -> CmdResult<ComponentOutput> {
    let project_ids = component::associated_projects(id)?;

    let mut projects_list = Vec::new();
    for pid in &project_ids {
        if let Ok(p) = project::load(pid) {
            projects_list.push(p);
        }
    }

    Ok((
        ComponentOutput {
            command: "component.projects".to_string(),
            id: Some(id.to_string()),
            extra: ComponentExtra {
                project_ids: Some(project_ids),
                projects: Some(projects_list),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn shared(id: Option<&str>) -> CmdResult<ComponentOutput> {
    if let Some(component_id) = id {
        // Show projects for a specific component
        let project_ids = component::associated_projects(component_id)?;
        let mut shared_map = std::collections::HashMap::new();
        shared_map.insert(component_id.to_string(), project_ids);

        Ok((
            ComponentOutput {
                command: "component.shared".to_string(),
                id: Some(component_id.to_string()),
                extra: ComponentExtra {
                    shared: Some(shared_map),
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ))
    } else {
        // Show all components and their projects
        let shared_map = component::shared_components()?;

        Ok((
            ComponentOutput {
                command: "component.shared".to_string(),
                extra: ComponentExtra {
                    shared: Some(shared_map),
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ))
    }
}
