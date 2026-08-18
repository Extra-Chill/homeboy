use crate::error::{Error, Result};
use crate::project::Project;
use std::collections::HashMap;
use std::path::Path;

use super::overrides::apply_component_overrides;
use crate::component::discover_from_portable;

pub fn resolve_project_component(
    project: &Project,
    component_id: &str,
) -> Result<crate::component::Component> {
    resolve_project_component_with_standalone_snapshot(project, component_id, None)
}

/// [`resolve_project_component`] against an already-resolved config root (#7505).
pub fn resolve_project_component_in_root(
    config_root: &Path,
    project: &Project,
    component_id: &str,
) -> Result<crate::component::Component> {
    resolve_project_component_with_standalone_snapshot_in_root(
        config_root,
        project,
        component_id,
        None,
    )
}

pub fn resolve_project_component_with_standalone_snapshot(
    project: &Project,
    component_id: &str,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) -> Result<crate::component::Component> {
    resolve_project_component_core(None, project, component_id, standalone_snapshot)
}

/// [`resolve_project_component_with_standalone_snapshot`] against an
/// already-resolved config root (#7505).
///
/// A supplied `standalone_snapshot` MUST have been produced by
/// [`StandaloneComponentConfigSnapshot::load_in_root`] with the same
/// `config_root`. Passing a snapshot read from a different home is the exact
/// half-injected split this rooting exists to remove: the fallbacks would come
/// from one home and everything else from another.
pub fn resolve_project_component_with_standalone_snapshot_in_root(
    config_root: &Path,
    project: &Project,
    component_id: &str,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) -> Result<crate::component::Component> {
    resolve_project_component_core(
        Some(config_root),
        project,
        component_id,
        standalone_snapshot,
    )
}

/// The one project-component resolution, parameterized by the path boundary.
///
/// `config_root` is `None` for the ambient entry points and `Some(_)` for the
/// rooted ones, and it governs every config-root-derived read this resolution
/// makes (standalone fallbacks and extension-driven `remote_path` inference).
/// The remaining reads — attachment validation, portable discovery, duplicate-id
/// scanning, local_path normalization — are functions of the supplied project
/// and the filesystem paths it names, and have no config root to resolve.
fn resolve_project_component_core(
    config_root: Option<&Path>,
    project: &Project,
    component_id: &str,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) -> Result<crate::component::Component> {
    let (component, attachment_local_path, attachment_remote_path, attachment_deployment_provider) =
        if let Some(attachment) = project
            .components
            .iter()
            .find(|component| component.id == component_id)
        {
            super::super::validate_component_local_path(project, component_id)?;
            crate::component::resolution::validate_duplicate_portable_component_ids(
                component_id,
                Path::new(&attachment.local_path),
                None,
            )?;
            (
                discover_from_portable(Path::new(&attachment.local_path)).ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "components.local_path",
                        format!(
                            "Project component '{}' points to '{}' but no homeboy.json was found",
                            component_id, attachment.local_path
                        ),
                        Some(project.id.clone()),
                        None,
                    )
                })?,
                attachment.local_path.clone(),
                attachment.remote_path.clone(),
                attachment.deployment_provider.clone(),
            )
        } else {
            return Err(Error::validation_invalid_argument(
                "components",
                format!(
                    "Project '{}' has no attached component '{}'",
                    project.id, component_id
                ),
                Some(project.id.clone()),
                None,
            ));
        };

    let mut resolved = bind_materialized_component_to_project_core(
        config_root,
        component,
        project,
        standalone_snapshot,
        attachment_remote_path,
        attachment_deployment_provider,
    );
    // Normalize the attachment `local_path` to an absolute path before it becomes
    // the resolved component's machine-local checkout. Deploy resolves components
    // directly through this function, while `release` reaches the same attachment
    // via the composed inventory (project-attached components win on ID). Because
    // `release` rejects a relative `local_path` outright (see
    // `component::validate_local_path`) but deploy silently accepts one, a relative
    // attachment value made the two commands resolve *different* checkouts for the
    // same component id (#7410). Normalizing here — the single shared seam — makes
    // `release`, `deploy`, and `component set` agree on one absolute path, matching
    // the write-side normalization `component set` already applies (#6938). A tilde
    // or absolute value is preserved as-is; a relative value is resolved against the
    // current working directory, exactly as portable discovery already interprets it.
    resolved.local_path = crate::component::normalize_component_local_path(&attachment_local_path)
        .unwrap_or(attachment_local_path);

    // Inherit project-level extensions when the component's homeboy.json doesn't
    // declare any. This handles clean tag clones from older releases where
    // extensions weren't yet in homeboy.json. (#932)
    if resolved.extensions.is_none() || resolved.extensions.as_ref().is_some_and(|e| e.is_empty()) {
        if let Some(project_extensions) = &project.extensions {
            if !project_extensions.is_empty() {
                resolved.extensions = Some(project_extensions.clone());
            }
        }
    }

    // Auto-resolve remote_path if still empty after all config layers.
    // Repo homeboy.json intentionally omits remote_path (it's deploy config),
    // so auto-detect it from source files when possible (#812).
    resolve_remote_path_core(config_root, &mut resolved);

    Ok(resolved)
}

