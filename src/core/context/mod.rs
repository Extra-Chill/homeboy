use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::component;
use crate::core::error::{Error, Result};
use crate::core::extension::{self, DiscoveryMarkerConfig, ExtensionManifest};
use crate::core::project::{self, Project};
use crate::core::server::SshClient;
use crate::core::server::{self, Server};

pub mod report;

pub use report::{build_report, build_report_for_component};

// === Local Context Detection (homeboy context command) ===

#[derive(Debug, Clone, Serialize)]

pub struct ComponentGap {
    pub field: String,
    pub reason: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct ContainedComponentInfo {
    pub id: String,
    pub build_artifact: String,
    pub remote_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ComponentGap>,
}

#[derive(Debug, Clone, Serialize)]

pub struct ProjectContext {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize)]

pub struct ContextOutput {
    #[serde(skip_serializing)]
    pub command: String,
    pub cwd: String,
    pub git_root: Option<String>,
    pub managed: bool,
    pub matched_components: Vec<String>,
    #[serde(skip_serializing)]
    pub contained_components: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Detect local working directory context.
/// Returns info about git root, matched components, and whether directory is managed.
pub fn run(path: Option<&str>) -> Result<(ContextOutput, i32)> {
    let cwd = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| Error::internal_io(e.to_string(), None))?,
    };

    let cwd_str = cwd.to_string_lossy().to_string();
    let resolved_target = component::resolve_target(component::TargetSpec {
        path_override: Some(&cwd_str),
        allow_synthetic: true,
        accept_bare_directory: true,
        ..component::TargetSpec::default()
    })?;
    let git_root = resolved_target
        .git_root
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());

    let components = component::inventory().unwrap_or_default();
    let matched_components: Vec<String> = components
        .iter()
        .filter(|c| component::resolution::component_contains_path(c, &cwd))
        .map(|c| c.id.clone())
        .collect();

    let mut matched_set: HashSet<String> = matched_components.into_iter().collect();
    if !resolved_target.synthetic {
        matched_set.insert(resolved_target.component_id.clone());
    }
    let mut matched: Vec<String> = matched_set.into_iter().collect();
    matched.sort();

    let managed = !matched.is_empty();

    // Check for contained components (monorepo pattern)
    let all_local_components: Vec<component::Component> = components;

    let contained: Vec<&component::Component> = all_local_components
        .iter()
        .filter(|c| component::resolution::component_is_contained_in_path(c, &cwd))
        .collect();

    let contained_ids: Vec<String> = contained.iter().map(|c| c.id.clone()).collect();

    // Find project if all contained components belong to one
    let project_ctx = if !contained_ids.is_empty() {
        find_project_for_components(&contained_ids).map(|p| ProjectContext {
            id: p.id.clone(),
            domain: p.domain.clone(),
        })
    } else {
        None
    };

    // Generate context-aware suggestion
    let suggestion = if managed {
        None
    } else if let Some(ref git_root_str) = git_root {
        // If we're in a git repo that *looks like* a configured component, suggest relinking.
        let git_root_path = PathBuf::from(git_root_str);
        let repo_name = git_root_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string());

        let relink_match = repo_name.as_ref().and_then(|name| {
            all_local_components
                .iter()
                .find(|c| c.id == *name || c.aliases.iter().any(|a| a == name))
        });

        if let Some(component_match) = relink_match {
            // Only suggest relink if paths differ (canonicalize best-effort)
            let git_canon = git_root_path.canonicalize().ok();
            let comp_canon = PathBuf::from(&component_match.local_path)
                .canonicalize()
                .ok();

            if git_canon.is_some() && comp_canon.is_some() && git_canon != comp_canon {
                // JSON-escape just enough for typical paths (quotes + backslashes).
                let json_path = git_root_str.replace('\\', "\\\\").replace('"', "\\\"");
                Some(format!(
                    "This looks like component '{}' but its repo path is set to '{}'. To reattach it to a project, use: homeboy project components attach-path <project-id> {}",
                    component_match.id,
                    component_match.local_path,
                    json_path
                ))
            } else {
                Some(format!(
                    "Repo detected. Prefer attaching it to a project: `homeboy project components attach-path <project-id> {}`",
                    git_root_str
                ))
            }
        } else if !contained_ids.is_empty() {
            if let Some(ref proj) = project_ctx {
                Some(format!(
                    "Monorepo root for project {} with {} components. Use `homeboy project show {}` for full details.",
                    proj.id,
                    contained_ids.len(),
                    proj.id
                ))
            } else {
                Some(format!(
                    "Directory contains {} configured components. Use `homeboy component show <id>` to see a specific component's configuration.",
                    contained_ids.len()
                ))
            }
        } else {
            Some(format!(
                "Repo detected. Prefer attaching it to a project: `homeboy project components attach-path <project-id> {}`",
                git_root_str
            ))
        }
    } else if !contained_ids.is_empty() {
        if let Some(ref proj) = project_ctx {
            Some(format!(
                "Monorepo root for project {} with {} components. Use `homeboy project show {}` for full details.",
                proj.id,
                contained_ids.len(),
                proj.id
            ))
        } else {
            Some(format!(
                "Directory contains {} configured components. Use `homeboy component show <id>` to see a specific component's configuration.",
                contained_ids.len()
            ))
        }
    } else {
        Some(
            "Repo not attached. Prefer: `homeboy project components attach-path <project-id> <path>`"
                .to_string(),
        )
    };

    Ok((
        ContextOutput {
            command: "context.show".to_string(),
            cwd: cwd_str,
            git_root,
            managed,
            matched_components: matched,
            contained_components: contained_ids,
            project: project_ctx,
            suggestion,
        },
        0,
    ))
}

