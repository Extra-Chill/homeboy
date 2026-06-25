use std::path::Path;

use serde::Serialize;

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::project::{
    component_local_path_blockers, load, resolve_project_components, Project,
    ProjectComponentAttachment,
};

use super::{
    attach_discovered_component_path, clear_component_attachments, project_component_ids,
    remove_components, set_component_attachments,
};

#[derive(Debug, Clone, Serialize)]
pub struct ProjectComponentsOutput {
    pub action: String,
    pub project_id: String,
    pub component_ids: Vec<String>,
    pub component_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached_component_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<Component>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn list_components(project_id: &str) -> Result<ProjectComponentsOutput> {
    let project = load(project_id)?;
    build_components_output(project_id, "list", &project)
}

pub fn set_components(project_id: &str, json_spec: &str) -> Result<ProjectComponentsOutput> {
    let raw = crate::core::config::read_json_spec_to_string(json_spec)?;
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
    let attached_component_id = attach_discovered_component_path(project_id, local_path)?;
    let project = load(project_id)?;
    build_components_summary(
        project_id,
        "attach_path",
        &project,
        Some(attached_component_id),
        Some(local_path.to_string_lossy().to_string()),
    )
}

pub fn remove_components_report(
    project_id: &str,
    component_ids: Vec<String>,
) -> Result<ProjectComponentsOutput> {
    remove_components(project_id, component_ids)?;
    let project = load(project_id)?;
    build_components_summary(project_id, "remove", &project, None, None)
}

pub fn clear_components(project_id: &str) -> Result<ProjectComponentsOutput> {
    clear_component_attachments(project_id)?;
    let project = load(project_id)?;
    build_components_summary(project_id, "clear", &project, None, None)
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
        component_count: project.components.len(),
        attached_component_id: None,
        attached_path: None,
        components,
        warnings: Vec::new(),
    })
}

fn build_components_summary(
    project_id: &str,
    action: &str,
    project: &Project,
    attached_component_id: Option<String>,
    attached_path: Option<String>,
) -> Result<ProjectComponentsOutput> {
    Ok(ProjectComponentsOutput {
        action: action.to_string(),
        project_id: project_id.to_string(),
        component_ids: project_component_ids(project),
        component_count: project.components.len(),
        attached_component_id,
        attached_path,
        components: Vec::new(),
        warnings: component_local_path_blockers(project),
    })
}

#[cfg(test)]
mod tests {
    use super::{build_components_summary, remove_components_report};
    use crate::core::project::{load, save, Project, ProjectComponentAttachment};
    use crate::test_support::with_isolated_home;

    #[test]
    fn attach_path_summary_omits_resolved_component_payload() {
        let project = Project {
            id: "site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "plugin".to_string(),
                local_path: "/repo/plugin".to_string(),
                remote_path: None,
            }],
            ..Default::default()
        };

        let output = build_components_summary(
            "site",
            "attach_path",
            &project,
            Some("plugin".to_string()),
            Some("/repo/plugin".to_string()),
        )
        .expect("summary output");

        assert_eq!(output.component_ids, vec!["plugin"]);
        assert_eq!(output.component_count, 1);
        assert_eq!(output.attached_component_id.as_deref(), Some("plugin"));
        assert!(output.components.is_empty());
        assert!(!output.warnings.is_empty());
    }

    #[test]
    fn remove_component_succeeds_when_unrelated_remaining_path_is_stale() {
        with_isolated_home(|_| {
            save(&Project {
                id: "site".to_string(),
                components: vec![
                    ProjectComponentAttachment {
                        id: "remove-me".to_string(),
                        local_path: "/tmp/homeboy-remove-me-missing".to_string(),
                        remote_path: None,
                    },
                    ProjectComponentAttachment {
                        id: "stale-remaining".to_string(),
                        local_path: "/tmp/homeboy-stale-remaining-missing".to_string(),
                        remote_path: None,
                    },
                ],
                ..Default::default()
            })
            .expect("save project");

            let output = remove_components_report("site", vec!["remove-me".to_string()])
                .expect("remove should not resolve unrelated stale component paths");

            assert_eq!(output.component_ids, vec!["stale-remaining"]);
            assert_eq!(output.component_count, 1);
            assert!(output.components.is_empty());
            assert!(output.warnings.iter().any(|warning| warning.contains(
                "Component 'stale-remaining' local_path '/tmp/homeboy-stale-remaining-missing' does not exist"
            )));
            assert!(!output
                .warnings
                .iter()
                .any(|warning| warning.contains("remove-me")));

            let project = load("site").expect("project still loads");
            assert_eq!(project.components.len(), 1);
            assert_eq!(project.components[0].id, "stale-remaining");
        });
    }
}