/// `remote_path` auto-resolution at the boundary this resolution is running on.
fn resolve_remote_path_core(
    config_root: Option<&Path>,
    component: &mut crate::component::Component,
) {
    match config_root {
        Some(config_root) => crate::component::resolve_remote_path_in_root(config_root, component),
        None => crate::component::resolve_remote_path(component),
    }
}

/// Apply the project-owned layers to component configuration discovered from a
/// materialized source tree. Exact-ref and isolated tag checkouts use this so
/// their source-owned fields come from the selected commit while attachment and
/// fleet/project policy keeps its normal precedence.
pub fn bind_materialized_component_to_project(
    component: crate::component::Component,
    project: &Project,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
    attachment_remote_path: Option<String>,
    attachment_deployment_provider: Option<crate::component::DeploymentProviderAttachment>,
) -> crate::component::Component {
    bind_materialized_component_to_project_core(
        None,
        component,
        project,
        standalone_snapshot,
        attachment_remote_path,
        attachment_deployment_provider,
    )
}

/// [`bind_materialized_component_to_project`] against an already-resolved config
/// root (#7505). See
/// [`resolve_project_component_with_standalone_snapshot_in_root`] for the
/// snapshot-provenance requirement.
pub fn bind_materialized_component_to_project_in_root(
    config_root: &Path,
    component: crate::component::Component,
    project: &Project,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
    attachment_remote_path: Option<String>,
    attachment_deployment_provider: Option<crate::component::DeploymentProviderAttachment>,
) -> crate::component::Component {
    bind_materialized_component_to_project_core(
        Some(config_root),
        component,
        project,
        standalone_snapshot,
        attachment_remote_path,
        attachment_deployment_provider,
    )
}

fn bind_materialized_component_to_project_core(
    config_root: Option<&Path>,
    mut component: crate::component::Component,
    project: &Project,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
    attachment_remote_path: Option<String>,
    attachment_deployment_provider: Option<crate::component::DeploymentProviderAttachment>,
) -> crate::component::Component {
    if let Some(remote_path) = attachment_remote_path.filter(|path| !path.trim().is_empty()) {
        component.remote_path = remote_path;
    }
    if attachment_deployment_provider.is_some() {
        component.deployment_provider = attachment_deployment_provider;
    }
    apply_standalone_component_fallbacks_core(config_root, &mut component, standalone_snapshot);
    apply_component_overrides(&component, project)
}

/// Reapply project-owned configuration after loading source-owned configuration
/// from an alternate checkout of an attached component.
pub fn bind_materialized_component_at_path(
    component: crate::component::Component,
    project: &Project,
) -> Result<crate::component::Component> {
    bind_materialized_component_at_path_core(None, component, project)
}

/// [`bind_materialized_component_at_path`] against an already-resolved config
/// root (#7505).
pub fn bind_materialized_component_at_path_in_root(
    config_root: &Path,
    component: crate::component::Component,
    project: &Project,
) -> Result<crate::component::Component> {
    bind_materialized_component_at_path_core(Some(config_root), component, project)
}

fn bind_materialized_component_at_path_core(
    config_root: Option<&Path>,
    component: crate::component::Component,
    project: &Project,
) -> Result<crate::component::Component> {
    let attachment = project
        .components
        .iter()
        .find(|attachment| attachment.id == component.id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "components",
                format!(
                    "Project '{}' has no attached component '{}'",
                    project.id, component.id
                ),
                Some(project.id.clone()),
                None,
            )
        })?;
    let mut component = bind_materialized_component_to_project_core(
        config_root,
        component,
        project,
        None,
        attachment.remote_path.clone(),
        attachment.deployment_provider.clone(),
    );
    inherit_project_extensions(&mut component, project);
    resolve_remote_path_core(config_root, &mut component);
    Ok(component)
}

