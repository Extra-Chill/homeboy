use crate::project::{Project, ProjectComponentOverrides};

/// Apply a single layer of component overrides to a component.
///
/// Fields from the overrides are applied only when present (Some), allowing
/// each config layer to selectively override specific settings.
fn apply_overrides_layer(
    component: &mut crate::component::Component,
    overrides: &ProjectComponentOverrides,
) {
    if let Some(remote_path) = &overrides.remote_path {
        component.remote_path = remote_path.clone();
    }
    if let Some(build_artifact) = &overrides.build_artifact {
        component.build_artifact = Some(build_artifact.clone());
    }
    if let Some(extract_command) = &overrides.extract_command {
        component.extract_command = Some(extract_command.clone());
    }
    if let Some(remote_owner) = &overrides.remote_owner {
        component.remote_owner = Some(remote_owner.clone());
    }
    if let Some(deploy_strategy) = &overrides.deploy_strategy {
        component.deploy_strategy = Some(deploy_strategy.clone());
    }
    if let Some(git_deploy) = &overrides.git_deploy {
        component.git_deploy = Some(git_deploy.clone());
    }
    if !overrides.hooks.is_empty() {
        component.hooks = overrides.hooks.clone();
    }
    if let Some(scopes) = &overrides.scopes {
        component.scopes = Some(scopes.clone());
    }
    if let Some(cli_path) = &overrides.cli_path {
        component.cli_path = Some(cli_path.clone());
    }
}

/// Apply component overrides with fleet → project cascade.
///
/// Resolution order: component (repo portable config) → fleet defaults → project overrides.
/// Fleet-level overrides provide defaults, project-level overrides take precedence.
pub fn apply_component_overrides(
    component: &crate::component::Component,
    project: &Project,
) -> crate::component::Component {
    let fleet_overrides = resolve_fleet_overrides(project, &component.id);
    let project_overrides = project.component_overrides.get(&component.id);

    if fleet_overrides.is_none() && project_overrides.is_none() {
        return component.clone();
    }

    let mut merged = component.clone();

    // Apply fleet-level overrides first (lowest precedence in the cascade)
    if let Some(overrides) = &fleet_overrides {
        apply_overrides_layer(&mut merged, overrides);
    }

    // Apply project-level overrides on top (highest precedence)
    if let Some(overrides) = project_overrides {
        apply_overrides_layer(&mut merged, overrides);
    }

    merged
}

/// Look up fleet-level component overrides for a project's component.
///
/// Finds the fleet(s) containing this project and returns the first matching
/// fleet-level override for the given component ID. If the project belongs
/// to multiple fleets, the first fleet with an override wins.
fn resolve_fleet_overrides(
    project: &Project,
    component_id: &str,
) -> Option<ProjectComponentOverrides> {
    let fleets = crate::fleet::list().ok()?;

    for fleet in &fleets {
        if fleet.project_ids.contains(&project.id) {
            if let Some(overrides) = fleet.component_overrides.get(component_id) {
                return Some(overrides.clone());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use std::collections::HashMap;

    fn base_component(id: &str) -> Component {
        Component {
            id: id.to_string(),
            remote_path: "original/path".to_string(),
            ..Component::default()
        }
    }

    fn project_with_overrides(
        id: &str,
        overrides: HashMap<String, ProjectComponentOverrides>,
    ) -> Project {
        Project {
            id: id.to_string(),
            component_overrides: overrides,
            ..Default::default()
        }
    }

    #[test]
    fn apply_overrides_layer_sets_remote_path() {
        let mut component = base_component("my-plugin");
        let overrides = ProjectComponentOverrides {
            remote_path: Some("wp-content/plugins/my-plugin".to_string()),
            ..Default::default()
        };

        apply_overrides_layer(&mut component, &overrides);
        assert_eq!(component.remote_path, "wp-content/plugins/my-plugin");
    }

    #[test]
    fn apply_overrides_layer_sets_deploy_strategy() {
        let mut component = base_component("my-plugin");
        let overrides = ProjectComponentOverrides {
            deploy_strategy: Some("git".to_string()),
            ..Default::default()
        };

        apply_overrides_layer(&mut component, &overrides);
        assert_eq!(component.deploy_strategy, Some("git".to_string()));
    }

    #[test]
    fn apply_overrides_layer_skips_none_fields() {
        let mut component = base_component("my-plugin");
        component.deploy_strategy = Some("rsync".to_string());
        let overrides = ProjectComponentOverrides::default();

        apply_overrides_layer(&mut component, &overrides);
        // deploy_strategy should remain unchanged
        assert_eq!(component.deploy_strategy, Some("rsync".to_string()));
        // remote_path should remain unchanged
        assert_eq!(component.remote_path, "original/path");
    }

    #[test]
    fn apply_overrides_layer_replaces_hooks() {
        let mut component = base_component("my-plugin");
        component
            .hooks
            .insert("pre:deploy".to_string(), vec!["echo old".to_string()]);

        let mut hooks = HashMap::new();
        hooks.insert("post:deploy".to_string(), vec!["echo new".to_string()]);
        let overrides = ProjectComponentOverrides {
            hooks,
            ..Default::default()
        };

        apply_overrides_layer(&mut component, &overrides);
        // Hooks should be replaced entirely
        assert!(component.hooks.contains_key("post:deploy"));
        assert!(!component.hooks.contains_key("pre:deploy"));
    }

    #[test]
    fn no_overrides_returns_clone() {
        let component = base_component("my-plugin");
        let project = project_with_overrides("my-project", HashMap::new());

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.remote_path, "original/path");
    }

    #[test]
    fn project_overrides_applied() {
        let component = base_component("my-plugin");

        let mut overrides = HashMap::new();
        overrides.insert(
            "my-plugin".to_string(),
            ProjectComponentOverrides {
                remote_path: Some("wp-content/plugins/my-plugin".to_string()),
                remote_owner: Some("www-data:www-data".to_string()),
                ..Default::default()
            },
        );
        let project = project_with_overrides("my-project", overrides);

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.remote_path, "wp-content/plugins/my-plugin");
        assert_eq!(result.remote_owner, Some("www-data:www-data".to_string()));
    }

    #[test]
    fn unmatched_component_id_not_applied() {
        let component = base_component("my-plugin");

        let mut overrides = HashMap::new();
        overrides.insert(
            "other-plugin".to_string(),
            ProjectComponentOverrides {
                remote_path: Some("wp-content/plugins/other".to_string()),
                ..Default::default()
            },
        );
        let project = project_with_overrides("my-project", overrides);

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.remote_path, "original/path");
    }

    #[test]
    fn apply_overrides_layer_sets_cli_path() {
        let mut component = base_component("my-plugin");
        assert_eq!(component.cli_path, None);

        let overrides = ProjectComponentOverrides {
            cli_path: Some("studio wp".to_string()),
            ..Default::default()
        };

        apply_overrides_layer(&mut component, &overrides);
        assert_eq!(component.cli_path, Some("studio wp".to_string()));
    }

    #[test]
    fn cli_path_override_applied_via_project() {
        let component = base_component("my-plugin");

        let mut overrides = HashMap::new();
        overrides.insert(
            "my-plugin".to_string(),
            ProjectComponentOverrides {
                cli_path: Some("studio wp".to_string()),
                ..Default::default()
            },
        );
        let project = project_with_overrides("my-studio-site", overrides);

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.cli_path, Some("studio wp".to_string()));
    }
}
