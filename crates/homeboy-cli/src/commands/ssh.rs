use clap::{Args, Subcommand};
use homeboy::core::engine::shell;
use homeboy::core::server::{self, Server};
use homeboy::core::server::{resolve_context, CommandObservation, SshClient, SshResolveArgs};
use serde::Serialize;
use serde_json::Value;
use std::io::IsTerminal;
use std::time::Duration;

use super::CmdResult;

#[derive(Args)]
#[command(
    after_help = "Persisted artifact and file transfers use `homeboy file`:\n  homeboy file copy ./artifact.tar.gz prod:/var/tmp/artifact.tar.gz\n  homeboy file copy prod:/var/tmp/result.json ./result.json\n  homeboy file sync ./artifacts prod:/var/tmp/artifacts"
)]
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

    /// Remote working directory. Overrides the project's configured base path.
    #[arg(long, value_parser = non_empty_cwd)]
    pub cwd: Option<String>,

    /// Write only the remote command's stdout to local stdout (and its stderr to
    /// local stderr), exiting with the remote exit code. Ideal for piping a
    /// remote export straight into a file. Combine with `--output <path>` to also
    /// persist the structured envelope. For a persisted remote artifact rather
    /// than stdout, use `homeboy file copy` or `homeboy file sync`. Requires a
    /// non-interactive command.
    #[arg(long)]
    pub raw: bool,

    /// Bound the complete non-interactive SSH command, in seconds.
    /// Progress remains on stderr so `--raw` preserves remote stdout.
    #[arg(long)]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub subcommand: Option<SshSubcommand>,
}

/// Whether this invocation requested raw stdout mode for a non-interactive
/// remote command.
pub(super) fn is_raw_command(args: &SshArgs) -> bool {
    args.raw && args.subcommand.is_none() && !args.command.is_empty()
}

#[derive(Subcommand)]
pub enum SshSubcommand {
    /// List configured SSH server targets
    List {
        /// Return complete, redacted server configuration records.
        #[arg(long)]
        full: bool,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "action")]
pub enum SshOutput {
    Connect(SshConnectOutput),
    List(SshListOutput),
}

/// An internally tagged enum variant must serialize as an object. Keeping the
/// list payload in this wrapper also gives `--full` the same stable shape.
#[derive(Debug, Clone, Serialize)]
pub struct SshListOutput {
    #[serde(flatten)]
    payload: Value,
}

#[derive(Debug, Serialize)]
pub struct SshConnectOutput {
    pub resolved_type: String,
    pub project_id: Option<String>,
    pub server_id: String,
    pub requested_cwd: Option<String>,
    pub effective_cwd: Option<String>,
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub success: bool,
    pub exit_code: i32,
    pub result_classification: String,
    pub phases: Vec<String>,
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

/// Captured raw SSH result. Raw stdout is presented before its terminal phase
/// diagnostics so those diagnostics are never evidence of output the caller
/// has not yet received.
pub(super) struct RawSshExecution {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub result_classification: String,
    pub observation_lost: bool,
    pub observation: CommandObservation,
    pub timed_out: bool,
}

impl RawSshExecution {
    pub(crate) fn completion_phase_stderr(&self) -> String {
        let command_phase = ssh_terminal_phase_name(self.observation, self.observation_lost);
        format!("[ssh] phase={command_phase}\n[ssh] phase=cleanup-finished\n")
    }

    pub(crate) fn output_file_evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "stdout_tail": bounded_tail(&self.stdout),
            "stderr_tail": bounded_tail(&self.stderr),
            "stdout_seen_bytes": self.stdout.len(),
            "stderr_seen_bytes": self.stderr.len(),
            "stdout_truncated": self.stdout.len() > RAW_EVIDENCE_LIMIT_BYTES,
            "stderr_truncated": self.stderr.len() > RAW_EVIDENCE_LIMIT_BYTES,
            "exit_code": self.exit_code,
            "result_classification": self.result_classification,
            "observation_lost": self.observation_lost,
            "observation": self.observation.code(),
            "timed_out": self.timed_out,
        })
    }
}

const RAW_EVIDENCE_LIMIT_BYTES: usize = 64 * 1024;

fn bounded_tail(value: &str) -> String {
    if value.len() <= RAW_EVIDENCE_LIMIT_BYTES {
        return value.to_string();
    }
    let start = value
        .char_indices()
        .find_map(|(index, _)| (value.len() - index <= RAW_EVIDENCE_LIMIT_BYTES).then_some(index))
        .unwrap_or(value.len());
    value[start..].to_string()
}