pub(crate) fn apply_standalone_component_fallbacks(
    component: &mut crate::component::Component,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) {
    apply_standalone_component_fallbacks_core(None, component, standalone_snapshot)
}

/// [`apply_standalone_component_fallbacks`] against an already-resolved config
/// root (#7505). See
/// [`resolve_project_component_with_standalone_snapshot_in_root`] for the
/// snapshot-provenance requirement.
pub(crate) fn apply_standalone_component_fallbacks_in_root(
    config_root: &Path,
    component: &mut crate::component::Component,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) {
    apply_standalone_component_fallbacks_core(Some(config_root), component, standalone_snapshot)
}

fn apply_standalone_component_fallbacks_core(
    config_root: Option<&Path>,
    component: &mut crate::component::Component,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) {
    let standalone = match standalone_snapshot {
        Some(snapshot) => snapshot.get(&component.id).cloned(),
        None => load_standalone_component_config_core(config_root, &component.id),
    };
    let Some(standalone) = standalone else {
        return;
    };

    if component.remote_path.trim().is_empty() && !standalone.remote_path.trim().is_empty() {
        component.remote_path = standalone.remote_path;
    }

    if component.extract_command.is_none() {
        component.extract_command = standalone.extract_command;
    }

    if component.remote_url.is_none() {
        component.remote_url = standalone.remote_url;
    }
}

