use std::collections::HashSet;

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

pub fn validate_deploy_component_local_paths(
    project: &Project,
    component_ids: &[String],
) -> Result<()> {
    if component_ids.is_empty() {
        return validate_component_local_paths(project);
    }

    let mut scoped_ids = component_ids.iter().cloned().collect::<HashSet<_>>();
    let standalone_snapshot = super::StandaloneComponentConfigSnapshot::load();
    for component_id in component_ids {
        validate_component_local_path(project, component_id)?;
        let component = super::resolve_project_component_with_standalone_snapshot(
            project,
            component_id,
            Some(&standalone_snapshot),
        )?;

        scoped_ids.extend(component.deploy_together);
        scoped_ids.extend(
            component
                .artifact_inputs
                .into_iter()
                .map(|input| input.component),
        );
    }

    let mut scoped_blockers = Vec::new();
    let mut hygiene_blockers = Vec::new();
    for attachment in &project.components {
        let Some(blocker) =
            component_local_path_blocker(project, &attachment.id, &attachment.local_path)
        else {
            continue;
        };

        if scoped_ids.contains(&attachment.id) {
            scoped_blockers.push(blocker);
        } else {
            hygiene_blockers.push(blocker);
        }
    }

    for blocker in hygiene_blockers {
        log_status!(
            "deploy",
            "Project component local_path hygiene warning: {}",
            blocker
        );
    }

    if scoped_blockers.is_empty() {
        return Ok(());
    }

    let mut err = Error::validation_invalid_argument(
        "components.local_path",
        "Scoped deploy has component local_path blockers",
        Some(project.id.clone()),
        Some(scoped_blockers.clone()),
    );
    for blocker in scoped_blockers {
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

pub(crate) fn component_local_path_blockers(project: &Project) -> Vec<String> {
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
    use tempfile::TempDir;

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

    fn repo_with_component(id: &str, extra: serde_json::Value) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        let mut component = serde_json::json!({
            "id": id,
            "remote_path": format!("wp-content/plugins/{id}"),
            "build_artifact": format!("dist/{id}.zip")
        });
        let component_obj = component.as_object_mut().expect("component object");
        for (key, value) in extra.as_object().expect("extra object") {
            component_obj.insert(key.clone(), value.clone());
        }
        std::fs::write(dir.path().join("homeboy.json"), component.to_string())
            .expect("write homeboy.json");
        dir
    }

    fn project_with_components(components: Vec<(&str, String)>) -> Project {
        Project {
            id: "site".to_string(),
            server_id: Some("server".to_string()),
            base_path: Some("/srv/site".to_string()),
            components: components
                .into_iter()
                .map(|(id, local_path)| ProjectComponentAttachment {
                    id: id.to_string(),
                    local_path,
                    remote_path: Some(format!("wp-content/plugins/{id}")),
                })
                .collect(),
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

    #[test]
    fn scoped_deploy_validation_ignores_unrelated_missing_component_local_path() {
        let requested = repo_with_component("requested", serde_json::json!({}));
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect("unrelated stale local_path should be hygiene-only");
    }

    #[test]
    fn scoped_deploy_validation_blocks_missing_direct_dependency_local_path() {
        let requested = repo_with_component(
            "requested",
            serde_json::json!({ "deploy_together": ["required"] }),
        );
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "required",
                "/tmp/homeboy-stale-required-component".to_string(),
            ),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect_err("stale direct dependency should block scoped deploy");

        assert!(err.message.contains("Scoped deploy"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Component 'required' local_path '/tmp/homeboy-stale-required-component' does not exist")));
        assert!(!err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy-stale-unrelated-component")));
    }

    #[test]
    fn scoped_deploy_validation_blocks_missing_artifact_input_dependency_local_path() {
        let requested = repo_with_component(
            "requested",
            serde_json::json!({
                "artifact_inputs": [{
                    "component": "producer",
                    "artifact": "dist/producer.zip",
                    "target": "vendor/producer.zip",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }]
            }),
        );
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "producer",
                "/tmp/homeboy-stale-producer-component".to_string(),
            ),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &["requested".to_string()])
            .expect_err("stale artifact producer should block scoped deploy");

        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Component 'producer' local_path '/tmp/homeboy-stale-producer-component' does not exist")));
        assert!(!err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy-stale-unrelated-component")));
    }

    #[test]
    fn full_project_deploy_validation_remains_strict_for_all_components() {
        let requested = repo_with_component("requested", serde_json::json!({}));
        let project = project_with_components(vec![
            ("requested", requested.path().to_string_lossy().to_string()),
            (
                "stale",
                "/tmp/homeboy-stale-unrelated-component".to_string(),
            ),
        ]);

        let err = validate_deploy_component_local_paths(&project, &[])
            .expect_err("full-project deploy should validate every component");

        assert!(err.message.contains("component local_path blockers"));
        assert!(err.hints.iter().any(|hint| hint.message.contains(
            "Component 'stale' local_path '/tmp/homeboy-stale-unrelated-component' does not exist"
        )));
    }
}
