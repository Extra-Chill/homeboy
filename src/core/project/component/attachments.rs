use std::path::Path;

use crate::error::{Error, Result};

use super::discovery::infer_attached_component_id;
use crate::project::{load, save, Project, ProjectComponentAttachment};

fn component_ids_from_attachments(components: &[ProjectComponentAttachment]) -> Vec<String> {
    components
        .iter()
        .map(|component| component.id.clone())
        .collect()
}

pub fn project_component_ids(project: &Project) -> Vec<String> {
    component_ids_from_attachments(&project.components)
}

pub fn has_component(project: &Project, component_id: &str) -> bool {
    project
        .components
        .iter()
        .any(|component| component.id == component_id)
}

pub fn set_component_attachments(
    project_id: &str,
    components: Vec<ProjectComponentAttachment>,
) -> Result<Vec<String>> {
    if components.is_empty() {
        return Err(Error::validation_invalid_argument(
            "components",
            "At least one component attachment is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for component in components {
        if component.local_path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "components.local_path",
                "Project component attachments require a non-empty local_path",
                Some(project_id.to_string()),
                None,
            ));
        }
        if seen.insert(component.id.clone()) {
            deduped.push(component);
        }
    }

    let mut project = load(project_id)?;
    project.components = deduped;
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn remove_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut project = load(project_id)?;

    let mut missing = Vec::new();
    for id in &component_ids {
        if !has_component(&project, id) {
            missing.push(id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "Component IDs not attached to project",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    project
        .components
        .retain(|component| !component_ids.contains(&component.id));
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn clear_component_attachments(project_id: &str) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    project.components.clear();
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn attach_component_path(project_id: &str, component_id: &str, local_path: &str) -> Result<()> {
    let mut project = load(project_id)?;

    if let Some(component) = project.components.iter_mut().find(|c| c.id == component_id) {
        component.local_path = local_path.to_string();
    } else {
        project.components.push(ProjectComponentAttachment {
            id: component_id.to_string(),
            local_path: local_path.to_string(),
        });
    }

    save(&project)
}

pub fn attach_discovered_component_path(project_id: &str, local_path: &Path) -> Result<String> {
    let component_id = infer_attached_component_id(local_path)?;
    attach_component_path(project_id, &component_id, &local_path.to_string_lossy())?;
    Ok(component_id)
}