fn inherit_project_extensions(component: &mut crate::component::Component, project: &Project) {
    if component.extensions.is_none() || component.extensions.as_ref().is_some_and(|e| e.is_empty())
    {
        if let Some(project_extensions) = &project.extensions {
            if !project_extensions.is_empty() {
                component.extensions = Some(project_extensions.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StandaloneComponentConfigSnapshot {
    components: HashMap<String, crate::component::Component>,
}

impl StandaloneComponentConfigSnapshot {
    pub fn load() -> Self {
        let Ok(dir) = crate::paths::components() else {
            return Self::default();
        };
        Self::load_from_dir(&dir)
    }

    /// [`StandaloneComponentConfigSnapshot::load`] against an already-resolved
    /// config root (#7505).
    ///
    /// A snapshot loaded here must only be paired with the `_in_root`
    /// resolvers using the same `config_root`.
    pub fn load_in_root(config_root: &Path) -> Self {
        Self::load_from_dir(&crate::paths::components_in_root(config_root))
    }

    fn load_from_dir(dir: &Path) -> Self {
        let mut snapshot = Self::default();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return snapshot;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let Some(component_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(component) = load_standalone_component_config_from_path(component_id, &path)
            else {
                continue;
            };

            snapshot
                .components
                .insert(component_id.to_string(), component);
        }

        snapshot
    }

    fn get(&self, component_id: &str) -> Option<&crate::component::Component> {
        self.components.get(component_id)
    }
}

fn load_standalone_component_config_core(
    config_root: Option<&Path>,
    component_id: &str,
) -> Option<crate::component::Component> {
    let dir = match config_root {
        Some(config_root) => crate::paths::components_in_root(config_root),
        None => crate::paths::components().ok()?,
    };
    let path = dir.join(format!("{component_id}.json"));
    load_standalone_component_config_from_path(component_id, &path)
}

fn load_standalone_component_config_from_path(
    component_id: &str,
    path: &Path,
) -> Option<crate::component::Component> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut json: serde_json::Value = serde_json::from_str(&content).ok()?;

    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "id".to_string(),
            serde_json::Value::String(component_id.to_string()),
        );
    }

    serde_json::from_value::<crate::component::Component>(json).ok()
}

pub fn resolve_project_components(project: &Project) -> Result<Vec<crate::component::Component>> {
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load();
    project
        .components
        .iter()
        .map(|component| {
            resolve_project_component_with_standalone_snapshot(
                project,
                &component.id,
                Some(&standalone_snapshot),
            )
        })
        .collect()
}

/// [`resolve_project_components`] against an already-resolved config root (#7505).
///
/// The snapshot is loaded from the same root it resolves against, so the
/// fallbacks and the resolution can never describe two different homes.
pub fn resolve_project_components_in_root(
    config_root: &Path,
    project: &Project,
) -> Result<Vec<crate::component::Component>> {
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load_in_root(config_root);
    project
        .components
        .iter()
        .map(|component| {
            resolve_project_component_with_standalone_snapshot_in_root(
                config_root,
                project,
                &component.id,
                Some(&standalone_snapshot),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use crate::project::{ProjectComponentAttachment, ProjectComponentOverrides};
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn repo_with_portable_remote_path(remote_path: &str) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        std::fs::write(
            dir.path().join("homeboy.json"),
            serde_json::json!({
                "id": "fixture",
                "remote_path": remote_path,
                "build_artifact": "dist/fixture.zip"
            })
            .to_string(),
        )
        .expect("write homeboy.json");
        dir
    }

    fn project_with_attachment(remote_path: Option<&str>, local_path: String) -> Project {
        Project {
            id: "site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "fixture".to_string(),
                local_path,
                remote_path: remote_path.map(str::to_string),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn attachment_remote_path_overrides_portable_remote_path() {
        let repo = repo_with_portable_remote_path("../wp-content/plugins/fixture");
        let project = project_with_attachment(
            Some("wp-content/plugins/fixture"),
            repo.path().to_string_lossy().to_string(),
        );

        let component = resolve_project_component(&project, "fixture").expect("component");

        assert_eq!(component.remote_path, "wp-content/plugins/fixture");
    }

    #[test]
    fn component_overrides_still_win_over_attachment_remote_path() {
        let repo = repo_with_portable_remote_path("portable/plugins/fixture");
        let mut project = project_with_attachment(
            Some("attachment/plugins/fixture"),
            repo.path().to_string_lossy().to_string(),
        );
        project.component_overrides.insert(
            "fixture".to_string(),
            ProjectComponentOverrides {
                remote_path: Some("override/plugins/fixture".to_string()),
                ..Default::default()
            },
        );

        let component = resolve_project_component(&project, "fixture").expect("component");

        assert_eq!(component.remote_path, "override/plugins/fixture");
    }

    #[test]
    fn project_resolution_uses_standalone_extract_command_as_fallback() {
        with_isolated_home(|home| {
            let repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
            let registered_repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
            let components_dir = home
                .path()
                .join(".config")
                .join("homeboy")
                .join("components");
            std::fs::create_dir_all(&components_dir).expect("components dir");
            std::fs::write(
                components_dir.join("fixture.json"),
                serde_json::json!({
                    "local_path": registered_repo.path(),
                    "extract_command": "unzip -o {{artifact}} && rm {{artifact}}",
                    "remote_url": "https://github.com/example/fixture.git"
                })
                .to_string(),
            )
            .expect("standalone component config");

            let project = project_with_attachment(None, repo.path().to_string_lossy().to_string());

            let component = resolve_project_component(&project, "fixture").expect("component");

            assert_eq!(component.local_path, repo.path().to_string_lossy());
            assert_eq!(
                component.extract_command.as_deref(),
                Some("unzip -o {{artifact}} && rm {{artifact}}")
            );
            assert_eq!(
                component.remote_url.as_deref(),
                Some("https://github.com/example/fixture.git")
            );
        });
    }

    #[test]
    fn project_overrides_win_over_standalone_extract_command() {
        with_isolated_home(|home| {
            let repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
            let components_dir = home
                .path()
                .join(".config")
                .join("homeboy")
                .join("components");
            std::fs::create_dir_all(&components_dir).expect("components dir");
            std::fs::write(
                components_dir.join("fixture.json"),
                serde_json::json!({
                    "local_path": repo.path(),
                    "extract_command": "unzip -o {{artifact}} && rm {{artifact}}"
                })
                .to_string(),
            )
            .expect("standalone component config");

            let mut project =
                project_with_attachment(None, repo.path().to_string_lossy().to_string());
            project.component_overrides.insert(
                "fixture".to_string(),
                ProjectComponentOverrides {
                    extract_command: Some("custom-extract {{artifact}}".to_string()),
                    ..Default::default()
                },
            );

            let component = resolve_project_component(&project, "fixture").expect("component");

            assert_eq!(
                component.extract_command.as_deref(),
                Some("custom-extract {{artifact}}")
            );
        });
    }

    #[test]
    fn materialized_source_binding_reapplies_all_explicit_project_overrides() {
        let source = Component {
            id: "fixture".to_string(),
            remote_path: "source/path".to_string(),
            build_artifact: Some("source.zip".to_string()),
            extract_command: Some("source-extract".to_string()),
            hooks: HashMap::from([("post:deploy".to_string(), vec!["source-hook".to_string()])]),
            scopes: Some(crate::component::ScopeConfig::default()),
            artifact_inputs: vec![crate::component::ArtifactInput {
                component: "source".to_string(),
                artifact: "source.zip".to_string(),
                target: "vendor".to_string(),
                sha256: None,
            }],
            cli_path: Some("source-cli".to_string()),
            ..Component::default()
        };
        let project = Project {
            id: "site".to_string(),
            component_overrides: HashMap::from([(
                "fixture".to_string(),
                ProjectComponentOverrides {
                    build_artifact: Some("project.zip".to_string()),
                    extract_command: Some("project-extract".to_string()),
                    hooks: HashMap::from([(
                        "post:deploy".to_string(),
                        vec!["project-hook".to_string()],
                    )]),
                    scopes: Some(crate::component::ScopeConfig {
                        deploy: Some(crate::component::CommandScopeConfig {
                            exclude: vec!["project-only".to_string()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    artifact_inputs: vec![crate::component::ArtifactInput {
                        component: "project".to_string(),
                        artifact: "project.zip".to_string(),
                        target: "vendor".to_string(),
                        sha256: None,
                    }],
                    cli_path: Some("project-cli".to_string()),
                    ..ProjectComponentOverrides::default()
                },
            )]),
            ..Project::default()
        };

        let bound = bind_materialized_component_to_project(
            source,
            &project,
            None,
            Some("project/path".to_string()),
            None,
        );

        assert_eq!(bound.remote_path, "project/path");
        assert_eq!(bound.build_artifact.as_deref(), Some("project.zip"));
        assert_eq!(bound.extract_command.as_deref(), Some("project-extract"));
        assert_eq!(bound.hooks["post:deploy"], ["project-hook"]);
        assert_eq!(
            bound
                .scopes
                .as_ref()
                .and_then(|scopes| scopes.deploy.as_ref())
                .map(|scope| scope.exclude.as_slice()),
            Some(["project-only".to_string()].as_slice())
        );
        assert_eq!(bound.artifact_inputs[0].component, "project");
        assert_eq!(bound.cli_path.as_deref(), Some("project-cli"));
    }

    #[test]
    fn status_resolution_reports_missing_component_local_path() {
        let project = project_with_attachment(
            Some("wp-content/plugins/fixture"),
            "/tmp/homeboy-missing-component-path".to_string(),
        );

        let err = resolve_project_component(&project, "fixture").expect_err("missing local_path");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("missing local_path"));
        assert!(err.hints.iter().any(|hint| {
            hint.message.contains(
                "Component 'fixture' local_path '/tmp/homeboy-missing-component-path' does not exist",
            )
        }));
    }

    /// The resolved attachment `local_path` must be normalized to the *same*
    /// absolute, lexically-clean checkout that `component set` persists — so
    /// `homeboy release <id>` (which reaches this resolver through the composed
    /// inventory) and `homeboy deploy` (which calls it directly) agree on one
    /// source of truth instead of resolving different checkouts for the same id.
    ///
    /// Regression coverage for #7410 / #6938. This exercises the normalization
    /// seam with a non-canonical attachment value (a `..` round-trip through the
    /// parent), which `release`'s `validate_local_path` would otherwise leave
    /// un-normalized while deploy silently accepted it. The value stays absolute
    /// throughout, so the test needs no process-CWD mutation and is race-free.
    #[test]
    fn attachment_local_path_normalizes_matching_component_set() {
        let repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
        let repo_path = repo.path().canonicalize().expect("canonical repo path");
        let dir_name = repo_path.file_name().expect("repo dir name");

        // A non-canonical but absolute attachment value: `<parent>/<dir>/../<dir>`.
        // It points at the same checkout but is not lexically normalized, mirroring
        // the drifted values that made `release` and `deploy` disagree.
        let noncanonical = repo_path
            .join("..")
            .join(dir_name)
            .to_string_lossy()
            .to_string();

        let project = project_with_attachment(None, noncanonical.clone());

        let resolved = resolve_project_component(&project, "fixture").expect("attachment resolves");

        assert!(
            std::path::Path::new(&resolved.local_path).is_absolute(),
            "resolved local_path must be absolute, got {:?}",
            resolved.local_path
        );

        // The resolver must agree with the write-side normalization that
        // `component set` applies to the same value — one source of truth.
        let component_set_value = crate::component::normalize_component_local_path(&noncanonical)
            .expect("normalize like component set");
        assert_eq!(resolved.local_path, component_set_value);

        // And it must point at the real checkout.
        assert_eq!(
            std::path::Path::new(&resolved.local_path)
                .canonicalize()
                .expect("canonical resolved path"),
            repo_path
        );
    }
}