fn find_project_for_components(component_ids: &[String]) -> Option<project::Project> {
    if component_ids.is_empty() {
        return None;
    }
    let projects = project::list().ok()?;
    projects
        .into_iter()
        .find(|p| component_ids.iter().all(|id| project::has_component(p, id)))
}

pub fn build_component_info(component: &component::Component) -> ContainedComponentInfo {
    let mut gaps = Vec::new();
    let local_path = PathBuf::from(&component.local_path);

    // Check for build configuration gaps.
    // Components should link an extension with build support.
    let extension_provides_build = component
        .extensions
        .as_ref()
        .map(|extensions| {
            extensions.keys().any(|extension_id| {
                extension::load_extension(extension_id)
                    .ok()
                    .is_some_and(|m| m.has_build())
            })
        })
        .unwrap_or(false);

    if !extension_provides_build && local_path.join("build.sh").exists() {
        gaps.push(ComponentGap {
            field: "extensions".to_string(),
            reason: "build.sh exists but no linked extension provides build support".to_string(),
            command: format!(
                "homeboy component set {} --extension <extension_id>",
                component.id
            ),
        });
    }

    // Check for missing build artifact when component appears deployable
    if component.build_artifact.is_none() && !component.remote_path.is_empty() {
        // Check if extension provides a pattern (would be resolved at deploy time)
        if !component::extension_provides_artifact_pattern(component) {
            // Component has remote_path but no artifact source
            gaps.push(ComponentGap {
                field: "buildArtifact".to_string(),
                reason: "Component has remotePath but no buildArtifact or extension pattern"
                    .to_string(),
                command: format!(
                    "homeboy component set {} --build-artifact \"build/{}.zip\"",
                    component.id, component.id
                ),
            });
        }
    }

    // Check for missing extension configuration
    if component.extensions.is_none() || component.extensions.as_ref().is_none_or(|m| m.is_empty())
    {
        let suggestions = extension_suggestions_for_path(&local_path);
        let extension_hint = if suggestions.len() == 1 {
            suggestions[0].as_str()
        } else {
            "EXTENSION_ID"
        };
        let reason = if suggestions.len() > 1 {
            format!(
                "No extension configured. Matching extension manifests: {}.",
                suggestions.join(", ")
            )
        } else {
            "No extension configured. Extension commands (lint, test, build) require a extension."
                .to_string()
        };
        gaps.push(ComponentGap {
            field: "extensions".to_string(),
            reason,
            command: format!(
                "homeboy component set {} --extension {}",
                component.id, extension_hint
            ),
        });
    }

    // Check for changelog without changelogTarget
    if component.changelog_target.is_none() {
        let changelog_candidates = [
            "CHANGELOG.md",
            "changelog.md",
            "docs/CHANGELOG.md",
            "docs/changelog.md",
            "HISTORY.md",
        ];

        for candidate in changelog_candidates {
            if local_path.join(candidate).exists() {
                gaps.push(ComponentGap {
                    field: "changelogTarget".to_string(),
                    reason: format!("{} exists", candidate),
                    command: format!(
                        "homeboy component set {} --changelog-target \"{}\"",
                        component.id, candidate
                    ),
                });
                break;
            }
        }
    }

    ContainedComponentInfo {
        id: component.id.clone(),
        build_artifact: component.build_artifact.clone().unwrap_or_default(),
        remote_path: component.remote_path.clone(),
        gaps,
    }
}