pub fn run(args: SshArgs) -> CmdResult<SshOutput> {
    match args.subcommand {
        Some(SshSubcommand::List { full }) => {
            let servers = server::list()?;
            Ok((SshOutput::List(list_output(servers, full)), 0))
        }
        None => {
            ssh_phase("target-resolution-start");
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
            ssh_phase("target-resolved");

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

            let effective_command = effective_remote_command(
                effective_cwd(args.cwd.as_deref(), result.base_path.as_deref()),
                command_string.as_deref(),
            );

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
                let timeout = command_timeout(args.timeout)?;
                ssh_phase("command-start");
                let output =
                    execute_non_interactive(&client, cmd, timeout, std::io::stdin().is_terminal());
                ssh_terminal_phase(&output);
                ssh_phase("cleanup-finished");

                Ok((
                    SshOutput::Connect(connect_output_from_execution(
                        &result.resolved_type,
                        result.project_id.clone(),
                        &result.server_id,
                        args.cwd.clone(),
                        effective_cwd(args.cwd.as_deref(), result.base_path.as_deref())
                            .map(str::to_string),
                        command_string.clone(),
                        &output,
                    )),
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
                        requested_cwd: args.cwd.clone(),
                        effective_cwd: effective_cwd(
                            args.cwd.as_deref(),
                            result.base_path.as_deref(),
                        )
                        .map(str::to_string),
                        command: None,
                        stdout: None,
                        stderr: None,
                        success: exit_code == 0,
                        exit_code,
                        result_classification: ssh_result_classification(exit_code == 0, exit_code),
                        failure_reason: ssh_failure_reason(exit_code == 0, exit_code),
                        phases: vec!["target-resolved".to_string()],
                        timed_out: false,
                    }),
                    exit_code,
                ))
            }
        }
    }
}

const SSH_LIST_LIMIT: usize = 20;
const SSH_LIST_TEXT_LIMIT: usize = 160;
const SSH_LIST_PROJECTION_BYTES: usize = 8 * 1024;

fn list_output(servers: Vec<Server>, full: bool) -> SshListOutput {
    if full {
        return SshListOutput {
            payload: serde_json::json!({
                "schema": "homeboy/ssh-list/v1",
                "servers": homeboy::core::redaction::redact_json(
                    &serde_json::to_value(servers).unwrap_or(Value::Null),
                ),
            }),
        };
    }

    let total = servers.len();
    let shown = servers
        .iter()
        .take(SSH_LIST_LIMIT)
        .map(|server| {
            serde_json::json!({
                "id": bounded_text(&server.id, SSH_LIST_TEXT_LIMIT),
                "host": bounded_text(&server.host, SSH_LIST_TEXT_LIMIT),
                "user": bounded_text(&server.user, SSH_LIST_TEXT_LIMIT),
                "port": server.port,
                "kind": server.kind.as_deref().map(|kind| bounded_text(kind, SSH_LIST_TEXT_LIMIT)),
                "runner_configured": server.runner.is_some(),
            })
        })
        .collect::<Vec<_>>();
    let projection = serde_json::json!({
        "schema": "homeboy/ssh-list/v1",
        "operator_summary": {
            "identity": "ssh list",
            "state": if total == 0 { "empty" } else { "configured" },
            "risk": Vec::<String>::new(),
            "next_action": "homeboy ssh <server-id> -- <command>",
        },
        "servers": shown,
        "truncation": {
            "servers": {
                "shown": shown.len(),
                "omitted": total.saturating_sub(shown.len()),
                "evidence_ref": "ssh:server-config",
                "full_command": "homeboy ssh list --full",
            }
        }
    });
    SshListOutput {
        payload: bounded_list_envelope(projection),
    }
}

/// Validate the rendered command envelope rather than trusting per-field caps.
fn bounded_list_envelope(projection: Value) -> Value {
    let projection = homeboy::core::redaction::redact_json(&projection);
    let output = SshListOutput {
        payload: projection.clone(),
    };
    if list_envelope_bytes(&output).is_ok_and(|bytes| bytes <= SSH_LIST_PROJECTION_BYTES) {
        return projection;
    }
    serde_json::json!({
        "schema": "homeboy/ssh-list/v1",
        "operator_summary": {
            "identity": "ssh list",
            "state": "configured",
            "risk": ["configured target details exceed the default response budget"],
            "next_action": "homeboy ssh list --full",
        },
        "servers": [],
        "truncation": {
            "servers": {
                "shown": 0,
                "omitted": "see_full_output",
                "evidence_ref": "ssh:server-config",
                "full_command": "homeboy ssh list --full",
            }
        }
    })
}

