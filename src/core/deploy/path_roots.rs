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

    let parent_relative_managed_path = parent_relative_managed_path(component, &remote_path)?;

    if parent_relative_managed_path.is_none() {
        reject_relative_parent_traversal(component, &remote_path)?;
    }

    if let Some(resolved) =
        resolve_with_project_root(project, component, fallback_base_path, &remote_path)?
    {
        return Ok(resolved);
    }

    if let Some(managed_path) = parent_relative_managed_path.as_deref() {
        if let Some(resolved) =
            resolve_optional_project_root(project, component, fallback_base_path, managed_path)?
        {
            return Ok(resolved);
        }

        // A parent-relative managed path (e.g. `../<prefix>/...`) can only
        // resolve safely through a configured/detected path_root. Without one,
        // joining it against base_path produces a literal `..` that escapes the
        // writable managed path root and fails mid-install with a cryptic
        // read-only filesystem error. Reject early with a clear diagnostic. (#3488)
        return Err(reject_unresolved_parent_relative_path(
            component,
            managed_path,
            &remote_path,
        ));
    }

    base_path::join_remote_path(Some(fallback_base_path), &remote_path)
}

/// Reject a parent-relative managed path (`../<prefix>/...`) whose path_root
/// was neither configured nor detected. Joining such a path against base_path
/// yields a literal `..` escape that lands outside the writable managed path
/// root, so we fail fast with an actionable diagnostic instead of letting the
/// install fail later with a cryptic read-only filesystem error. (#3488)
///
/// The matched rule (and therefore its root name) comes from the component's
/// extension manifest, so this stays agnostic of any specific content system.
fn reject_unresolved_parent_relative_path(
    component: &Component,
    managed_path: &str,
    remote_path: &str,
) -> Error {
    // The parent-relative suffix was only accepted because it matched a managed
    // path-root rule, so a matching rule is expected here. Fall back to a generic
    // label (rather than a content-system-specific one) only as a defensive guard.
    let matching_rule = component_remote_path_root_rules(component)
        .into_iter()
        .find(|rule| path_matches_prefix(managed_path, &rule.path_prefix));

    let (root_name, example_child): (String, String) = match matching_rule {
        Some(rule) => (
            rule.root.clone(),
            strip_path_prefix(managed_path, &rule.path_prefix)
                .trim_start_matches('/')
                .to_string(),
        ),
        None => (
            "managed path root".to_string(),
            managed_path.trim_start_matches('/').to_string(),
        ),
    };

    Error::validation_invalid_argument(
        "remotePath",
        format!(
            "Component '{}' remote_path '{}' resolves outside the writable managed path root: the parent-relative '..' escape requires path_root '{}' which was not configured or detected for this runtime",
            component.id, remote_path, root_name
        ),
        Some(remote_path.to_string()),
        Some(vec![
            format!(
                "Set remote_path to an explicit absolute path inside the writable managed path root (absolute paths are used verbatim, e.g. '/<runtime-root>/{}')",
                example_child
            ),
            format!(
                "Configure project path_roots.{} to the active remote managed path root, or ensure the extension can detect it at deploy time",
                root_name
            ),
        ]),
    )
}

fn parent_relative_managed_path(
    component: &Component,
    remote_path: &str,
) -> Result<Option<String>> {
    let trimmed = remote_path.trim();
    let mut saw_parent = false;
    let mut suffix_segments = Vec::new();

    for segment in trimmed.split('/') {
        let segment = segment.trim();
        if segment.is_empty() || segment == "." {
            continue;
        }

        if segment == ".." {
            if suffix_segments.is_empty() {
                saw_parent = true;
                continue;
            }

            reject_relative_parent_traversal(component, remote_path)?;
        }

        suffix_segments.push(segment);
    }

    if !saw_parent {
        return Ok(None);
    }

    let suffix = suffix_segments.join("/");
    if suffix.is_empty()
        || !component_remote_path_root_rules(component)
            .iter()
            .any(|rule| path_matches_prefix(&suffix, &rule.path_prefix))
    {
        reject_relative_parent_traversal(component, remote_path)?;
    }

    Ok(Some(suffix))
}

