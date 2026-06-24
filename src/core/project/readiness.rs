use crate::core::component;
use crate::core::error::{Error, Result};

use super::Project;

pub fn calculate_deploy_readiness(project: &Project) -> (bool, Vec<String>) {
    let mut blockers = Vec::new();

    match &project.server_id {
        None => {
            blockers.push(format!(
                "Missing server_id - set with: homeboy project set {} --json '{{\"server_id\": \"<server-id>\"}}'",
                project.id
            ));
        }
        Some(sid) if !crate::core::server::exists(sid) => {
            blockers.push(format!(
                "Server '{}' not found - create with: homeboy server set {} --json '{{\"host\": \"...\", \"user\": \"...\"}}'",
                sid, sid
            ));
        }
        _ => {}
    }

    if project
        .base_path
        .as_ref()
        .map(|p| p.is_empty())
        .unwrap_or(true)
    {
        blockers.push(format!(
            "Missing base_path - set with: homeboy project set {} --json '{{\"base_path\": \"/path/to/webroot\"}}'",
            project.id
        ));
    }

    if project.components.is_empty() {
        blockers.push(format!(
            "No components linked - add with: homeboy project components add {} <component-id> or attach a repo: homeboy project components attach-path {} <component-id> <path>",
            project.id,
            project.id
        ));
    } else {
        blockers.extend(component_local_path_blockers(project));

        let standalone_snapshot = super::StandaloneComponentConfigSnapshot::load();
        let has_deployable = project.components.iter().any(|attachment| {
            if let Ok(comp) = super::resolve_project_component_with_standalone_snapshot(
                project,
                &attachment.id,
                Some(&standalone_snapshot),
            ) {
                let is_git = comp.deploy_strategy.as_deref() == Some("git");
                let has_artifact = component::resolve_artifact(&comp).is_some();
                is_git || has_artifact
            } else {
                false
            }
        });

        if !has_deployable {
            blockers.push(format!(
                "No deployable components - {} component(s) exist but none have a build artifact or deploy strategy configured",
                project.components.len()
            ));
        }
    }

    (blockers.is_empty(), blockers)
}

pub fn validate_component_local_paths(project: &Project) -> Result<()> {
    let blockers = component_local_path_blockers(project);
    if blockers.is_empty() {
        return Ok(());
    }

    let mut err = Error::validation_invalid_argument(
        "components.local_path",
        "Project has component local_path blockers",
        Some(project.id.clone()),
        Some(blockers.clone()),
    );
    for blocker in blockers {
        err = err.with_hint(blocker);
    }
    Err(err)
}

pub fn validate_component_local_path(project: &Project, component_id: &str) -> Result<()> {
    let blockers = project
        .components
        .iter()
        .filter(|attachment| attachment.id == component_id)
        .filter_map(|attachment| {
            component_local_path_blocker(project, &attachment.id, &attachment.local_path)
        })
        .collect::<Vec<_>>();

    if blockers.is_empty() {
        return Ok(());
    }

    let mut err = Error::validation_invalid_argument(
        "components.local_path",
        "Project component has a missing local_path",
        Some(project.id.clone()),
        Some(blockers.clone()),
    );
    for blocker in blockers {
        err = err.with_hint(blocker);
    }
    Err(err)
}

fn component_local_path_blockers(project: &Project) -> Vec<String> {
    project
        .components
        .iter()
        .filter_map(|attachment| {
            component_local_path_blocker(project, &attachment.id, &attachment.local_path)
        })
        .collect()
}

fn component_local_path_blocker(
    project: &Project,
    component_id: &str,
    local_path: &str,
) -> Option<String> {
    let trimmed = local_path.trim();
    if trimmed.is_empty() {
        return Some(format!(
            "Component '{}' is missing local_path - attach a checkout with: homeboy project components attach-path {} <local-path>",
            component_id, project.id
        ));
    }

    let expanded = shellexpand::tilde(trimmed);
    let path = std::path::Path::new(expanded.as_ref());
    if path.exists() {
        return None;
    }

    Some(format!(
        "Component '{}' local_path '{}' does not exist - update it with: homeboy project components attach-path {} <local-path>",
        component_id, trimmed, project.id
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project::ProjectComponentAttachment;

    fn project_with_component(local_path: String) -> Project {
        Project {
            id: "site".to_string(),
            server_id: Some("server".to_string()),
            base_path: Some("/srv/site".to_string()),
            components: vec![ProjectComponentAttachment {
                id: "plugin".to_string(),
                local_path,
                remote_path: Some("wp-content/plugins/plugin".to_string()),
            }],
            ..Project::default()
        }
    }

    #[test]
    fn project_show_readiness_blocks_missing_component_local_path() {
        let project = project_with_component("/tmp/homeboy-missing-component-path".to_string());

        let (ready, blockers) = calculate_deploy_readiness(&project);

        assert!(!ready);
        assert!(blockers.iter().any(|blocker| {
            blocker.contains("Component 'plugin' local_path '/tmp/homeboy-missing-component-path' does not exist")
        }));
    }

    #[test]
    fn deploy_readiness_validation_fails_closed_on_missing_component_local_path() {
        let project = project_with_component("/tmp/homeboy-missing-component-path".to_string());

        let err = validate_component_local_paths(&project).expect_err("missing path should block");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("component local_path blockers"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("local_path '/tmp/homeboy-missing-component-path' does not exist")));
    }
}
