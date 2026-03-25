//! project_server_context — extracted from mod.rs.

use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::SshClient;
use crate::server::{self, Server};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use crate::core::context::RemoteProjectContext;
use crate::core::context::ProjectServerContext;


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