fn list_envelope_bytes(output: &SshListOutput) -> serde_json::Result<usize> {
    let data = serde_json::to_value(SshOutput::List(output.clone()))?;
    let response = crate::commands::utils::response::cli_response_for_json_result_for_identity(
        &Ok(data),
        0,
        &crate::commands::utils::response::CommandIdentity::with_operation("ssh", "list"),
        None,
    );
    serde_json::to_vec(&response).map(|rendered| rendered.len())
}

fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .find_map(|(index, _)| (index >= limit).then_some(index))
        .unwrap_or(value.len());
    format!("{}...", &value[..end])
}

pub(crate) fn render_list_summary(payload: &Value) -> Option<String> {
    let summary = payload.get("operator_summary")?;
    let count = payload.get("servers")?.as_array()?.len();
    Some(format!(
        "SSH targets\nStatus: {}\nTargets shown: {count}\nNext: {}",
        summary.get("state")?.as_str()?,
        summary.get("next_action")?.as_str()?,
    ))
}

fn connect_output_from_execution(
    resolved_type: &str,
    project_id: Option<String>,
    server_id: &str,
    requested_cwd: Option<String>,
    effective_cwd: Option<String>,
    // Prefer the quoted/normalized command string for JSON output so multi-arg
    // invocations remain unambiguous (e.g. args containing spaces).
    command: Option<String>,
    output: &homeboy::core::server::CommandOutput,
) -> SshConnectOutput {
    SshConnectOutput {
        resolved_type: resolved_type.to_string(),
        project_id,
        server_id: server_id.to_string(),
        requested_cwd,
        effective_cwd,
        command,
        stdout: Some(output.stdout.clone()),
        stderr: Some(output.stderr.clone()),
        success: output.success,
        exit_code: output.exit_code,
        result_classification: ssh_result_classification_with_observation(
            output.success,
            output.exit_code,
            ssh_observation_lost(output),
        ),
        failure_reason: if ssh_observation_lost(output) {
            ssh_failure_reason_with_observation(output)
        } else {
            ssh_failure_reason(output.success, output.exit_code)
        },
        phases: ssh_execution_phases(output),
        timed_out: output.timed_out,
    }
}

/// Execute a non-interactive remote command and return its raw stdout/stderr and
/// exit code, for `--raw` mode. Resolution mirrors [`run`], but the caller emits
/// the remote streams directly instead of a JSON envelope.
pub(super) fn execute_raw_command(args: &SshArgs) -> homeboy::core::Result<RawSshExecution> {
    ssh_phase("target-resolution-start");
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
    ssh_phase("target-resolved");

    let command_string: Option<String> = if args.command.len() == 1 {
        Some(args.command[0].clone())
    } else {
        Some(shell::quote_args(&args.command))
    };
    let effective_command = effective_remote_command(
        effective_cwd(args.cwd.as_deref(), result.base_path.as_deref()),
        command_string.as_deref(),
    );

    let mut client = SshClient::from_server(&result.server, &result.server_id)?;
    if let Some(ref user_override) = args.user {
        client.user = user_override.clone();
    }
    let cmd = effective_command.as_deref().ok_or_else(|| {
        homeboy::core::Error::internal_unexpected(
            "No command resolved for non-interactive SSH execution".to_string(),
        )
    })?;
    let timeout = command_timeout(args.timeout)?;
    ssh_phase("command-start");
    let output = execute_non_interactive(&client, cmd, timeout, std::io::stdin().is_terminal());
    Ok(raw_execution_from_output(output))
}

fn raw_execution_from_output(output: homeboy::core::server::CommandOutput) -> RawSshExecution {
    let observation_lost = ssh_observation_lost(&output);
    RawSshExecution {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.exit_code,
        result_classification: ssh_result_classification_with_observation(
            output.success,
            output.exit_code,
            observation_lost,
        ),
        observation_lost,
        observation: output.observation,
        timed_out: output.timed_out,
    }
}

fn ssh_execution_phases(output: &homeboy::core::server::CommandOutput) -> Vec<String> {
    let command_phase = ssh_terminal_phase_name(output.observation, ssh_observation_lost(output));
    vec![
        "target-resolved".to_string(),
        "command-started".to_string(),
        command_phase.to_string(),
        "cleanup-finished".to_string(),
    ]
}

fn ssh_terminal_phase(output: &homeboy::core::server::CommandOutput) {
    ssh_phase(ssh_terminal_phase_name(
        output.observation,
        ssh_observation_lost(output),
    ));
}

fn ssh_terminal_phase_name(
    observation: CommandObservation,
    observation_lost: bool,
) -> &'static str {
    if observation == CommandObservation::SpawnFailed {
        "command-not-started"
    } else if observation_lost {
        "command-observation-lost"
    } else {
        "command-finished"
    }
}

