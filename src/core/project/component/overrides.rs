use crate::core::project::{Project, ProjectComponentOverrides};

/// Apply component overrides with fleet → project cascade.
///
/// Resolution order: component (repo portable config) → fleet defaults → project overrides.
/// Fleet-level overrides provide defaults, project-level overrides take precedence.
///
/// `cli_path` has an extra fallback step: if no explicit override at any layer
/// sets it, the project-scoped `Project::cli_path` fills it in via
/// [`crate::core::project::project_cli_path`]. This makes "every component on
/// this site uses the same wrapper CLI" a one-line project config instead of a
/// per-component repeat. Component-level `cli_path` still wins as the
/// most-specific escape hatch.
pub fn apply_component_overrides(
    component: &crate::core::component::Component,
    project: &Project,
) -> crate::core::component::Component {
    let fleet_overrides = resolve_fleet_overrides(project, &component.id);
    let project_overrides = project.component_overrides.get(&component.id);
    let project_cli_fallback = crate::core::project::project_cli_path(project);

    if fleet_overrides.is_none() && project_overrides.is_none() && project_cli_fallback.is_none() {
        return component.clone();
    }

    let mut merged = component.clone();

    // Apply fleet-level overrides first (lowest precedence in the cascade)
    if let Some(overrides) = &fleet_overrides {
        overrides.apply_to_component(&mut merged);
    }

    // Apply project-level component overrides on top (highest precedence
    // among explicit overrides)
    if let Some(overrides) = project_overrides {
        overrides.apply_to_component(&mut merged);
    }

    // cli_path-only fallback: project-scoped CLI path fills in the gap when no
    // explicit override at any layer set it. This is intentionally last so any
    // explicit override above wins.
    if merged.cli_path.is_none() {
        if let Some(cli_path) = project_cli_fallback {
            merged.cli_path = Some(cli_path);
        }
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
    let fleets = crate::core::fleet::list().ok()?;

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
    use crate::core::component::Component;
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
    fn component_override_config_sets_remote_path() {
        let mut component = base_component("my-plugin");
        let overrides = ProjectComponentOverrides {
            remote_path: Some("wp-content/plugins/my-plugin".to_string()),
            ..Default::default()
        };

        overrides.apply_to_component(&mut component);
        assert_eq!(component.remote_path, "wp-content/plugins/my-plugin");
    }

    #[test]
    fn component_override_config_sets_deploy_strategy() {
        let mut component = base_component("my-plugin");
        let overrides = ProjectComponentOverrides {
            deploy_strategy: Some("git".to_string()),
            ..Default::default()
        };

        overrides.apply_to_component(&mut component);
        assert_eq!(component.deploy_strategy, Some("git".to_string()));
    }

    #[test]
    fn component_override_config_skips_none_fields() {
        let mut component = base_component("my-plugin");
        component.deploy_strategy = Some("rsync".to_string());
        let overrides = ProjectComponentOverrides::default();

        overrides.apply_to_component(&mut component);
        // deploy_strategy should remain unchanged
        assert_eq!(component.deploy_strategy, Some("rsync".to_string()));
        // remote_path should remain unchanged
        assert_eq!(component.remote_path, "original/path");
    }

    #[test]
    fn component_override_config_replaces_hooks() {
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

        overrides.apply_to_component(&mut component);
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
    fn project_component_overrides_parse_existing_json_shape() {
        let project: Project = serde_json::from_str(
            r#"{
                "component_overrides": {
                    "my-plugin": {
                        "remote_path": "wp-content/plugins/my-plugin",
                        "build_artifact": "dist/my-plugin.zip",
                        "extract_command": "unzip -o {{artifact}}",
                        "remote_owner": "www-data:www-data",
                        "deploy_strategy": "git",
                        "git_deploy": { "remote": "deploy", "branch": "stable" },
                        "hooks": { "post:deploy": ["wp cache flush"] },
                        "scopes": { "deploy": { "include": ["src/**"] } },
                        "artifact_inputs": [
                            { "component": "builder", "artifact": "build.zip", "target": "dist/build.zip" }
                        ],
                        "cli_path": "lando wp"
                    }
                }
            }"#,
        )
        .expect("existing project override shape should parse");

        let mut component = base_component("my-plugin");
        let overrides = project
            .component_overrides
            .get("my-plugin")
            .expect("override entry");
        overrides.apply_to_component(&mut component);

        assert_eq!(component.remote_path, "wp-content/plugins/my-plugin");
        assert_eq!(
            component.build_artifact.as_deref(),
            Some("dist/my-plugin.zip")
        );
        assert_eq!(
            component.extract_command.as_deref(),
            Some("unzip -o {{artifact}}")
        );
        assert_eq!(component.remote_owner.as_deref(), Some("www-data:www-data"));
        assert_eq!(component.deploy_strategy.as_deref(), Some("git"));
        assert_eq!(
            component
                .git_deploy
                .as_ref()
                .map(|config| config.remote.as_str()),
            Some("deploy")
        );
        assert_eq!(
            component.hooks["post:deploy"],
            vec!["wp cache flush".to_string()]
        );
        assert!(component
            .scopes
            .as_ref()
            .and_then(|scopes| scopes.deploy.as_ref())
            .is_some());
        assert_eq!(component.artifact_inputs[0].component, "builder");
        assert_eq!(component.cli_path.as_deref(), Some("lando wp"));
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
    fn component_override_config_sets_cli_path() {
        let mut component = base_component("my-plugin");
        assert_eq!(component.cli_path, None);

        let overrides = ProjectComponentOverrides {
            cli_path: Some("lando wp".to_string()),
            ..Default::default()
        };

        overrides.apply_to_component(&mut component);
        assert_eq!(component.cli_path, Some("lando wp".to_string()));
    }

    #[test]
    fn cli_path_override_applied_via_project() {
        let component = base_component("my-plugin");

        let mut overrides = HashMap::new();
        overrides.insert(
            "my-plugin".to_string(),
            ProjectComponentOverrides {
                cli_path: Some("lando wp".to_string()),
                ..Default::default()
            },
        );
        let project = project_with_overrides("my-site", overrides);

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.cli_path, Some("lando wp".to_string()));
    }

    /// Project-scoped `cli_path` fills in when no explicit component override sets it.
    /// This is the headline of #1165 — one config line on the project, not per component.
    #[test]
    fn project_cli_path_fills_in_for_unset_components() {
        let component = base_component("my-plugin");
        let project = Project {
            id: "my-site".to_string(),
            cli_path: Some("lando wp".to_string()),
            ..Default::default()
        };

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.cli_path, Some("lando wp".to_string()));
    }

    /// Component-level override is the most-specific escape hatch and wins
    /// over project-scoped `cli_path`.
    #[test]
    fn component_override_wins_over_project_cli_path() {
        let component = base_component("my-plugin");

        let mut overrides = HashMap::new();
        overrides.insert(
            "my-plugin".to_string(),
            ProjectComponentOverrides {
                cli_path: Some("lando wp".to_string()),
                ..Default::default()
            },
        );
        let project = Project {
            id: "my-site".to_string(),
            cli_path: Some("wp-env run cli wp".to_string()),
            component_overrides: overrides,
            ..Default::default()
        };

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.cli_path, Some("lando wp".to_string()));
    }

    /// Component's own (homeboy.json) `cli_path` is the highest-precedence
    /// escape hatch and should not be clobbered by project-scoped fallback.
    #[test]
    fn component_repo_cli_path_wins_over_project_cli_path() {
        let mut component = base_component("my-plugin");
        component.cli_path = Some("docker wp".to_string());

        let project = Project {
            id: "my-site".to_string(),
            cli_path: Some("lando wp".to_string()),
            ..Default::default()
        };

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.cli_path, Some("docker wp".to_string()));
    }

    /// When neither explicit overrides nor project-scoped `cli_path` are set,
    /// `cli_path` stays `None` and downstream resolution falls through to the
    /// extension default (or `"wp"`).
    #[test]
    fn unset_everywhere_stays_none() {
        let component = base_component("my-plugin");
        let project = Project {
            id: "my-site".to_string(),
            ..Default::default()
        };

        let result = apply_component_overrides(&component, &project);
        assert_eq!(result.cli_path, None);
    }
}
