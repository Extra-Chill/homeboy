//! Shared component remote-path resolution used by deploy and inverse operations.

use crate::component::{self, Component};
use crate::engine::shell;
use crate::error::{Error, Result};
use crate::extension::catalog;
use crate::paths as base_path;
use crate::project::Project;
use crate::server::SshClient;
use homeboy_extension_contract::RemotePathRootRule;
use std::collections::HashSet;

/// Effective deploy remote path for a component.
///
/// Fallible since #11119: this is the deploy-facing resolver, so an unresolved
/// multi-extension `remote_path_inference` conflict must be a hard error here
/// rather than the empty string that deploy would then treat as "not set".
pub fn component_remote_path(component: &Component) -> Result<String> {
    if component.remote_path.trim().is_empty() {
        Ok(component::try_auto_resolve_remote_path(component)?
            .unwrap_or_else(|| component.remote_path.clone()))
    } else {
        Ok(component.remote_path.clone())
    }
}

pub fn resolve_effective_remote_path(
    project: &Project,
    component: &Component,
    fallback_base_path: &str,
) -> Result<String> {
    let remote_path = component_remote_path(component)?;

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

        // A parent-relative managed path (e.g. `../<managed-prefix>/...`) can
        // only resolve safely through a configured/detected path_root. Without
        // one, joining it against base_path produces a literal `..` that escapes
        // the writable managed root and fails mid-install with a cryptic
        // read-only filesystem error. Reject early with a clear diagnostic. (#3488)
        return Err(reject_unresolved_parent_relative_path(
            component,
            managed_path,
            &remote_path,
        ));
    }

    base_path::join_remote_path(Some(fallback_base_path), &remote_path)
}

