use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::server::{self, Server, SshClient};

use super::super::Runner;

pub(super) fn validate_absolute_path(field: &str, path: &str) -> Result<()> {
    if path.starts_with('/') {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        field,
        format!("{field} must be an absolute path"),
        Some(path.to_string()),
        None,
    ))
}

pub(super) fn deterministic_remote_path(
    workspace_root: &str,
    local_path: &Path,
    snapshot: &str,
    run_isolation_token: Option<&str>,
) -> String {
    let name = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    let mut hasher = Sha256::new();
    hasher.update(local_path.display().to_string().as_bytes());
    hasher.update(snapshot.as_bytes());
    // Fold a per-run isolation token (when present) into the digest so two
    // distinct runs at the same source HEAD never resolve to the same remote
    // workspace directory. This prevents cross-run contamination where a later
    // run observes an earlier run's leftover untracked artifacts (#4393).
    if let Some(token) = run_isolation_token.filter(|token| !token.trim().is_empty()) {
        hasher.update(b"\0run-isolation\0");
        hasher.update(token.as_bytes());
    }
    let digest = hex_prefix(&hasher.finalize(), 12);
    format!(
        "{}/_lab_workspaces/{}-{}",
        workspace_root.trim_end_matches('/'),
        sanitize_path_segment(name),
        digest
    )
}

pub(crate) fn git_output(local_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(local_path)
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some("run git".to_string())))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(super) fn owner_capture_shell(reference: &str) -> String {
    format!(
        "owner_path={reference}; while [ ! -e \"$owner_path\" ] && [ \"$owner_path\" != \"/\" ]; do owner_path=$(dirname \"$owner_path\"); done; owner=\"\"; if [ -e \"$owner_path\" ]; then owner=$(stat -c '%u:%g' \"$owner_path\" 2>/dev/null || stat -f '%u:%g' \"$owner_path\" 2>/dev/null || true); fi",
    )
}

pub(super) fn owner_restore_shell(parent: &str, dest: &str) -> String {
    format!(
        "if [ \"$(id -u)\" = \"0\" ] && [ -n \"$owner\" ] && [ \"$owner\" != \"0:0\" ]; then chown \"$owner\" {parent} && chown -R \"$owner\" {dest}; fi",
    )
}

pub(crate) fn ssh_client_for_runner(runner: &Runner) -> Result<(Server, SshClient)> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runner requires server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env.clone());
    Ok((server, client))
}

/// Best-effort capture of a shell command's trimmed stdout. Returns `None` on
/// spawn failure or non-zero exit. Used for advisory provenance reads (e.g.
/// reading a synthetic snapshot checkout's HEAD over SSH) where a failure must
/// not abort the surrounding operation.
pub(crate) fn run_shell_capture(command: &str) -> Option<String> {
    let output = Command::new("sh").args(["-c", command]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn run_shell_command(command: &str, action: &str) -> Result<()> {
    let output = Command::new("sh")
        .args(["-c", command])
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some(action.to_string())))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = bounded_command_output(&output.stdout);
    let stderr = bounded_command_output(&output.stderr);
    // The command is typically a local `tar ... | ssh <runner> <extract>`
    // pipe, so an SSH transport that drops mid-materialization surfaces here
    // as a signal death (`sh` returns no exit code, reported as -1) or an
    // SSH connection error (exit 255 / transient connection stderr) rather
    // than a genuine remote command failure. Classify that as a retryable
    // transport failure with structured evidence so the caller can resume
    // from a deterministic phase instead of terminalizing an opaque
    // `invalid_input` with no diagnosable cause (#8803).
    if let Some(error) = classify_transport_failure(action, &output.status, &stdout, &stderr) {
        return Err(error);
    }
    let evidence = match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => "the command exited without stdout or stderr".to_string(),
        (false, true) => format!("stdout: {stdout}"),
        (true, false) => format!("stderr: {stderr}"),
        (false, false) => format!("stdout: {stdout}; stderr: {stderr}"),
    };
    Err(Error::internal_unexpected(format!(
        "{action} failed during command execution (exit status {}): {evidence}",
        output.status.code().unwrap_or(-1),
    )))
}

