mod client;

pub use client::*;

use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::{self, Server};

/// Arguments for SSH context resolution
#[derive(Default)]
pub struct SshResolveArgs {
    /// Bare ID (tries project first, then server)
    pub id: Option<String>,
    /// Force project resolution
    pub project: Option<String>,
    /// Force server resolution
    pub server: Option<String>,
}

/// Result of SSH context resolution
#[derive(Debug)]
pub struct SshResolveResult {
    /// How the target was resolved ("project" or "server")
    pub resolved_type: String,
    /// Project ID if resolved via project
    pub project_id: Option<String>,
    /// Server ID
    pub server_id: String,
    /// Resolved server configuration
    pub server: Server,
    /// Project base_path (only when resolved via project)
    pub base_path: Option<String>,
}

/// Resolve SSH connection context from arguments.
/// All validation happens here - returns a ready-to-connect result.
pub fn resolve_context(args: &SshResolveArgs) -> Result<SshResolveResult> {
    // Validation: At least one ID must be provided
    if args.id.is_none() && args.project.is_none() && args.server.is_none() {
        return Err(Error::validation_missing_argument(vec![
            "<id>".to_string(),
            "--project".to_string(),
            "--server".to_string(),
        ]));
    }

    // Resolution logic
    let (resolved_type, project_id, server_id, server, base_path) = resolve_internal(args)?;

    // Validation: Server must have required fields
    if !server.is_valid() {
        let mut missing = Vec::new();
        if server.host.is_empty() {
            missing.push("host".to_string());
        }
        if server.user.is_empty() {
            missing.push("user".to_string());
        }
        return Err(Error::ssh_server_invalid(server_id, missing));
    }

    Ok(SshResolveResult {
        resolved_type,
        project_id,
        server_id,
        server,
        base_path,
    })
}

fn resolve_internal(
    args: &SshResolveArgs,
) -> Result<(String, Option<String>, String, Server, Option<String>)> {
    // --project flag: force project resolution
    if let Some(project_id) = &args.project {
        let project = project::load(project_id)?;
        let base_path = project.base_path.clone();
        let (server_id, server) = resolve_from_project(&project)?;
        return Ok((
            "project".to_string(),
            Some(project.id),
            server_id,
            server,
            base_path,
        ));
    }

    // --server flag: force server resolution
    if let Some(server_id) = &args.server {
        let server = server::load(server_id)?;
        return Ok(("server".to_string(), None, server_id.clone(), server, None));
    }

    // Bare id: try project first, then server
    let id = args.id.as_ref().unwrap(); // Safe: validated above

    if let Ok(project) = project::load(id) {
        let base_path = project.base_path.clone();
        let (server_id, server) = resolve_from_project(&project)?;
        return Ok((
            "project".to_string(),
            Some(project.id),
            server_id,
            server,
            base_path,
        ));
    }

    if let Ok(server) = server::load(id) {
        return Ok(("server".to_string(), None, id.clone(), server, None));
    }

    Err(Error::validation_invalid_argument(
        "id",
        "No matching project or server",
        Some(id.clone()),
        Some(vec!["project".to_string(), "server".to_string()]),
    ))
}

fn resolve_from_project(project: &Project) -> Result<(String, Server)> {
    let server_id = project.server_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "project.server_id",
            "Server not configured for project",
            None,
            None,
        )
    })?;
    let server = server::load(&server_id)?;
    Ok((server_id, server))
}
