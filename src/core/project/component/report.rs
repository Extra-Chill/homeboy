use std::path::Path;

use serde::Serialize;

use crate::component::Component;
use crate::error::{Error, Result};
use crate::project::{load, resolve_project_components, Project, ProjectComponentAttachment};

use super::{
    attach_discovered_component_path, clear_component_attachments, project_component_ids,
    remove_components, set_component_attachments,
};

#[derive(Debug, Clone, Serialize)]
pub struct ProjectComponentsOutput {
    pub action: String,
    pub project_id: String,
    pub component_ids: Vec<String>,
    pub components: Vec<Component>,
}

pub fn list_components(project_id: &str) -> Result<ProjectComponentsOutput> {
    let project = load(project_id)?;
    build_components_output(project_id, "list", &project)
}

pub fn set_components(project_id: &str, json_spec: &str) -> Result<ProjectComponentsOutput> {
    let raw = crate::config::read_json_spec_to_string(json_spec)?;
    let attachments: Vec<ProjectComponentAttachment> = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse project component attachments".to_string()),
            None,
        )
    })?;

    set_component_attachments(project_id, attachments)?;
    let project = load(project_id)?;
    build_components_output(project_id, "set", &project)
}

pub fn attach_component_path_report(
    project_id: &str,
    local_path: &Path,
) -> Result<ProjectComponentsOutput> {
    attach_discovered_component_path(project_id, local_path)?;
    let project = load(project_id)?;
    build_components_output(project_id, "attach_path", &project)
}

pub fn remove_components_report(
    project_id: &str,
    component_ids: Vec<String>,
) -> Result<ProjectComponentsOutput> {
    remove_components(project_id, component_ids)?;
    let project = load(project_id)?;
    build_components_output(project_id, "remove", &project)
}

pub fn clear_components(project_id: &str) -> Result<ProjectComponentsOutput> {
    clear_component_attachments(project_id)?;
    let project = load(project_id)?;
    build_components_output(project_id, "clear", &project)
}

fn build_components_output(
    project_id: &str,
    action: &str,
    project: &Project,
) -> Result<ProjectComponentsOutput> {
    let components = resolve_project_components(project)?;

    Ok(ProjectComponentsOutput {
        action: action.to_string(),
        project_id: project_id.to_string(),
        component_ids: project_component_ids(project),
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_list_components_default_path() {
        let project_id = "";
        let _result = list_components(&project_id);
    }

    #[test]
    fn test_set_components_default_path() {
        let project_id = "";
        let json_spec = "";
        let _result = set_components(&project_id, &json_spec);
    }

    #[test]
    fn test_set_components_some_parse_project_component_attachments_to_string() {
        let project_id = "";
        let json_spec = "";
        let _result = set_components(&project_id, &json_spec);
    }

    #[test]
    fn test_set_components_default_path_2() {
        let project_id = "";
        let json_spec = "";
        let _result = set_components(&project_id, &json_spec);
    }

    #[test]
    fn test_set_components_default_path_3() {
        let project_id = "";
        let json_spec = "";
        let _result = set_components(&project_id, &json_spec);
    }

    #[test]
    fn test_set_components_default_path_4() {
        let project_id = "";
        let json_spec = "";
        let _result = set_components(&project_id, &json_spec);
    }

    #[test]
    fn test_attach_component_path_report_default_path() {
        let project_id = "";
        let local_path = Path::new("");
        let _result = attach_component_path_report(&project_id, &local_path);
    }

    #[test]
    fn test_attach_component_path_report_default_path_2() {
        let project_id = "";
        let local_path = Path::new("");
        let _result = attach_component_path_report(&project_id, &local_path);
    }

    #[test]
    fn test_remove_components_report_default_path() {
        let project_id = "";
        let component_ids = Vec::new();
        let _result = remove_components_report(&project_id, component_ids);
    }

    #[test]
    fn test_remove_components_report_default_path_2() {
        let project_id = "";
        let component_ids = Vec::new();
        let _result = remove_components_report(&project_id, component_ids);
    }

    #[test]
    fn test_clear_components_default_path() {
        let project_id = "";
        let _result = clear_components(&project_id);
    }

    #[test]
    fn test_clear_components_default_path_2() {
        let project_id = "";
        let _result = clear_components(&project_id);
    }

}