fn reject_relative_parent_traversal(component: &Component, remote_path: &str) -> Result<()> {
    if !remote_path.split('/').any(|segment| segment.trim() == "..") {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "remotePath",
        format!(
            "Component '{}' remote path '{}' escapes the project base_path with '..' segments",
            component.id, remote_path
        ),
        Some(remote_path.to_string()),
        Some(vec![
            "Configure an explicit absolute remote_path when deploying outside base_path".to_string(),
            "Configure a project path_root and use the managed path prefix when the extension supports it".to_string(),
        ]),
    ))
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
    fallback_base_path: &str,
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

        let resolved_root = base_path::join_remote_path(Some(fallback_base_path), root)?;

        if path.is_empty() {
            return Ok(Some(resolved_root));
        }

        return base_path::join_remote_path(Some(&resolved_root), path).map(Some);
    }

    Ok(None)
}

fn resolve_optional_project_root(
    project: &Project,
    component: &Component,
    fallback_base_path: &str,
    remote_path: &str,
) -> Result<Option<String>> {
    for rule in component_remote_path_root_rules(component) {
        if !path_matches_prefix(remote_path, &rule.path_prefix) {
            continue;
        }

        let Some(root) = project.path_roots.get(&rule.root) else {
            return Ok(None);
        };

        let path = if rule.strip_prefix {
            strip_path_prefix(remote_path, &rule.path_prefix)
        } else {
            remote_path
        };

        let resolved_root = base_path::join_remote_path(Some(fallback_base_path), root)?;

        if path.is_empty() {
            return Ok(Some(resolved_root));
        }

        return base_path::join_remote_path(Some(&resolved_root), path).map(Some);
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
                protected_path_suffixes: Vec::new(),
                owner_hints: Vec::new(),
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

    // A non-WordPress extension + component, used to prove the path-root
    // machinery is content-system-agnostic: the root name and prefix come from
    // the extension manifest, never hardcoded in core.
    fn install_generic_extension() {
        crate::core::extension::save_manifest(&ExtensionManifest {
            id: "static-site".to_string(),
            name: "Static Site".to_string(),
            version: "1.0.0".to_string(),
            deploy: Some(DeployCapability {
                verifications: Vec::new(),
                overrides: Vec::new(),
                protected_path_suffixes: Vec::new(),
                owner_hints: Vec::new(),
                archive_install: Vec::new(),
                remote_path_inference: Vec::new(),
                path_roots: vec![RemotePathRootRule {
                    path_prefix: "public".to_string(),
                    root: "public_root".to_string(),
                    strip_prefix: true,
                    detect_command: None,
                }],
                version_patterns: Vec::new(),
                since_tag: None,
            }),
            ..serde_json::from_value(serde_json::json!({
                "name": "Static Site",
                "version": "1.0.0"
            }))
            .expect("manifest")
        })
        .expect("save generic extension");
    }

    fn generic_component(remote_path: &str) -> Component {
        let mut component = Component::new(
            "gen-fixture".to_string(),
            "/tmp/gen-fixture".to_string(),
            remote_path.to_string(),
            None,
        );
        component.extensions = Some(HashMap::from([(
            "static-site".to_string(),
            ScopedExtensionConfig::default(),
        )]));
        component
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
    fn resolves_relative_content_root_against_base_path() {
        with_isolated_home(|_| {
            install_extension();
            let project = Project {
                id: "site".to_string(),
                base_path: Some("/srv/site".to_string()),
                path_roots: HashMap::from([("wp_content".to_string(), "wp-content".to_string())]),
                ..Project::default()
            };

            let resolved = resolve_effective_remote_path(
                &project,
                &component("wp-content/plugins/foo"),
                "/srv/site",
            )
            .expect("resolve path");

            assert_eq!(resolved, "/srv/site/wp-content/plugins/foo");
        });
    }

    #[test]
    fn parent_relative_content_paths_without_root_are_rejected() {
        with_isolated_home(|_| {
            install_extension();

            let err = resolve_effective_remote_path(
                &Project {
                    id: "site".to_string(),
                    base_path: Some("/srv/site".to_string()),
                    ..Project::default()
                },
                &component("../wp-content/plugins/foo"),
                "/srv/site",
            )
            .expect_err("parent-relative content path without a root should be rejected");

            let message = err.to_string();
            assert!(
                message.contains("resolves outside the writable managed path root"),
                "expected clear root-escape diagnostic, got: {message}"
            );
            // `wp_content` is the root NAME registered by the test's wordpress
            // extension manifest — it must flow through from config, not be
            // hardcoded in core. Proves the diagnostic is data-driven.
            assert!(message.contains("wp_content"));

            // Remediation lives in details.tried (same shape as the other
            // path-root errors in this module).
            let tried = err.details["tried"].as_array().expect("tried hints");
            let tried_text: String = tried
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                tried_text.contains("runtime-root>/plugins/foo"),
                "diagnostic should suggest an explicit absolute path shape, got: {tried_text}"
            );
            assert!(
                tried_text.contains("path_roots.wp_content"),
                "diagnostic should mention configuring the path_root, got: {tried_text}"
            );
        });
    }

    #[test]
    fn wp_cloud_parent_relative_path_without_root_is_rejected_3488() {
        // Regression for issue #3488: WP Cloud base_path `/htdocs/__wp__` with a
        // `../wp-content/...` remote_path and no detected wp_content root must be
        // rejected at preflight instead of producing `/htdocs/__wp__/../wp-content/...`
        // (which `mkdir -p` expands into a read-only filesystem).
        with_isolated_home(|_| {
            install_extension();

            let err = resolve_effective_remote_path(
                &Project {
                    id: "wp-docs-runtime".to_string(),
                    base_path: Some("/htdocs/__wp__".to_string()),
                    ..Project::default()
                },
                &component("../wp-content/plugins/frontend-agent-chat"),
                "/htdocs/__wp__",
            )
            .expect_err("WP Cloud parent-relative escape must be rejected (#3488)");

            let message = err.to_string();
            assert!(message.contains("resolves outside the writable managed path root"));
            assert!(message.contains("frontend-agent-chat"));
            assert!(
                !message.contains("/htdocs/__wp__/../wp-content"),
                "must not surface the escaping expanded path, got: {message}"
            );
        });
    }

    #[test]
    fn parent_relative_path_rejection_is_content_system_agnostic() {
        // The path-root mechanism must not assume WordPress. A non-WP extension
        // ("static-site") registers its own root name ("public_root") and prefix
        // ("public"); the rejection diagnostic must surface THAT name, proving
        // the root identity is extension-driven, not hardcoded in core. (#3488)
        with_isolated_home(|_| {
            install_generic_extension();

            let err = resolve_effective_remote_path(
                &Project {
                    id: "static".to_string(),
                    base_path: Some("/srv/static".to_string()),
                    ..Project::default()
                },
                &generic_component("../public/assets/bundle.js"),
                "/srv/static",
            )
            .expect_err("generic parent-relative escape should be rejected");

            let message = err.to_string();
            assert!(
                message.contains("resolves outside the writable managed path root"),
                "generic diagnostic, got: {message}"
            );
            // The root NAME comes from the static-site extension manifest, not WP.
            assert!(
                message.contains("public_root"),
                "diagnostic must use the extension-defined root name, got: {message}"
            );
            assert!(
                !message.contains("wp_content"),
                "must not leak a WordPress-specific root name for a non-WP extension, got: {message}"
            );

            // And it must resolve correctly when the root IS configured.
            let resolved = resolve_effective_remote_path(
                &Project {
                    id: "static".to_string(),
                    base_path: Some("/srv/static".to_string()),
                    path_roots: HashMap::from([(
                        "public_root".to_string(),
                        "/srv/static/public".to_string(),
                    )]),
                    ..Project::default()
                },
                &generic_component("../public/assets/bundle.js"),
                "/srv/static/current",
            )
            .expect("generic parent-relative path should resolve through root");

            assert_eq!(resolved, "/srv/static/public/assets/bundle.js");
        });
    }

    #[test]
    fn parent_relative_content_paths_use_configured_root_when_available() {
        with_isolated_home(|_| {
            install_extension();

            let resolved = resolve_effective_remote_path(
                &project_with_root(),
                &component("../wp-content/plugins/foo"),
                "/htdocs/__wp__",
            )
            .expect("parent-relative content path should resolve through root");

            assert_eq!(resolved, "/htdocs/wp-content/plugins/foo");
        });
    }

    #[test]
    fn parent_relative_non_content_paths_are_rejected() {
        with_isolated_home(|_| {
            install_extension();

            let err = resolve_effective_remote_path(
                &project_with_root(),
                &component("../var/log/app.log"),
                "/srv/site",
            )
            .expect_err("parent traversal should fail");

            assert!(err.to_string().contains("escapes the project base_path"));
        });
    }

    #[test]
    fn parent_segments_after_managed_prefix_are_rejected() {
        with_isolated_home(|_| {
            install_extension();

            let err = resolve_effective_remote_path(
                &project_with_root(),
                &component("../wp-content/../secrets/foo"),
                "/srv/site",
            )
            .expect_err("internal parent traversal should fail");

            assert!(err.to_string().contains("escapes the project base_path"));
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
