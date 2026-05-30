use crate::core::component::Component;
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::extension::{self, RemotePathRootRule};
use crate::core::paths as base_path;
use crate::core::project::Project;
use crate::core::server::SshClient;
use std::collections::HashSet;

pub(super) fn component_remote_path(component: &Component) -> String {
    if component.remote_path.trim().is_empty() {
        component
            .auto_resolve_remote_path()
            .unwrap_or_else(|| component.remote_path.clone())
    } else {
        component.remote_path.clone()
    }
}

pub(super) fn resolve_effective_remote_path(
    project: &Project,
    component: &Component,
    fallback_base_path: &str,
) -> Result<String> {
    let remote_path = component_remote_path(component);

    if remote_path.trim_start().starts_with('/') {
        return base_path::join_remote_path(Some(fallback_base_path), &remote_path);
    }

    if let Some(resolved) = resolve_with_project_root(project, component, &remote_path)? {
        return Ok(resolved);
    }

    base_path::join_remote_path(Some(fallback_base_path), &remote_path)
}

pub(super) fn project_with_detected_path_roots(
    project: &Project,
    components: &[Component],
    base_path: &str,
    client: &SshClient,
) -> Project {
    let mut resolved = project.clone();
    let mut checked = HashSet::new();

    for rule in components.iter().flat_map(component_remote_path_root_rules) {
        if resolved.path_roots.contains_key(&rule.root) || !checked.insert(rule.root.clone()) {
            continue;
        }

        let Some(command) = rule.detect_command.as_deref() else {
            continue;
        };

        let command = command.replace("{{basePath}}", base_path);
        let output = client.execute(&format!(
            "cd {} && {}",
            shell::quote_path(base_path),
            command
        ));

        let root = output.stdout.trim().trim_end_matches('/');
        if output.success && !root.is_empty() {
            log_status!(
                "deploy",
                "Detected project path root {}={}",
                rule.root,
                root
            );
            resolved
                .path_roots
                .insert(rule.root.clone(), root.to_string());
        }
    }

    resolved
}

fn resolve_with_project_root(
    project: &Project,
    component: &Component,
    remote_path: &str,
) -> Result<Option<String>> {
    for rule in component_remote_path_root_rules(component) {
        if !path_matches_prefix(remote_path, &rule.path_prefix) {
            continue;
        }

        let Some(root) = project.path_roots.get(&rule.root) else {
            return Err(Error::validation_invalid_argument(
                "remotePath",
                format!(
                    "Component '{}' remote path '{}' matches managed path root '{}' ({}) but that root was not configured or detected",
                    component.id, remote_path, rule.root, rule.path_prefix
                ),
                Some(remote_path.to_string()),
                Some(vec![
                    format!(
                        "Configure project path_roots.{} to the active remote root for {}",
                        rule.root, rule.path_prefix
                    ),
                    format!(
                        "Use an explicit absolute/relative remote path if '{}' should not be root-managed",
                        remote_path
                    ),
                ]),
            ));
        };

        let path = if rule.strip_prefix {
            strip_path_prefix(remote_path, &rule.path_prefix)
        } else {
            remote_path
        };

        if path.is_empty() {
            return base_path::join_remote_path(None, root).map(Some);
        }

        return base_path::join_remote_path(Some(root), path).map(Some);
    }

    Ok(None)
}

fn component_remote_path_root_rules(component: &Component) -> Vec<RemotePathRootRule> {
    let Some(extensions) = &component.extensions else {
        return Vec::new();
    };

    extensions
        .keys()
        .filter_map(|id| extension::load_extension(id).ok())
        .filter_map(|manifest| manifest.deploy)
        .flat_map(|deploy| deploy.path_roots)
        .collect()
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let path = path.trim_matches('/');
    let prefix = prefix.trim_matches('/');

    !prefix.is_empty() && (path == prefix || path.starts_with(&format!("{}/", prefix)))
}

