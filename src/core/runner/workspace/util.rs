use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::server::{self, Server, SshClient};

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
    Err(Error::internal_unexpected(format!(
        "{action} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )))
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
    let mut args = vec![
        "-o BatchMode=yes".to_string(),
        "-o ConnectTimeout=10".to_string(),
        "-o ServerAliveInterval=15".to_string(),
        "-o ServerAliveCountMax=3".to_string(),
    ];
    if let Some(identity_file) = &client.identity_file {
        args.push(format!("-i {}", shell::quote_arg(identity_file)));
    }
    if let Some(session) = &client.auth {
        args.push("-o ControlMaster=auto".to_string());
        args.push(format!(
            "-o ControlPath={}",
            shell::quote_arg(&session.control_path)
        ));
        args.push(format!(
            "-o ControlPersist={}",
            shell::quote_arg(&session.persist)
        ));
    }
    if client.port != 22 {
        args.push(format!("-p {}", client.port));
    }
    args.join(" ")
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
