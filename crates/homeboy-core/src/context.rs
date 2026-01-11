use crate::config::{ConfigManager, ProjectConfiguration, ServerConfig};
use crate::ssh::SshClient;
use crate::{Error, Result};

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
