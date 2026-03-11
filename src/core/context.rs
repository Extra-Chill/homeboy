use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::component;
use crate::error::{Error, Result};
use crate::extension;
use crate::project::{self, Project};
use crate::server::{self, Server};
use crate::ssh::SshClient;

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
    let git_root = detect_git_root(&cwd);

    let components = component::list().unwrap_or_default();
    let projects = project::list().unwrap_or_default();
    let attached_components = collect_attached_components(&projects);

    let matched_registered: Vec<String> = components
        .iter()
        .filter(|c| path_matches(&cwd, &c.local_path))
        .map(|c| c.id.clone())
        .collect();

    let matched_attached: Vec<String> = attached_components
        .iter()
        .filter(|c| path_matches(&cwd, &c.local_path))
        .map(|c| c.id.clone())
        .collect();

    let matched: Vec<String> = matched_registered
        .into_iter()
        .chain(matched_attached)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let managed = !matched.is_empty();

    // Check for contained components (monorepo pattern)
    let all_local_components: Vec<component::Component> = components
        .into_iter()
        .chain(attached_components)
        .collect();

    let contained: Vec<&component::Component> = all_local_components
        .iter()
        .filter(|c| path_is_parent_of(&cwd, &c.local_path))
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

fn collect_attached_components(projects: &[project::Project]) -> Vec<component::Component> {
    let mut components = Vec::new();
    let mut seen = HashSet::new();

    for project in projects {
        for attachment in &project.components {
            let Some(local_path) = attachment.local_path.as_deref() else {
                continue;
            };

            if !seen.insert((attachment.id.clone(), local_path.to_string())) {
                continue;
            }

            if let Some(mut component) = component::discover_from_portable(Path::new(local_path)) {
                component.id = attachment.id.clone();
                components.push(component);
            }
        }
    }

    components
}

fn detect_git_root(cwd: &PathBuf) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }
    None
}

fn path_matches(cwd: &Path, local_path: &str) -> bool {
    let local = PathBuf::from(local_path);

    let cwd_canonical = cwd.canonicalize().ok();
    let local_canonical = local.canonicalize().ok();

    match (cwd_canonical, local_canonical) {
        (Some(cwd_path), Some(local_path)) => {
            cwd_path == local_path || cwd_path.starts_with(&local_path)
        }
        _ => false,
    }
}

pub fn path_is_parent_of(parent: &Path, child_path: &str) -> bool {
    let child = PathBuf::from(child_path);
    match (parent.canonicalize().ok(), child.canonicalize().ok()) {
        (Some(parent_canonical), Some(child_canonical)) => {
            child_canonical.starts_with(&parent_canonical) && child_canonical != parent_canonical
        }
        _ => false,
    }
}

fn find_project_for_components(component_ids: &[String]) -> Option<project::Project> {
    if component_ids.is_empty() {
        return None;
    }
    let projects = project::list().ok()?;
    projects
        .into_iter()
        .find(|p| component_ids.iter().all(|id| p.component_ids.contains(id)))
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
        // Suggest a extension based on project file indicators
        let suggestion =
            if local_path.join("style.css").exists() && local_path.join("functions.php").exists() {
                Some("wordpress")
            } else if local_path.join("Cargo.toml").exists() {
                Some("rust")
            } else if local_path.join("package.json").exists() {
                Some("nodejs")
            } else {
                None
            };

        let extension_hint = suggestion.unwrap_or("EXTENSION_ID");
        gaps.push(ComponentGap {
            field: "extensions".to_string(),
            reason: "No extension configured. Extension commands (lint, test, build) require a extension."
                .to_string(),
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
