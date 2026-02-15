use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::component;
use crate::error::{Error, Result};
use crate::module;
use crate::paths;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
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

    let matched: Vec<String> = components
        .iter()
        .filter(|c| path_matches(&cwd, &c.local_path))
        .map(|c| c.id.clone())
        .collect();

    let managed = !matched.is_empty();

    // Check for contained components (monorepo pattern)
    let contained: Vec<&component::Component> = components
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
            components.iter().find(|c| c.id == *name || c.aliases.iter().any(|a| a == name))
        });

        if let Some(component_match) = relink_match {
            // Only suggest relink if paths differ (canonicalize best-effort)
            let git_canon = git_root_path.canonicalize().ok();
            let comp_canon = PathBuf::from(&component_match.local_path).canonicalize().ok();

            if git_canon.is_some() && comp_canon.is_some() && git_canon != comp_canon {
                // JSON-escape just enough for typical paths (quotes + backslashes).
                let json_path = git_root_str.replace('\\', "\\\\").replace('"', "\\\"");
                Some(format!(
                    "This looks like component '{}' but local_path is set to '{}'. To relink: homeboy component set {} --json '{{\"local_path\":\"{}\"}}' (edit JSON if your path contains unusual characters)",
                    component_match.id,
                    component_match.local_path,
                    component_match.id,
                    json_path
                ))
            } else {
                Some(format!(
                    "This directory is a git repo ('{}') but is not managed by Homeboy. Run `homeboy init --all` to see all components.",
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
                "This directory is a git repo ('{}') but is not managed by Homeboy. Run `homeboy init --all` to see all components.",
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
            "This directory is not managed by Homeboy. Run `homeboy init --all` to see all components."
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

    // Check for build configuration gaps
    // Skip gap detection if:
    // 1. Component has explicit buildCommand, OR
    // 2. Component's module provides a bundled build script
    if component.build_command.is_none() {
        let module_provides_build = component
            .modules
            .as_ref()
            .map(|modules| {
                modules.keys().any(|module_id| {
                    module::load_module(module_id)
                        .ok()
                        .and_then(|m| m.build)
                        .and_then(|b| b.module_script)
                        .and_then(|script| {
                            paths::module(module_id)
                                .ok()
                                .map(|dir| dir.join(&script).exists())
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        // Only flag as gap if module doesn't provide build and local build.sh exists
        if !module_provides_build && local_path.join("build.sh").exists() {
            gaps.push(ComponentGap {
                field: "buildCommand".to_string(),
                reason: "build.sh exists".to_string(),
                command: format!(
                    "homeboy component set {} --build-command \"./build.sh\"",
                    component.id
                ),
            });
        }
    }

    // Check for missing build artifact when component appears deployable
    if component.build_artifact.is_none() && !component.remote_path.is_empty() {
        // Check if module provides a pattern (would be resolved at deploy time)
        if !component::module_provides_artifact_pattern(component) {
            // Component has remote_path but no artifact source
            gaps.push(ComponentGap {
                field: "buildArtifact".to_string(),
                reason: "Component has remotePath but no buildArtifact or module pattern".to_string(),
                command: format!(
                    "homeboy component set {} --build-artifact \"build/{}.zip\"",
                    component.id, component.id
                ),
            });
        }
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
        build_command: component.build_command.clone(),
        remote_path: component.remote_path.clone(),
        gaps,
    }
}

// === Project/Server Context Resolution ===

pub struct ProjectServerContext {
    pub project: Project,
    pub server_id: String,
    pub server: Server,
}

pub enum ResolvedTarget {
    Project(Box<ProjectServerContext>),
    Server { server_id: String, server: Server },
}

pub fn resolve_project_server(project_id: &str) -> Result<ProjectServerContext> {
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

pub fn resolve_project_server_with_base_path(
    project_id: &str,
) -> Result<(ProjectServerContext, String)> {
    let ctx = resolve_project_server(project_id)?;
    let base_path = require_project_base_path(project_id, &ctx.project)?;
    Ok((ctx, base_path))
}

pub fn resolve_project_or_server_id(id: &str) -> Result<ResolvedTarget> {
    if let Ok(ctx) = resolve_project_server(id) {
        return Ok(ResolvedTarget::Project(Box::new(ctx)));
    }

    let server = server::load(id).map_err(|_| Error::server_not_found(id.to_string(), vec![]))?;

    Ok(ResolvedTarget::Server {
        server_id: id.to_string(),
        server,
    })
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
