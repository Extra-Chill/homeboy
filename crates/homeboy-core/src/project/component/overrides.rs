use crate::project::{Project, ProjectComponentOverrides};

/// Apply project-level component overrides.
///
/// Resolution order: component (repo portable config) → project overrides.
///
/// `cli_path` has an extra fallback step: if no explicit override at any layer
/// sets it, the project-scoped `Project::cli_path` fills it in via
/// [`crate::project::project_cli_path`]. This makes "every component on
/// this site uses the same wrapper CLI" a one-line project config instead of a
/// per-component repeat. Component-level `cli_path` still wins as the
/// most-specific escape hatch.
pub fn apply_component_overrides(
    component: &crate::component::Component,
    project: &Project,
) -> crate::component::Component {
    resolve_deploy_override_inputs(component, project).1
}

/// Resolve project override layers without allowing target binding fields to
/// contaminate source/build preparation identity.
pub fn resolve_deploy_override_inputs(
    component: &crate::component::Component,
    project: &Project,
) -> (crate::component::Component, crate::component::Component) {
    let mut preparation = component.clone();
    let mut binding = component.clone();
    if let Some(overrides) = project.component_overrides.get(&component.id) {
        apply_preparation_overrides(overrides, &mut preparation);
        overrides.apply_to_component(&mut binding);
    }

    if binding.cli_path.is_none() {
        if let Some(cli_path) = crate::project::project_cli_path(project) {
            binding.cli_path = Some(cli_path);
        }
    }

    (preparation, binding)
}

fn apply_preparation_overrides(
    overrides: &ProjectComponentOverrides,
    component: &mut crate::component::Component,
) {
    if let Some(build_artifact) = &overrides.build_artifact {
        component.build_artifact = Some(build_artifact.clone());
    }
    if let Some(scopes) = &overrides.scopes {
        component.scopes = Some(scopes.clone());
    }
    if !overrides.artifact_inputs.is_empty() {
        component.artifact_inputs = overrides.artifact_inputs.clone();
    }
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
        component.hooks.insert(
            homeboy_extension_contract::HookEvent::PreVersionBump,
            vec!["echo old".to_string()],
        );

        let mut hooks = HashMap::new();
        hooks.insert(
            homeboy_extension_contract::HookEvent::PostDeploy,
            vec!["echo new".to_string()],
        );
        let overrides = ProjectComponentOverrides {
            hooks,
            ..Default::default()
        };

        overrides.apply_to_component(&mut component);
        // Hooks should be replaced entirely
        assert!(component
            .hooks
            .contains_key(&homeboy_extension_contract::HookEvent::PostDeploy));
        assert!(!component
            .hooks
            .contains_key(&homeboy_extension_contract::HookEvent::PreVersionBump));
    }

    #[test]
    fn project_overrides_apply_only_to_the_matching_component_id() {
        let overrides = |id: &str| {
            HashMap::from([(
                id.to_string(),
                ProjectComponentOverrides {
                    remote_path: Some("wp-content/plugins/my-plugin".to_string()),
                    remote_owner: Some("www-data:www-data".to_string()),
                    ..Default::default()
                },
            )])
        };
        let component = base_component("my-plugin");

        let matched = apply_component_overrides(
            &component,
            &project_with_overrides("my-project", overrides("my-plugin")),
        );
        assert_eq!(matched.remote_path, "wp-content/plugins/my-plugin");
        assert_eq!(matched.remote_owner.as_deref(), Some("www-data:www-data"));

        for project in [
            project_with_overrides("my-project", HashMap::new()),
            project_with_overrides("my-project", overrides("other-plugin")),
        ] {
            let result = apply_component_overrides(&component, &project);
            assert_eq!(result.remote_path, "original/path");
            assert_eq!(result.remote_owner, None);
        }
    }

    /// `cli_path` precedence (#1165): the component's own config wins, then
    /// the project component override, then the project-scoped `cli_path`;
    /// unset everywhere stays `None` so the extension default applies.
    #[test]
    fn cli_path_resolves_component_then_override_then_project() {
        for (component_cli, override_cli, project_cli, expected) in [
            (None, None, None, None),
            (None, None, Some("lando wp"), Some("lando wp")),
            (None, Some("lando wp"), None, Some("lando wp")),
            (
                None,
                Some("lando wp"),
                Some("wp-env run cli wp"),
                Some("lando wp"),
            ),
            (Some("docker wp"), None, Some("lando wp"), Some("docker wp")),
        ] {
            let mut component = base_component("my-plugin");
            component.cli_path = component_cli.map(str::to_string);
            let mut component_overrides = HashMap::new();
            if let Some(cli_path) = override_cli {
                component_overrides.insert(
                    "my-plugin".to_string(),
                    ProjectComponentOverrides {
                        cli_path: Some(cli_path.to_string()),
                        ..Default::default()
                    },
                );
            }
            let project = Project {
                id: "my-site".to_string(),
                cli_path: project_cli.map(str::to_string),
                component_overrides,
                ..Default::default()
            };

            assert_eq!(
                apply_component_overrides(&component, &project)
                    .cli_path
                    .as_deref(),
                expected,
                "component={component_cli:?} override={override_cli:?} project={project_cli:?}"
            );
        }
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
            component.hooks[&homeboy_extension_contract::HookEvent::PostDeploy],
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
    fn deploy_override_inputs_keep_build_and_binding_fields_separate() {
        let component = base_component("my-plugin");
        let mut overrides = HashMap::new();
        overrides.insert(
            "my-plugin".to_string(),
            ProjectComponentOverrides {
                build_artifact: Some("build/project.zip".to_string()),
                remote_path: Some("wp-content/plugins/project".to_string()),
                extract_command: Some("unzip {{artifact}}".to_string()),
                ..Default::default()
            },
        );
        let project = project_with_overrides("site", overrides);

        let (preparation, binding) = resolve_deploy_override_inputs(&component, &project);

        assert_eq!(
            preparation.build_artifact.as_deref(),
            Some("build/project.zip")
        );
        assert_eq!(preparation.remote_path, "original/path");
        assert_eq!(binding.build_artifact.as_deref(), Some("build/project.zip"));
        assert_eq!(binding.remote_path, "wp-content/plugins/project");
        assert_eq!(
            binding.extract_command.as_deref(),
            Some("unzip {{artifact}}")
        );
    }
}