fn ssh_observation_lost(output: &homeboy::core::server::CommandOutput) -> bool {
    matches!(
        output.observation,
        CommandObservation::StdinDeliveryFailed
            | CommandObservation::Cancelled
            | CommandObservation::StreamDrainTimedOut
            | CommandObservation::TransportObservationFailed
    )
}

fn execute_non_interactive(
    client: &SshClient,
    command: &str,
    timeout: Option<Duration>,
    stdin_is_terminal: bool,
) -> homeboy::core::server::CommandOutput {
    match (timeout, stdin_is_terminal) {
        (Some(timeout), true) => client.execute_with_timeout(command, timeout),
        (Some(timeout), false) => client.execute_with_piped_stdin_and_timeout(command, timeout),
        (None, false) => client.execute_with_piped_stdin(command),
        (None, true) => client.execute(command),
    }
}

fn command_timeout(seconds: Option<u64>) -> homeboy::core::Result<Option<Duration>> {
    match seconds {
        Some(0) => Err(homeboy::core::Error::validation_invalid_argument(
            "timeout",
            "SSH command timeout must be at least one second",
            None,
            None,
        )),
        Some(seconds) => Ok(Some(Duration::from_secs(seconds))),
        None => Ok(None),
    }
}

fn non_empty_cwd(value: &str) -> Result<String, String> {
    if value.is_empty() {
        Err("remote working directory must not be empty".to_string())
    } else {
        Ok(value.to_string())
    }
}

fn ssh_phase(phase: &str) {
    eprintln!("[ssh] phase={phase}");
}

fn ssh_result_classification(success: bool, exit_code: i32) -> String {
    ssh_result_classification_with_observation(success, exit_code, false)
}

fn ssh_result_classification_with_observation(
    success: bool,
    exit_code: i32,
    observation_lost: bool,
) -> String {
    if observation_lost {
        return "remote_state_indeterminate".to_string();
    }
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

fn ssh_failure_reason_with_observation(
    output: &homeboy::core::server::CommandOutput,
) -> Option<String> {
    if ssh_observation_lost(output) {
        return Some(
            "SSH observation was interrupted; remote execution may have continued after the client lost its terminal result."
                .to_string(),
        );
    }
    ssh_failure_reason(output.success, output.exit_code)
}

/// Use an explicitly requested cwd when present; otherwise preserve the
/// project-target base path default.
fn effective_cwd<'a>(
    requested_cwd: Option<&'a str>,
    base_path: Option<&'a str>,
) -> Option<&'a str> {
    requested_cwd.or(base_path)
}

