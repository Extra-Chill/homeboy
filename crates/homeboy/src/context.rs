use serde::Serialize;
use std::path::PathBuf;

use crate::config::{ConfigManager, ProjectConfiguration, ServerConfig};
use crate::ssh::SshClient;
use crate::{Error, Result};

// === Local Context Detection (homeboy context command) ===

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextOutput {
    pub command: String,
    pub cwd: String,
    pub git_root: Option<String>,
    pub managed: bool,
    pub matched_components: Vec<String>,
    pub suggestion: Option<String>,
}

/// Detect local working directory context.
/// Returns info about git root, matched components, and whether directory is managed.
pub fn run(path: Option<&str>) -> Result<(ContextOutput, i32)> {
    let cwd = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()
            .map_err(|e| Error::internal_io(e.to_string(), None))?,
    };

    let cwd_str = cwd.to_string_lossy().to_string();
    let git_root = detect_git_root(&cwd);

    let components = ConfigManager::list_components().unwrap_or_default();

    let matched: Vec<String> = components
        .iter()
        .filter(|c| path_matches(&cwd, &c.local_path))
        .map(|c| c.id.clone())
        .collect();

    let managed = !matched.is_empty();

    let suggestion = if managed {
        None
    } else {
        Some(
            "This directory is not managed by Homeboy. To initialize, create a project or component."
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

fn path_matches(cwd: &PathBuf, local_path: &str) -> bool {
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

// === Project/Server Context Resolution ===

pub struct ProjectServerContext {
    pub project: ProjectConfiguration,
    pub server_id: String,
    pub server: ServerConfig,
}

pub enum ResolvedTarget {
    Project(Box<ProjectServerContext>),
    Server {
        server_id: String,
        server: ServerConfig,
    },
}

pub fn resolve_project_server(project_id: &str) -> Result<ProjectServerContext> {
    let project = ConfigManager::load_project(project_id)?;

    let server_id = project.server_id.clone().ok_or_else(|| {
        Error::config_missing_key("project.serverId", Some(project_id.to_string()))
    })?;

    let server = ConfigManager::load_server(&server_id)
        .map_err(|_| Error::server_not_found(server_id.clone()))?;

    Ok(ProjectServerContext {
        project,
        server_id,
        server,
    })
}

pub fn require_project_base_path(
    project_id: &str,
    project: &ProjectConfiguration,
) -> Result<String> {
    project
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::config_missing_key("project.basePath", Some(project_id.to_string())))
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

    let server =
        ConfigManager::load_server(id).map_err(|_| Error::server_not_found(id.to_string()))?;

    Ok(ResolvedTarget::Server {
        server_id: id.to_string(),
        server,
    })
}

pub struct RemoteProjectContext {
    pub project: ProjectConfiguration,
    pub server_id: String,
    pub server: ServerConfig,
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
