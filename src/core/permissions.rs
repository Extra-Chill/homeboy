use std::process::Command;

use crate::defaults;
use crate::error::{Error, Result};
use crate::utils::shell;
use crate::ssh::{CommandOutput, SshClient};

/// Fix local file permissions before build.
///
/// Ensures files have group read/write so the zip archive contains correct permissions.
/// This addresses the issue where Claude Code sometimes creates files with 600 permissions.
pub fn fix_local_permissions(local_path: &str) {
    let quoted_path = shell::quote_path(local_path);
    let perms = defaults::load_defaults().permissions.local;

    eprintln!(
        "[build] Fixing local file permissions in {} (files: {}, dirs: {})",
        local_path, perms.file_mode, perms.dir_mode
    );

    // Fix files (configurable mode, default: g+rw)
    let file_cmd = format!(
        "find {} -type f -exec chmod {} {{}} + 2>/dev/null || true",
        quoted_path, perms.file_mode
    );
    Command::new("sh").args(["-c", &file_cmd]).output().ok();

    // Fix directories (configurable mode, default: g+rwx)
    let dir_cmd = format!(
        "find {} -type d -exec chmod {} {{}} + 2>/dev/null || true",
        quoted_path, perms.dir_mode
    );
    Command::new("sh").args(["-c", &dir_cmd]).output().ok();
}

/// Fix file permissions after deployment.
pub fn fix_deployed_permissions(ssh_client: &SshClient, remote_path: &str) -> Result<()> {
    let quoted_path = shell::quote_path(remote_path);
    let perms = defaults::load_defaults().permissions.remote;

    let dir_cmd = format!(
        "find {} -type d -exec chmod {} {{}} + 2>/dev/null",
        quoted_path, perms.dir_mode
    );
    let dir_output = ssh_client.execute(&dir_cmd);
    ensure_remote_success(dir_output, "chmod directories", remote_path)?;

    let file_cmd = format!(
        "find {} -type f -exec chmod {} {{}} + 2>/dev/null",
        quoted_path, perms.file_mode
    );
    let file_output = ssh_client.execute(&file_cmd);
    ensure_remote_success(file_output, "chmod files", remote_path)?;

    Ok(())
}

fn ensure_remote_success(output: CommandOutput, operation: &str, remote_path: &str) -> Result<()> {
    if output.success {
        return Ok(());
    }

    Err(Error::remote_command_failed(
        crate::error::RemoteCommandFailedDetails {
            command: format!("{} on {}", operation, remote_path),
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            target: crate::error::TargetDetails {
                project_id: None,
                server_id: None,
                host: None,
            },
        },
    ))
}