/// SSH connection failures worth retrying, matched against the piped command's
/// stderr. Kept in sync with `homeboy_core::server::is_transient_ssh_error`,
/// which operates on the runner `CommandOutput` type rather than the local
/// `sh` process output available here.
const TRANSIENT_SSH_STDERR_PATTERNS: [&str; 10] = [
    "connection refused",
    "connection reset",
    "connection timed out",
    "no route to host",
    "network is unreachable",
    "temporary failure in name resolution",
    "could not resolve hostname",
    "broken pipe",
    "ssh_exchange_identification",
    "connection closed by remote host",
];

/// Return a retryable [`ErrorCode::RunnerLabTransportFailure`] when a piped
/// materialization command failed because its SSH transport dropped, or `None`
/// when the failure is an ordinary non-transport command error.
fn classify_transport_failure(
    action: &str,
    status: &std::process::ExitStatus,
    stdout: &str,
    stderr: &str,
) -> Option<Error> {
    // `code() == None` means the process was killed by a signal (e.g. SIGPIPE
    // when the remote SSH end closed), reported to callers as exit status -1.
    let signal_death = status.code().is_none();
    // SSH itself exits 255 on a connection-level error, distinct from a remote
    // command's own non-zero exit code.
    let ssh_connection_exit = status.code() == Some(255);
    let lower_stderr = stderr.to_lowercase();
    let matched_transient = TRANSIENT_SSH_STDERR_PATTERNS
        .iter()
        .find(|pattern| lower_stderr.contains(**pattern))
        .copied();

    if !signal_death && !ssh_connection_exit && matched_transient.is_none() {
        return None;
    }

    let close_reason = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::to_string)
        .or_else(|| matched_transient.map(str::to_string))
        .unwrap_or_else(|| {
            if signal_death {
                "SSH transport closed the command without an exit code".to_string()
            } else {
                "SSH connection error (exit 255) without stderr".to_string()
            }
        });

    let exit_code = status.code().unwrap_or(-1);
    Some(
        Error::new(
            ErrorCode::RunnerLabTransportFailure,
            format!("{action} failed (exit status {exit_code}): {close_reason}"),
            serde_json::json!({
                "action": action,
                "exit_code": exit_code,
                "signal_death": signal_death,
                "transport_close_reason": close_reason,
                "stdout": stdout,
                "stderr": stderr,
            }),
        )
        .with_retryable(true),
    )
}

const COMMAND_FAILURE_OUTPUT_LIMIT: usize = 4 * 1024;

fn bounded_command_output(output: &[u8]) -> String {
    if output.len() <= COMMAND_FAILURE_OUTPUT_LIMIT {
        return String::from_utf8_lossy(output).trim().to_string();
    }

    let prefix = match std::str::from_utf8(output) {
        Ok(output) => {
            let end = output
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index <= COMMAND_FAILURE_OUTPUT_LIMIT)
                .last()
                .unwrap_or(0);
            output[..end].to_string()
        }
        Err(_) => String::from_utf8_lossy(&output[..COMMAND_FAILURE_OUTPUT_LIMIT]).into_owned(),
    };
    format!("{}... [truncated]", prefix.trim())
}

pub(crate) fn shell_command_for_runner(runner: &Runner, command: &str) -> Result<String> {
    match runner.kind {
        super::super::RunnerKind::Local => Ok(command.to_string()),
        super::super::RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                return Ok(command.to_string());
            }
            let remote = format!("{}@{}", client.user, client.host);
            Ok(format!(
                "ssh {ssh_args} {remote} {command}",
                ssh_args = ssh_args(&client),
                remote = shell::quote_arg(&remote),
                command = shell::quote_arg(command),
            ))
        }
    }
}

pub(super) fn tar_exclude_args(excludes: &[String]) -> String {
    excludes
        .iter()
        .map(|pattern| format!("--exclude {}", shell::quote_arg(pattern)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn ssh_args(client: &SshClient) -> String {
    homeboy_core::server::ssh_args::shell_join_args(
        &homeboy_core::server::ssh_args::client_option_args(
            client,
            homeboy_core::server::ssh_args::SshArgOptions {
                batch_mode: true,
                connect_timeout: true,
                keepalive: true,
                port_flag: Some(homeboy_core::server::ssh_args::SshPortFlag::Lowercase),
                ..homeboy_core::server::ssh_args::SshArgOptions::default()
            },
        ),
    )
}

pub(crate) fn parent_remote_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
        .to_string()
}

pub(crate) fn sanitize_path_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if segment.is_empty() {
        "workspace".to_string()
    } else {
        segment
    }
}

pub(super) fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(chars)
        .collect()
}
