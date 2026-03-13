//! SSH tunnel for remote database connections.
//!
//! Creates an SSH tunnel that forwards a local port to the remote database,
//! allowing local tools (GUI clients, CLIs) to connect to remote databases.

use serde::Serialize;
use std::process::{Command, Stdio};

use crate::context::resolve_project_ssh;
use crate::project;
use crate::{Error, Result};

const DEFAULT_DATABASE_HOST: &str = "127.0.0.1";
const DEFAULT_LOCAL_DB_PORT: u16 = 33306;

#[derive(Serialize, Clone)]
pub struct DbTunnelInfo {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub database: String,
    pub user: String,
}

#[derive(Serialize, Clone)]
pub struct DbTunnelResult {
    pub project_id: String,
    pub base_path: Option<String>,
    pub domain: Option<String>,
    pub exit_code: i32,
    pub success: bool,
    pub tunnel: DbTunnelInfo,
}

pub fn create_tunnel(project_id: &str, local_port: Option<u16>) -> Result<DbTunnelResult> {
    let project = project::load(project_id)?;
    let ctx = resolve_project_ssh(project_id)?;
    let server = ctx.server;
    let client = ctx.client;

    let remote_host = if project.database.host.is_empty() {
        DEFAULT_DATABASE_HOST.to_string()
    } else {
        project.database.host.clone()
    };

    let remote_port = project.database.port;
    let bind_port = local_port.unwrap_or(DEFAULT_LOCAL_DB_PORT);

    let tunnel_info = DbTunnelInfo {
        local_port: bind_port,
        remote_host: remote_host.clone(),
        remote_port,
        database: project.database.name.clone(),
        user: project.database.user.clone(),
    };

    let mut ssh_args = Vec::new();

    if let Some(identity_file) = &client.identity_file {
        ssh_args.push("-i".to_string());
        ssh_args.push(identity_file.clone());
    }

    if server.port != 22 {
        ssh_args.push("-p".to_string());
        ssh_args.push(server.port.to_string());
    }

    ssh_args.push("-N".to_string());
    ssh_args.push("-L".to_string());
    ssh_args.push(format!("{}:{}:{}", bind_port, remote_host, remote_port));
    ssh_args.push(format!("{}@{}", server.user, server.host));

    let status = Command::new("ssh")
        .args(&ssh_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let exit_code = match status {
        Ok(s) => s.code().unwrap_or(0),
        Err(e) => {
            return Err(Error::internal_io(
                e.to_string(),
                Some("SSH tunnel".to_string()),
            ))
        }
    };

    let success = exit_code == 0 || exit_code == 130;

    Ok(DbTunnelResult {
        project_id: project_id.to_string(),
        base_path: project.base_path.clone(),
        domain: project.domain.clone(),
        exit_code,
        success,
        tunnel: tunnel_info,
    })
}
