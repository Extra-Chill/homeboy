use clap::{Args, Subcommand};
use homeboy::core::engine::shell;
use homeboy::core::server::{self, Server};
use homeboy::core::server::{resolve_context, SshClient, SshResolveArgs};
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct SshArgs {
    /// Target ID (project or server; project wins when ambiguous)
    pub target: Option<String>,

    /// Command to execute (omit for interactive shell).
    ///
    /// Examples:
    ///   homeboy ssh my-project -- ls -la
    ///   homeboy ssh my-project -- wp plugin list
    ///
    /// If you need shell operators (&&, |, redirects), pass a single quoted string:
    ///   homeboy ssh my-project "cd /var/www && ls | head"
    #[arg(num_args = 0.., trailing_var_arg = true)]
    pub command: Vec<String>,

    /// Force interpretation as server ID
    #[arg(long)]
    pub as_server: bool,

    /// Override the SSH user (instead of the server's configured user)
    #[arg(long)]
    pub user: Option<String>,

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub success: bool,
    pub exit_code: i32,
    pub result_classification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
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

            let command_string: Option<String> = if args.command.is_empty() {
                None
            } else if args.command.len() == 1 {
                // Preserve legacy behavior: a single string is treated as a raw shell command.
                Some(args.command[0].clone())
            } else {
                // Multi-arg form (typically from `-- <cmd...>`): quote args safely.
                // Note: this intentionally does NOT support shell operators; pass a single string for that.
                Some(shell::quote_args(&args.command))
            };

            // When project is resolved with base_path, auto-cd to project root
            let effective_command = match (&result.project_id, &result.base_path, &command_string) {
                // Project with base_path and command: cd to base_path then run command
                (Some(_), Some(bp), Some(cmd)) => {
                    Some(format!("cd {} && {}", shell::quote_path(bp), cmd))
                }
                // Project with base_path, no command: interactive shell starts in base_path
                (Some(_), Some(bp), None) => Some(format!("cd {}", shell::quote_path(bp))),
                // No project context or no base_path: use command as-is
                _ => command_string.clone(),
            };

            let mut client = SshClient::from_server(&result.server, &result.server_id)?;
            if let Some(ref user_override) = args.user {
                client.user = user_override.clone();
            }

            if !args.command.is_empty() {
                // Non-interactive: capture output for JSON response
                let cmd = effective_command.as_deref().ok_or_else(|| {
                    homeboy::core::Error::internal_unexpected(
                        "No command resolved for non-interactive SSH execution".to_string(),
                    )
                })?;
                let output = client.execute(cmd);

                Ok((
                    SshOutput::Connect(SshConnectOutput {
                        resolved_type: result.resolved_type,
                        project_id: result.project_id,
                        server_id: result.server_id,
                        // Prefer the quoted/normalized command string for JSON output so
                        // multi-arg invocations remain unambiguous (e.g. args containing spaces).
                        command: command_string.clone(),
                        stdout: Some(output.stdout),
                        stderr: Some(output.stderr),
                        success: output.success,
                        exit_code: output.exit_code,
                        result_classification: ssh_result_classification(
                            output.success,
                            output.exit_code,
                        ),
                        failure_reason: ssh_failure_reason(output.success, output.exit_code),
                    }),
                    output.exit_code,
                ))
            } else {
                // Interactive: TTY passthrough
                let exit_code = client.execute_interactive(effective_command.as_deref());

                Ok((
                    SshOutput::Connect(SshConnectOutput {
                        resolved_type: result.resolved_type,
                        project_id: result.project_id,
                        server_id: result.server_id,
                        command: None,
                        stdout: None,
                        stderr: None,
                        success: exit_code == 0,
                        exit_code,
                        result_classification: ssh_result_classification(exit_code == 0, exit_code),
                        failure_reason: ssh_failure_reason(exit_code == 0, exit_code),
                    }),
                    exit_code,
                ))
            }
        }
    }
}

fn ssh_result_classification(success: bool, exit_code: i32) -> String {
    if success {
        return "remote_command_success".to_string();
    }

    if exit_code == 255 || exit_code < 0 {
        return "ssh_transport_failed".to_string();
    }

    "remote_command_failed".to_string()
}

fn ssh_failure_reason(success: bool, exit_code: i32) -> Option<String> {
    if success {
        return None;
    }

    if exit_code == 255 {
        return Some("SSH transport failed with exit code 255".to_string());
    }

    if exit_code < 0 {
        return Some("SSH process terminated without a remote exit code".to_string());
    }

    Some(format!(
        "Remote command exited with status {exit_code}; stdout/stderr may be empty for no-output commands"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_success_classification_does_not_depend_on_output() {
        assert_eq!(ssh_result_classification(true, 0), "remote_command_success");
        assert_eq!(ssh_failure_reason(true, 0), None);
    }

    #[test]
    fn ssh_failure_reason_handles_empty_output_failures() {
        assert_eq!(
            ssh_result_classification(false, 42),
            "remote_command_failed"
        );
        assert_eq!(
            ssh_failure_reason(false, 42).as_deref(),
            Some("Remote command exited with status 42; stdout/stderr may be empty for no-output commands")
        );
    }

    #[test]
    fn ssh_transport_failure_is_distinct_from_remote_command_failure() {
        assert_eq!(
            ssh_result_classification(false, 255),
            "ssh_transport_failed"
        );
        assert_eq!(
            ssh_failure_reason(false, 255).as_deref(),
            Some("SSH transport failed with exit code 255")
        );
    }
}