/// Compose the remote command, rooting it at its effective cwd.
///
/// Extracted from `run` so the generated command is directly assertable
/// (#10839). The interactive shape in particular could not be observed from a
/// test while it was an inline `match` arm, which is how `cd <base_path>` --
/// a command that exits immediately -- shipped as the "open a shell" case.
fn effective_remote_command(cwd: Option<&str>, command: Option<&str>) -> Option<String> {
    match (cwd, command) {
        // A requested cwd or project base path with a command: cd then run it.
        (Some(cwd), Some(cmd)) => Some(format!("cd {} && {}", shell::quote_path(cwd), cmd)),
        // A requested cwd or project base path without a command: start an
        // interactive shell rooted at that directory.
        //
        // `cd <bp>` alone is a complete remote command, so ssh ran it, it
        // succeeded, and the session ended -- reporting
        // `remote_command_success` with no shell ever presented. Replace the
        // process with the operator's shell so the session persists until they
        // exit it.
        //
        // `exec` rather than a plain invocation so the shell's exit status is
        // the session's. Not `-l`: a login shell re-runs the profile scripts,
        // and one that cd's to $HOME would silently undo the base_path this
        // command exists to apply. The pty requested for the interactive path
        // is what makes the exec'd shell interactive.
        (Some(cwd), None) => Some(format!(
            "cd {} && exec \"${{SHELL:-/bin/sh}}\"",
            shell::quote_path(cwd)
        )),
        // No cwd: use the command as-is. A
        // server-only session with no command stays None, so ssh opens its own
        // login shell exactly as before.
        (None, command) => command.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;
    use std::collections::HashMap;

    #[test]
    fn list_projection_bounds_items_bytes_and_redacts_full_records() {
        let servers = (0..(SSH_LIST_LIMIT + 5))
            .map(|index| Server {
                id: format!("server-{index}"),
                aliases: Vec::new(),
                host: "h".repeat(4_000),
                user: "user".to_string(),
                port: 22,
                identity_file: None,
                kind: None,
                auth: None,
                env: HashMap::from([("API_TOKEN".to_string(), "secret-value".to_string())]),
                runner: None,
            })
            .collect::<Vec<_>>();
        let compact = list_output(servers.clone(), false).payload;
        let rendered = serde_json::to_string(&compact).expect("compact JSON");

        assert_eq!(
            compact["servers"].as_array().expect("servers").len(),
            SSH_LIST_LIMIT
        );
        assert_eq!(compact["truncation"]["servers"]["omitted"], 5);
        assert!(rendered.len() < 8 * 1024, "{rendered}");
        assert_eq!(
            render_list_summary(&compact).as_deref(),
            Some("SSH targets\nStatus: configured\nTargets shown: 20\nNext: homeboy ssh <server-id> -- <command>")
        );

        let full = serde_json::to_string(&SshOutput::List(list_output(servers, true)))
            .expect("full list serializes");
        assert!(!full.contains("secret-value"), "{full}");
    }

    #[test]
    fn list_projection_hard_bounds_oversized_ids_and_nested_runner_identity() {
        let servers = vec![Server {
            id: "id-".repeat(10_000),
            aliases: vec!["alias-".repeat(10_000)],
            host: "host-".repeat(10_000),
            user: "user-".repeat(10_000),
            port: 22,
            identity_file: Some("identity-".repeat(10_000)),
            kind: Some("kind-".repeat(10_000)),
            auth: None,
            env: HashMap::new(),
            runner: Some(homeboy::core::server::ServerRunner {
                workspace_root: Some("workspace-".repeat(10_000)),
                ..Default::default()
            }),
        }];
        let compact = list_output(servers.clone(), false);
        assert!(list_envelope_bytes(&compact).unwrap() <= SSH_LIST_PROJECTION_BYTES);
        let full = serde_json::to_string(&SshOutput::List(list_output(servers, true)))
            .expect("full list serializes");
        let round_trip: Value = serde_json::from_str(&full).expect("full list round trips");
        assert_eq!(round_trip["action"], "List");
        assert!(round_trip["servers"].is_array());
    }

    #[test]
    fn list_projection_fuzzes_untrusted_text_without_exceeding_its_envelope_budget() {
        for value in [
            "x".repeat(10_000),
            "\u{00e9}".repeat(10_000),
            "\"\\\n".repeat(10_000),
        ] {
            let server = Server {
                id: value.clone(),
                aliases: vec![value.clone()],
                host: value.clone(),
                user: value.clone(),
                port: 22,
                identity_file: Some(value.clone()),
                kind: Some(value),
                auth: None,
                env: HashMap::new(),
                runner: None,
            };
            let compact = list_output(vec![server], false);
            assert!(list_envelope_bytes(&compact).unwrap() <= SSH_LIST_PROJECTION_BYTES);
        }
    }

    #[test]
    fn list_projection_bounds_the_actual_tagged_command_result_wire_output() {
        let output = list_output(
            vec![Server {
                id: "id-".repeat(10_000),
                aliases: Vec::new(),
                host: "host-".repeat(10_000),
                user: "user-".repeat(10_000),
                port: 22,
                identity_file: None,
                kind: Some("kind-".repeat(10_000)),
                auth: None,
                env: HashMap::new(),
                runner: None,
            }],
            false,
        );
        let data = serde_json::to_value(SshOutput::List(output.clone())).expect("tagged data");
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_identity(
            &Ok(data),
            0,
            &crate::commands::utils::response::CommandIdentity::with_operation("ssh", "list"),
            None,
        );
        let wire = serde_json::to_vec(&envelope).expect("wire serializes");

        assert!(wire.len() <= SSH_LIST_PROJECTION_BYTES);
        let wire: Value = serde_json::from_slice(&wire).expect("wire round trips");
        assert_eq!(wire["operation"], "list");
        assert_eq!(wire["data"]["action"], "List");
    }

    #[test]
    fn list_projection_falls_back_at_the_actual_wire_budget_boundary() {
        let escaped = "\"\\\n".repeat(10_000);
        let servers = (0..SSH_LIST_LIMIT)
            .map(|index| Server {
                id: format!("{index}-{escaped}"),
                aliases: Vec::new(),
                host: escaped.clone(),
                user: escaped.clone(),
                port: 22,
                identity_file: None,
                kind: Some(escaped.clone()),
                auth: None,
                env: HashMap::new(),
                runner: None,
            })
            .collect();
        let output = list_output(servers, false);

        assert!(list_envelope_bytes(&output).unwrap() <= SSH_LIST_PROJECTION_BYTES);
        assert!(output.payload["servers"].as_array().unwrap().is_empty());
        assert_eq!(
            output.payload["truncation"]["servers"]["omitted"],
            "see_full_output"
        );
    }

    /// A cwd-rooted interactive session must hand back a shell, not run `cd` and quit.
    #[test]
    fn interactive_ssh_execs_a_shell_after_changing_directory() {
        let command = effective_remote_command(Some("/srv/app"), None)
            .expect("a project with a base path must produce a remote command");

        assert_eq!(command, "cd '/srv/app' && exec \"${SHELL:-/bin/sh}\"");
        assert!(
            command.contains("exec"),
            "the session must replace itself with a shell rather than terminating \
             after cd (#10839): {command}"
        );
        assert!(
            !command.contains(" -l"),
            "a login shell re-runs profile scripts that may cd away from the \
             base path this command exists to apply: {command}"
        );
    }

    /// Project base paths remain the default, while explicit cwd applies to every target type.
    #[test]
    fn explicit_cwd_overrides_project_base_path_and_roots_server_commands() {
        assert_eq!(
            effective_cwd(Some("/srv/override"), Some("/srv/project")),
            Some("/srv/override")
        );
        assert_eq!(
            effective_cwd(None, Some("/srv/project")),
            Some("/srv/project")
        );
        assert_eq!(
            effective_cwd(Some("/srv/server"), None),
            Some("/srv/server")
        );
        assert_eq!(
            effective_remote_command(Some("/srv/app"), Some("ls -la")).as_deref(),
            Some("cd '/srv/app' && ls -la")
        );
        let cwd = "/srv/dir with spaces;$(nope)'";
        let args = vec![
            "printf".to_string(),
            "%s".to_string(),
            "safe; argument".to_string(),
        ];
        assert_eq!(
            effective_remote_command(Some(cwd), Some(&shell::quote_args(&args))).as_deref(),
            Some("cd '/srv/dir with spaces;$(nope)'\\''' && printf %s 'safe; argument'")
        );
    }

    /// Server-only SSH remains unchanged when neither a cwd nor project base path exists.
    #[test]
    fn cwdless_remote_command_shapes_are_unchanged() {
        // Server-only, no base path: the command passes through verbatim.
        assert_eq!(
            effective_remote_command(None, Some("uptime")).as_deref(),
            Some("uptime")
        );
        // Server-only interactive: ssh opens its own login shell, as before.
        assert_eq!(effective_remote_command(None, None), None);
        // A project without a base path has nothing to cd to.
        assert_eq!(
            effective_remote_command(None, Some("uptime")).as_deref(),
            Some("uptime")
        );
        assert_eq!(effective_remote_command(None, None), None);
    }

    #[test]
    fn cwd_cli_validation_rejects_empty_values() {
        assert_eq!(non_empty_cwd("/srv/app").as_deref(), Ok("/srv/app"));
        assert_eq!(
            non_empty_cwd(""),
            Err("remote working directory must not be empty".to_string())
        );
    }

    #[test]
    fn cwd_parses_before_a_safe_multi_argument_command_and_rejects_empty_input() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "ssh",
            "sandbox",
            "--cwd",
            "/srv/dir with spaces;$(nope)'",
            "--",
            "printf",
            "%s",
            "safe; argument",
        ])
        .expect("cwd and a multi-argument command should parse");
        let Commands::Ssh(args) = cli.command else {
            panic!("expected ssh command");
        };
        assert_eq!(args.cwd.as_deref(), Some("/srv/dir with spaces;$(nope)'"));
        assert_eq!(args.command, ["printf", "%s", "safe; argument"]);
        assert!(Cli::try_parse_from(["homeboy", "ssh", "sandbox", "--cwd", ""]).is_err());
    }

    #[test]
    fn structured_output_records_requested_and_effective_cwd() {
        let output = homeboy::core::server::CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
            exit_code: 0,
            timed_out: false,
            observation: Default::default(),
            child_resource: None,
        };
        let connect = connect_output_from_execution(
            "project",
            Some("project".to_string()),
            "server",
            Some("/srv/override".to_string()),
            Some("/srv/override".to_string()),
            Some("pwd".to_string()),
            &output,
        );

        assert_eq!(connect.requested_cwd.as_deref(), Some("/srv/override"));
        assert_eq!(connect.effective_cwd.as_deref(), Some("/srv/override"));
    }

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

    fn command_output(
        stdout: &str,
        stderr: &str,
        success: bool,
        exit_code: i32,
        timed_out: bool,
        observation: CommandObservation,
    ) -> homeboy::core::server::CommandOutput {
        homeboy::core::server::CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            success,
            exit_code,
            timed_out,
            observation,
            child_resource: None,
        }
    }

    #[test]
    fn raw_execution_preserves_delayed_and_last_moment_output_before_completion() {
        let client = localhost_client();

        let delayed = execute_non_interactive(
            &client,
            "sleep 0.02; printf delayed-output",
            Some(Duration::from_secs(1)),
            true,
        );
        let final_output = execute_non_interactive(
            &client,
            "printf output-immediately-before-exit",
            Some(Duration::from_secs(1)),
            true,
        );
        let delayed = raw_execution_from_output(delayed);
        let final_output = raw_execution_from_output(final_output);

        assert_eq!(delayed.stdout, "delayed-output");
        assert_eq!(final_output.stdout, "output-immediately-before-exit");
        for execution in [delayed, final_output] {
            assert!(!execution.observation_lost);
            assert_eq!(execution.exit_code, 0);
            assert!(execution
                .completion_phase_stderr()
                .contains("command-finished"));
            assert!(execution
                .completion_phase_stderr()
                .contains("cleanup-finished"));
        }
    }

    #[test]
    fn raw_execution_reports_indeterminate_observation_without_command_finished() {
        let timeout = raw_execution_from_output(command_output(
            "partial",
            "drain timed out",
            false,
            124,
            true,
            CommandObservation::StreamDrainTimedOut,
        ));
        let interrupted = raw_execution_from_output(command_output(
            "partial",
            "remote stderr remains opaque to terminal state classification",
            false,
            130,
            false,
            CommandObservation::Cancelled,
        ));
        let transport = raw_execution_from_output(command_output(
            "",
            "connection lost",
            false,
            255,
            false,
            CommandObservation::TransportObservationFailed,
        ));
        assert_eq!(transport.stderr, "connection lost");

        for execution in [timeout, interrupted, transport] {
            assert!(execution.observation_lost);
            assert_eq!(
                execution.result_classification,
                "remote_state_indeterminate"
            );
            assert!(execution
                .completion_phase_stderr()
                .contains("command-observation-lost"));
            assert!(!execution
                .completion_phase_stderr()
                .contains("command-finished"));
        }
    }

    #[test]
    fn structured_phases_do_not_report_command_finished_after_lost_observation() {
        let output = command_output(
            "",
            "transport observation failed",
            false,
            -1,
            false,
            CommandObservation::TransportObservationFailed,
        );
        let phases = ssh_execution_phases(&output);

        assert!(phases.contains(&"command-observation-lost".to_string()));
        assert!(!phases.contains(&"command-finished".to_string()));
        assert_eq!(phases.last().map(String::as_str), Some("cleanup-finished"));
    }

    #[test]
    fn spawn_failure_never_reports_a_terminal_command_phase() {
        let output = command_output(
            "",
            "ssh unavailable",
            false,
            -1,
            false,
            CommandObservation::SpawnFailed,
        );
        let phases = ssh_execution_phases(&output);
        let raw = raw_execution_from_output(output);

        assert!(!raw.observation_lost);
        assert!(phases.contains(&"command-not-started".to_string()));
        assert!(!phases.contains(&"command-finished".to_string()));
        assert!(raw
            .completion_phase_stderr()
            .contains("command-not-started"));
        assert!(!raw.completion_phase_stderr().contains("command-finished"));
    }

    #[test]
    fn structured_result_classification_matches_lost_observation_phase() {
        let output = command_output(
            "partial",
            "stream drain timed out",
            false,
            124,
            true,
            CommandObservation::StreamDrainTimedOut,
        );
        let connect = connect_output_from_execution(
            "server",
            None,
            "local",
            None,
            None,
            Some("printf partial".to_string()),
            &output,
        );

        assert_eq!(connect.result_classification, "remote_state_indeterminate");
        assert!(connect
            .failure_reason
            .as_deref()
            .expect("typed failure reason")
            .contains("may have continued"));
        assert!(connect
            .phases
            .contains(&"command-observation-lost".to_string()));
        assert!(!connect.phases.contains(&"command-finished".to_string()));
    }

    #[test]
    fn piped_stdin_delivery_failure_is_typed_indeterminate_state() {
        let execution = raw_execution_from_output(command_output(
            "partial stdout",
            "unrelated remote stderr",
            false,
            1,
            false,
            CommandObservation::StdinDeliveryFailed,
        ));

        assert!(execution.observation_lost);
        assert_eq!(
            execution.result_classification,
            "remote_state_indeterminate"
        );
        assert!(execution
            .completion_phase_stderr()
            .contains("command-observation-lost"));
    }

    #[test]
    fn raw_output_file_evidence_is_bounded_without_losing_transport_state() {
        let execution = RawSshExecution {
            stdout: "x".repeat(RAW_EVIDENCE_LIMIT_BYTES + 1),
            stderr: "y".repeat(RAW_EVIDENCE_LIMIT_BYTES + 1),
            exit_code: 0,
            result_classification: "remote_command_success".to_string(),
            observation_lost: false,
            observation: CommandObservation::Complete,
            timed_out: false,
        };

        let evidence = execution.output_file_evidence();
        assert_eq!(evidence["stdout_seen_bytes"], RAW_EVIDENCE_LIMIT_BYTES + 1);
        assert_eq!(evidence["stderr_seen_bytes"], RAW_EVIDENCE_LIMIT_BYTES + 1);
        assert_eq!(
            evidence["stdout_tail"].as_str().unwrap().len(),
            RAW_EVIDENCE_LIMIT_BYTES
        );
        assert_eq!(
            evidence["stderr_tail"].as_str().unwrap().len(),
            RAW_EVIDENCE_LIMIT_BYTES
        );
        assert_eq!(evidence["stdout_truncated"], true);
        assert_eq!(evidence["exit_code"], 0);
        assert_eq!(evidence["observation"], "complete");
        assert_eq!(evidence["timed_out"], false);
    }

    #[test]
    fn raw_output_file_evidence_preserves_typed_terminal_observation() {
        for (observation, timed_out, expected) in [
            (
                CommandObservation::StreamDrainTimedOut,
                true,
                "stream_drain_timed_out",
            ),
            (CommandObservation::Cancelled, false, "cancelled"),
            (
                CommandObservation::StdinDeliveryFailed,
                false,
                "stdin_delivery_failed",
            ),
            (
                CommandObservation::TransportObservationFailed,
                false,
                "transport_observation_failed",
            ),
        ] {
            let evidence = raw_execution_from_output(command_output(
                "",
                "",
                false,
                -1,
                timed_out,
                observation,
            ))
            .output_file_evidence();

            assert_eq!(evidence["observation"], expected);
            assert_eq!(evidence["timed_out"], timed_out);
        }
    }

    fn raw_args(command: Vec<&str>, raw: bool) -> SshArgs {
        SshArgs {
            target: Some("wp-build-runtime".to_string()),
            command: command.into_iter().map(str::to_string).collect(),
            as_server: false,
            user: None,
            cwd: None,
            raw,
            timeout: None,
            subcommand: None,
        }
    }

    #[test]
    fn is_raw_command_requires_raw_flag_and_a_command() {
        // Raw mode applies only to a non-interactive command with --raw.
        assert!(is_raw_command(&raw_args(vec!["printf", "hi"], true)));
        // Without --raw it is the normal JSON-envelope path.
        assert!(!is_raw_command(&raw_args(vec!["printf", "hi"], false)));
        // --raw with no command (interactive) is not a raw stdout invocation.
        assert!(!is_raw_command(&raw_args(vec![], true)));
    }

    #[test]
    fn command_timeout_requires_a_positive_deadline() {
        assert_eq!(
            command_timeout(Some(2)).unwrap(),
            Some(Duration::from_secs(2))
        );
        assert!(command_timeout(Some(0)).is_err());
    }

    fn localhost_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: std::collections::HashMap::new(),
        }
    }

    /// The non-interactive SSH dispatcher must select the timed piped-stdin path
    /// when `--timeout` is present and controller stdin is not a terminal, so
    /// the fix for timed SSH stdin forwarding is actually exercised from the CLI.
    #[test]
    fn non_interactive_dispatch_selects_timed_piped_stdin_for_non_terminal_timeout() {
        let client = localhost_client();

        let output = execute_non_interactive(
            &client,
            "printf timed-piped",
            Some(Duration::from_secs(5)),
            false,
        );

        assert!(output.success, "{}", output.stderr);
        assert_eq!(output.stdout, "timed-piped");
    }

    /// Each of the four dispatch quadrants must route to the expected execution
    /// path. These use local execution so the test is deterministic and does not
    /// require an SSH server.
    #[test]
    fn non_interactive_dispatch_routes_all_stdin_timeout_combinations() {
        let client = localhost_client();

        let timed_terminal =
            execute_non_interactive(&client, "printf tt", Some(Duration::from_secs(5)), true);
        assert!(timed_terminal.success);
        assert_eq!(timed_terminal.stdout, "tt");

        let timed_piped =
            execute_non_interactive(&client, "printf tp", Some(Duration::from_secs(5)), false);
        assert!(timed_piped.success);
        assert_eq!(timed_piped.stdout, "tp");

        let untimed_piped = execute_non_interactive(&client, "printf up", None, false);
        assert!(untimed_piped.success);
        assert_eq!(untimed_piped.stdout, "up");

        let untimed_terminal = execute_non_interactive(&client, "printf ut", None, true);
        assert!(untimed_terminal.success);
        assert_eq!(untimed_terminal.stdout, "ut");
    }
}