fn extension_suggestions_for_path(local_path: &Path) -> Vec<String> {
    extension::load_all_extensions()
        .map(|extensions| extension_suggestions_from_manifests(local_path, &extensions))
        .unwrap_or_default()
}

fn extension_suggestions_from_manifests(
    local_path: &Path,
    extensions: &[ExtensionManifest],
) -> Vec<String> {
    let mut suggestions: Vec<String> = extensions
        .iter()
        .filter(|manifest| {
            manifest
                .discovery_markers()
                .iter()
                .any(|rule| discovery_marker_matches(local_path, rule))
        })
        .map(|manifest| manifest.id.clone())
        .collect();
    suggestions.sort();
    suggestions.dedup();
    suggestions
}

fn discovery_marker_matches(local_path: &Path, rule: &DiscoveryMarkerConfig) -> bool {
    if rule.all.is_empty() && rule.any.is_empty() {
        return false;
    }
    let all_match = rule
        .all
        .iter()
        .all(|marker| marker_exists(local_path, marker));
    let any_match = rule.any.is_empty()
        || rule
            .any
            .iter()
            .any(|marker| marker_exists(local_path, marker));

    all_match && any_match
}

fn marker_exists(local_path: &Path, marker: &str) -> bool {
    if marker.contains('*') || marker.contains('?') || marker.contains('[') {
        let pattern = local_path.join(marker).to_string_lossy().to_string();
        glob::glob(&pattern)
            .ok()
            .is_some_and(|mut matches| matches.any(|entry| entry.is_ok()))
    } else {
        local_path.join(marker).exists()
    }
}

// === Project/Server Context Resolution ===

pub(crate) struct ProjectServerContext {
    pub project: Project,
    pub server_id: String,
    pub server: Server,
}

pub(crate) fn resolve_project_server(project_id: &str) -> Result<ProjectServerContext> {
    let project = project::load(project_id)?;

    let server_id = project.server_id.clone().ok_or_else(|| {
        Error::config_missing_key("project.server_id", Some(project_id.to_string()))
    })?;

    let server =
        server::load(&server_id).map_err(|_| Error::server_not_found(server_id.clone(), vec![]))?;

    Ok(ProjectServerContext {
        project,
        server_id,
        server,
    })
}

pub fn require_project_base_path(project_id: &str, project: &Project) -> Result<String> {
    project
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::config_missing_key("project.base_path", Some(project_id.to_string())))
}

pub struct RemoteProjectContext {
    pub project: Project,
    pub server_id: String,
    pub server: Server,
    pub client: SshClient,
    pub base_path: Option<String>,
}

pub fn resolve_project_ssh(project_id: &str) -> Result<RemoteProjectContext> {
    let ctx = resolve_project_server(project_id)?;
    let client = SshClient::from_server(&ctx.server, &ctx.server_id)?;

    Ok(RemoteProjectContext {
        base_path: ctx.project.base_path.clone(),
        project: ctx.project,
        server_id: ctx.server_id,
        server: ctx.server,
        client,
    })
}

