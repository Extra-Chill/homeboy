use crate::component;

use super::Project;

pub fn calculate_deploy_readiness(project: &Project) -> (bool, Vec<String>) {
    let mut blockers = Vec::new();

    match &project.server_id {
        None => {
            blockers.push(format!(
                "Missing server_id - set with: homeboy project set {} '{{\"server_id\": \"<server-id>\"}}'",
                project.id
            ));
        }
        Some(sid) if !crate::server::exists(sid) => {
            blockers.push(format!(
                "Server '{}' not found - create with: homeboy server set {} '{{\"host\": \"...\", \"user\": \"...\"}}'",
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
            "Missing base_path - set with: homeboy project set {} '{{\"base_path\": \"/path/to/webroot\"}}'",
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
        let has_deployable = project.components.iter().any(|attachment| {
            if let Ok(comp) = super::resolve_project_component(project, &attachment.id) {
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
