use clap::{Args, Subcommand};
use homeboy::server::{self, Server};
use homeboy::shell;
use homeboy::ssh::{resolve_context, SshClient, SshResolveArgs};
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct SshArgs {
    /// Target ID (project or server; project wins when ambiguous)
    pub target: Option<String>,

    /// Command to execute (omit for interactive shell)
    pub command: Option<String>,

    /// Force interpretation as server ID
    #[arg(long)]
    pub as_server: bool,

    #[command(subcommand)]
    pub subcommand: Option<SshSubcommand>,
}

#[derive(Subcommand)]
pub enum SshSubcommand {
    /// List configured SSH server targets
    List,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action")]
pub enum SshOutput {
    Connect(SshConnectOutput),
    List(SshListOutput),
}

#[derive(Debug, Serialize)]

pub struct SshConnectOutput {
    pub resolved_type: String,
    pub project_id: Option<String>,
    pub server_id: String,
    pub command: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct SshListOutput {
    pub servers: Vec<Server>,
}

pub fn run(args: SshArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<SshOutput> {
    match args.subcommand {
        Some(SshSubcommand::List) => {
            let servers = server::list()?;
            Ok((SshOutput::List(SshListOutput { servers }), 0))
        }
        None => {
            // Build resolve args based on simplified CLI args
            let resolve_args = if args.as_server {
                SshResolveArgs {
                    id: None,
                    project: None,
                    server: args.target.clone(),
                }
            } else {
                SshResolveArgs {
                    id: args.target.clone(),
                    project: None,
                    server: None,
                }
            };
            let result = resolve_context(&resolve_args)?;

            // When project is resolved with base_path, auto-cd to project root
            let effective_command = match (&result.project_id, &result.base_path, &args.command) {
                // Project with base_path and command: cd to base_path then run command
                (Some(_), Some(bp), Some(cmd)) => {
                    Some(format!("cd {} && {}", shell::quote_path(bp), cmd))
                }
                // Project with base_path, no command: interactive shell starts in base_path
                (Some(_), Some(bp), None) => Some(format!("cd {}", shell::quote_path(bp))),
                // No project context or no base_path: use command as-is
                _ => args.command.clone(),
            };

            // Execute interactive SSH (CLI-owned TTY interaction)
            let client = SshClient::from_server(&result.server, &result.server_id)?;
            let exit_code = client.execute_interactive(effective_command.as_deref());

            Ok((
                SshOutput::Connect(SshConnectOutput {
                    resolved_type: result.resolved_type,
                    project_id: result.project_id,
                    server_id: result.server_id,
                    command: args.command,
                }),
                exit_code,
            ))
        }
    }
}