pub fn resolve_project_ssh_with_base_path(
    project_id: &str,
) -> Result<(RemoteProjectContext, String)> {
    let ctx = resolve_project_ssh(project_id)?;
    let base_path = require_project_base_path(project_id, &ctx.project)?;
    Ok((ctx, base_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    fn manifest(id: &str, markers: serde_json::Value) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "provides": {
                "discovery_markers": markers
            }
        }))
        .expect("manifest parses");
        manifest.id = id.to_string();
        manifest
    }

    #[test]
    fn extension_suggestions_are_manifest_driven() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("cargo marker");

        let suggestions = extension_suggestions_from_manifests(
            dir.path(),
            &[
                manifest("rust-like", serde_json::json!([{ "all": ["Cargo.toml"] }])),
                manifest(
                    "wordpress-like",
                    serde_json::json!([{ "all": ["style.css", "functions.php"] }]),
                ),
            ],
        );

        assert_eq!(suggestions, vec!["rust-like"]);
    }

    #[test]
    fn extension_suggestions_support_globs_and_ambiguity() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("package marker");
        std::fs::write(dir.path().join("src/index.ts"), "export {};").expect("ts marker");

        let suggestions = extension_suggestions_from_manifests(
            dir.path(),
            &[
                manifest(
                    "node-like",
                    serde_json::json!([{ "all": ["package.json"] }]),
                ),
                manifest(
                    "typescript-like",
                    serde_json::json!([{ "all": ["package.json"], "any": ["src/**/*.ts"] }]),
                ),
            ],
        );

        assert_eq!(suggestions, vec!["node-like", "typescript-like"]);
    }

    fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
        let dir = home.join(".config/homeboy/components");
        std::fs::create_dir_all(&dir).expect("components dir");
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::json!({
                "local_path": local_path,
                "remote_path": format!("wp-content/plugins/{id}")
            })
            .to_string(),
        )
        .expect("component registration");
    }

    fn write_project(home: &Path, id: &str, component_ids: &[&str]) {
        let dir = home.join(".config/homeboy/projects").join(id);
        std::fs::create_dir_all(&dir).expect("project dir");
        let components: Vec<_> = component_ids
            .iter()
            .map(|component_id| {
                serde_json::json!({
                    "id": component_id,
                    "local_path": ""
                })
            })
            .collect();
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::json!({
                "domain": "example.test",
                "components": components
            })
            .to_string(),
        )
        .expect("project registration");
    }

    fn init_git(path: &Path) {
        let output = std::process::Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .expect("git init");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn context_uses_registered_component_resolution_for_cwd_inside_component() {
        with_isolated_home(|home| {
            let repo = home.path().join("registered-component");
            let nested = repo.join("src");
            std::fs::create_dir_all(&nested).expect("nested dir");
            init_git(&repo);
            write_component_registration(home.path(), "registered-component", &repo);

            let (output, status) = run(Some(nested.to_str().expect("utf8 path"))).expect("context");

            assert_eq!(status, 0);
            assert!(output.managed);
            assert_eq!(output.matched_components, vec!["registered-component"]);
            assert_eq!(
                output.git_root,
                Some(repo.canonicalize().unwrap().to_string_lossy().to_string())
            );
            assert_eq!(output.suggestion, None);
        });
    }

    #[test]
    fn context_uses_portable_config_resolution_for_unregistered_cwd() {
        with_isolated_home(|home| {
            let repo = home.path().join("portable-component");
            let nested = repo.join("lib");
            std::fs::create_dir_all(&nested).expect("nested dir");
            init_git(&repo);
            std::fs::write(
                repo.join("homeboy.json"),
                serde_json::json!({
                    "id": "portable-component",
                    "remote_path": "wp-content/plugins/portable-component"
                })
                .to_string(),
            )
            .expect("portable config");

            let (output, status) = run(Some(nested.to_str().expect("utf8 path"))).expect("context");

            assert_eq!(status, 0);
            assert!(output.managed);
            assert_eq!(output.matched_components, vec!["portable-component"]);
            assert_eq!(
                output.git_root,
                Some(repo.canonicalize().unwrap().to_string_lossy().to_string())
            );
            assert_eq!(output.suggestion, None);
        });
    }

    #[test]
    fn context_preserves_monorepo_contained_component_suggestion() {
        with_isolated_home(|home| {
            let monorepo = home.path().join("monorepo");
            let plugin = monorepo.join("plugins/demo-plugin");
            let theme = monorepo.join("themes/demo-theme");
            std::fs::create_dir_all(&plugin).expect("plugin dir");
            std::fs::create_dir_all(&theme).expect("theme dir");
            init_git(&monorepo);
            write_component_registration(home.path(), "demo-plugin", &plugin);
            write_component_registration(home.path(), "demo-theme", &theme);
            write_project(home.path(), "demo-site", &["demo-plugin", "demo-theme"]);

            let (output, status) =
                run(Some(monorepo.to_str().expect("utf8 path"))).expect("context");

            assert_eq!(status, 0);
            assert!(!output.managed);
            assert_eq!(output.contained_components.len(), 2);
            assert_eq!(
                output.project.as_ref().map(|project| project.id.as_str()),
                Some("demo-site")
            );
            assert_eq!(
                output.suggestion,
                Some(
                    "Monorepo root for project demo-site with 2 components. Use `homeboy project show demo-site` for full details."
                        .to_string()
                )
            );
        });
    }
}
