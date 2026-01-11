use clap::Args;
use homeboy_core::config::ConfigManager;
use homeboy_core::ssh::SshClient;
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct SshArgs {
    /// Project ID
    pub project_id: String,

    /// Command to execute (omit for interactive shell)
    pub command: Option<String>,
}

#[derive(Serialize)]
pub struct SshOutput {
    pub project_id: String,
    pub command: Option<String>,
}

pub fn run(args: SshArgs) -> CmdResult<SshOutput> {
    let project = ConfigManager::load_project(&args.project_id)?;

    let server_id = project.server_id.ok_or_else(|| {
        homeboy_core::Error::Other("Server not configured for project".to_string())
    })?;

    let server = ConfigManager::load_server(&server_id)?;

    if !server.is_valid() {
        return Err(homeboy_core::Error::Other(
            "Server is not properly configured".to_string(),
        ));
    }

    let client = SshClient::from_server(&server, &server_id)?;

    let exit_code = client.execute_interactive(args.command.as_deref());

    Ok((
        SshOutput {
            project_id: args.project_id,
            command: args.command,
        },
        exit_code,
    ))
}