fn strip_path_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    let path = path.trim_start_matches('/');
    let prefix = prefix.trim_matches('/');

    path.strip_prefix(prefix)
        .map(|remaining| remaining.trim_start_matches('/'))
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::{Component, ScopedExtensionConfig};
    use crate::core::extension::{DeployCapability, ExtensionManifest};
    use crate::core::server::SshClient;
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;

    fn component(remote_path: &str) -> Component {
        let mut component = Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            remote_path.to_string(),
            None,
        );
        component.extensions = Some(HashMap::from([(
            "wordpress".to_string(),
            ScopedExtensionConfig::default(),
        )]));
        component
    }

    fn project_with_root() -> Project {
        Project {
            id: "site".to_string(),
            base_path: Some("/srv/site".to_string()),
            path_roots: HashMap::from([(
                "wp_content".to_string(),
                "/htdocs/wp-content".to_string(),
            )]),
            ..Project::default()
        }
    }

    fn install_extension() {
        install_extension_with_detect_command(None);
    }

    fn install_extension_with_detect_command(detect_command: Option<&str>) {
        crate::core::extension::save_manifest(&ExtensionManifest {
            id: "wordpress".to_string(),
            name: "WordPress".to_string(),
            version: "1.0.0".to_string(),
            deploy: Some(DeployCapability {
                verifications: Vec::new(),
                overrides: Vec::new(),
                archive_install: Vec::new(),
                remote_path_inference: Vec::new(),
                path_roots: vec![RemotePathRootRule {
                    path_prefix: "wp-content".to_string(),
                    root: "wp_content".to_string(),
                    strip_prefix: true,
                    detect_command: detect_command.map(str::to_string),
                }],
                version_patterns: Vec::new(),
                since_tag: None,
            }),
            ..serde_json::from_value(serde_json::json!({
                "name": "WordPress",
                "version": "1.0.0"
            }))
            .expect("manifest")
        })
        .expect("save extension");
    }

    fn local_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "local".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        }
    }

    #[test]
    fn test_component_remote_path() {
        assert_eq!(
            component_remote_path(&component("explicit/path")),
            "explicit/path"
        );

        with_isolated_home(|_| {
            install_extension();
            let mut auto = component("");
            auto.local_path = std::env::temp_dir().to_string_lossy().to_string();

            assert_eq!(component_remote_path(&auto), "");
        });
    }

    #[test]
    fn test_resolve_effective_remote_path() {
        with_isolated_home(|_| {
            install_extension();

            let resolved = resolve_effective_remote_path(
                &project_with_root(),
                &component("wp-content/plugins/foo"),
                "/srv/site",
            )
            .expect("resolve path");

            assert_eq!(resolved, "/htdocs/wp-content/plugins/foo");
        });
    }

    #[test]
    fn parent_relative_content_paths_fall_back_to_base_path() {
        with_isolated_home(|_| {
            install_extension();

            let resolved = resolve_effective_remote_path(
                &project_with_root(),
                &component("../wp-content/plugins/foo"),
                "/srv/site",
            )
            .expect("resolve path");

            assert_eq!(resolved, "/srv/site/../wp-content/plugins/foo");
        });
    }

    #[test]
    fn preserves_parent_relative_paths_before_base_path_fallback() {
        with_isolated_home(|_| {
            install_extension();

            let resolved = resolve_effective_remote_path(
                &project_with_root(),
                &component("../var/log/app.log"),
                "/srv/site",
            )
            .expect("resolve path");

            assert_eq!(resolved, "/srv/site/../var/log/app.log");
        });
    }

    #[test]
    fn test_project_with_detected_path_roots() {
        with_isolated_home(|_| {
            install_extension_with_detect_command(Some("printf /detected/wp-content"));
            let project = Project {
                id: "site".to_string(),
                ..Project::default()
            };

            let detected = project_with_detected_path_roots(
                &project,
                &[component("wp-content/plugins/foo")],
                "/tmp",
                &local_client(),
            );

            assert_eq!(
                detected.path_roots.get("wp_content").map(String::as_str),
                Some("/detected/wp-content")
            );
        });
    }

    #[test]
    fn applies_content_root_to_theme_paths() {
        with_isolated_home(|_| {
            install_extension();

            let resolved = resolve_effective_remote_path(
                &project_with_root(),
                &component("wp-content/themes/theme"),
                "/srv/site",
            )
            .expect("resolve path");

            assert_eq!(resolved, "/htdocs/wp-content/themes/theme");
        });
    }

    #[test]
    fn falls_back_to_base_path_when_rule_does_not_match() {
        with_isolated_home(|_| {
            install_extension();

            let resolved = resolve_effective_remote_path(
                &project_with_root(),
                &component("var/log/app.log"),
                "/srv/site",
            )
            .expect("resolve path");

            assert_eq!(resolved, "/srv/site/var/log/app.log");
        });
    }

    #[test]
    fn matching_path_root_without_detected_root_fails_instead_of_falling_back() {
        with_isolated_home(|_| {
            install_extension();

            let err = resolve_effective_remote_path(
                &Project {
                    id: "site".to_string(),
                    base_path: Some("/srv/site".to_string()),
                    ..Project::default()
                },
                &component("wp-content/plugins/foo"),
                "/srv/site",
            )
            .expect_err("missing managed root should fail");

            let message = err.to_string();
            assert!(message.contains("matches managed path root"));
            assert!(message.contains("wp_content"));
        });
    }
}