/// Reject a parent-relative managed path (`../<managed-prefix>/...`) whose
/// path_root was neither configured nor detected. Joining such a path against
/// base_path yields a literal `..` escape that lands outside the writable
/// managed root, so we fail fast with an actionable diagnostic instead of
/// letting the install fail later with a cryptic read-only filesystem error.
/// (#3488)
fn reject_unresolved_parent_relative_path(
    component: &Component,
    managed_path: &str,
    remote_path: &str,
) -> Error {
    let matching_rule = component_remote_path_root_rules(component)
        .into_iter()
        .find(|rule| path_matches_prefix(managed_path, &rule.path_prefix));
    let managed_root_name = matching_rule
        .as_ref()
        .map(|rule| rule.root.clone())
        .unwrap_or_else(|| "managed_root".to_string());

    // The runtime managed root is unknown — that's the failure — so show the
    // *shape* of a correct absolute remote_path by stripping the extension's
    // managed prefix off the path so it isn't doubled under the root.
    let example_child = matching_rule
        .as_ref()
        .map(|rule| strip_path_prefix(managed_path, &rule.path_prefix).to_string())
        .unwrap_or_else(|| managed_path.trim_start_matches('/').to_string());

    Error::validation_invalid_argument(
        "remotePath",
        format!(
            "Component '{}' remote_path '{}' resolves outside the writable managed root: the parent-relative '..' escape requires path_root '{}' which was not configured or detected for this runtime",
            component.id, remote_path, managed_root_name
        ),
        Some(remote_path.to_string()),
        Some(vec![
            format!(
                "Set remote_path to an explicit absolute path inside the writable managed root (absolute paths are used verbatim, e.g. '/<runtime-managed-root>/{}')",
                example_child.trim_start_matches('/')
            ),
            format!(
                "Configure project path_roots.{} to the active remote managed root, or ensure the extension can detect it at deploy time",
                managed_root_name
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

pub fn project_with_detected_path_roots(
    project: &Project,
    components: &[Component],
    base_path: &str,
    client: &SshClient,
    operation: &str,
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
            crate::log_status!(
                operation,
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
        .filter_map(|id| catalog::load_extension(id).ok())
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
    use crate::component::{Component, ScopedExtensionConfig};
    use crate::server::SshClient;
    use crate::test_support::with_isolated_home;
    use homeboy_extension_contract::manifest_capabilities::DeployCapability;
    use homeboy_extension_contract::ExtensionManifest;
    use std::collections::HashMap;

    fn component(remote_path: &str) -> Component {
        let mut component = Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            remote_path.to_string(),
            None,
        );
        component.extensions = Some(HashMap::from([(
            "package-host".to_string(),
            ScopedExtensionConfig::default(),
        )]));
        component
    }

    fn install_extension() {
        install_extension_with_detect_command(None);
    }

    fn install_extension_with_detect_command(detect_command: Option<&str>) {
        catalog::save_manifest(&ExtensionManifest {
            id: "package-host".to_string(),
            name: "Package Host".to_string(),
            version: "1.0.0".to_string(),
            deploy: Some(DeployCapability {
                verifications: Vec::new(),
                overrides: Vec::new(),
                protected_path_suffixes: Vec::new(),
                owner_hints: Vec::new(),
                archive_install: Vec::new(),
                remote_path_inference: Vec::new(),
                dependency_inference: Vec::new(),
                path_roots: vec![RemotePathRootRule {
                    path_prefix: "packages".to_string(),
                    root: "package_root".to_string(),
                    strip_prefix: true,
                    detect_command: detect_command.map(str::to_string),
                }],
                version_patterns: Vec::new(),
                since_tag: None,
            }),
            ..serde_json::from_value(serde_json::json!({
                "name": "Package Host",
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

    fn project(path_roots: &[(&str, &str)]) -> Project {
        Project {
            id: "site".to_string(),
            base_path: Some("/srv/site".to_string()),
            path_roots: path_roots
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ..Project::default()
        }
    }

    #[test]
    fn resolves_remote_paths_through_managed_roots_or_base_path() {
        with_isolated_home(|_| {
            install_extension();
            let absolute_root = [("package_root", "/opt/packages")];
            let relative_root = [("package_root", "packages")];

            for (roots, remote_path, base, expected) in [
                (
                    &absolute_root[..],
                    "packages/foo",
                    "/srv/site",
                    "/opt/packages/foo",
                ),
                (
                    &absolute_root[..],
                    "packages/themes/theme",
                    "/srv/site",
                    "/opt/packages/themes/theme",
                ),
                (
                    &relative_root[..],
                    "packages/foo",
                    "/srv/site",
                    "/srv/site/packages/foo",
                ),
                (
                    &absolute_root[..],
                    "../packages/foo",
                    "/srv/runtime/app",
                    "/opt/packages/foo",
                ),
                (
                    &absolute_root[..],
                    "var/log/app.log",
                    "/srv/site",
                    "/srv/site/var/log/app.log",
                ),
            ] {
                let resolved =
                    resolve_effective_remote_path(&project(roots), &component(remote_path), base)
                        .unwrap_or_else(|err| panic!("{remote_path}: {err}"));
                assert_eq!(resolved, expected, "{remote_path}");
            }
        });
    }

    #[test]
    fn rejects_remote_paths_that_escape_or_lack_their_managed_root() {
        with_isolated_home(|_| {
            install_extension();
            let with_root = [("package_root", "/opt/packages")];

            for (roots, remote_path, expected) in [
                (
                    &with_root[..],
                    "../var/log/app.log",
                    "escapes the project base_path",
                ),
                (
                    &with_root[..],
                    "../packages/../secrets/foo",
                    "escapes the project base_path",
                ),
                (&[][..], "packages/foo", "matches managed path root"),
            ] {
                let err = resolve_effective_remote_path(
                    &project(roots),
                    &component(remote_path),
                    "/srv/site",
                )
                .expect_err(remote_path);
                let message = err.to_string();
                assert!(message.contains(expected), "{remote_path}: {message}");
                if roots.is_empty() {
                    assert!(message.contains("package_root"));
                }
            }
        });
    }

    #[test]
    fn parent_relative_managed_path_without_root_is_rejected_before_expansion_3488() {
        with_isolated_home(|_| {
            install_extension();

            let err = resolve_effective_remote_path(
                &Project {
                    id: "docs-runtime".to_string(),
                    base_path: Some("/srv/runtime/app".to_string()),
                    ..Project::default()
                },
                &component("../packages/frontend-agent-chat"),
                "/srv/runtime/app",
            )
            .expect_err("parent-relative escape must be rejected (#3488)");

            let message = err.to_string();
            assert!(message.contains("resolves outside the writable managed root"));
            assert!(message.contains("package_root"));
            assert!(
                !message.contains("/srv/runtime/app/../packages"),
                "must not surface the escaping expanded path, got: {message}"
            );

            let tried_text = err.details["tried"]
                .as_array()
                .expect("tried hints")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(tried_text.contains("runtime-managed-root>/frontend-agent-chat"));
            assert!(tried_text.contains("path_roots.package_root"));
        });
    }

    #[test]
    fn detects_missing_path_roots_with_the_extension_detect_command() {
        with_isolated_home(|_| {
            install_extension_with_detect_command(Some("printf /detected/packages"));
            let project = Project {
                id: "site".to_string(),
                ..Project::default()
            };

            let detected = project_with_detected_path_roots(
                &project,
                &[component("packages/foo")],
                "/tmp",
                &local_client(),
                "test",
            );

            assert_eq!(
                detected.path_roots.get("package_root").map(String::as_str),
                Some("/detected/packages")
            );
        });
    }
}
