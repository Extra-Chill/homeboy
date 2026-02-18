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
pub fn fix_deployed_permissions(ssh_client: &SshClient, remote_path: &str, remote_owner: Option<&str>) -> Result<()> {
    let quoted_path = shell::quote_path(remote_path);

    // Step 1: Fix ownership (chown before chmod)
    fix_deployed_ownership(ssh_client, remote_path, remote_owner, &quoted_path);

    // Step 2: Fix permissions
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

/// Fix ownership of deployed files via chown.
/// Uses configured remote_owner if provided, otherwise auto-detects from existing ownership.
fn fix_deployed_ownership(ssh_client: &SshClient, remote_path: &str, remote_owner: Option<&str>, quoted_path: &str) {
    let owner = if let Some(configured) = remote_owner {
        configured.to_string()
    } else {
        // Auto-detect: stat the remote_path to get current owner:group
        let stat_cmd = format!("stat -c '%U:%G' {} 2>/dev/null", quoted_path);
        let stat_output = ssh_client.execute(&stat_cmd);
        if !stat_output.success || stat_output.stdout.trim().is_empty() {
            eprintln!("[deploy] Could not detect ownership of {}, skipping chown", remote_path);
            return;
        }
        let detected = stat_output.stdout.trim().to_string();
        // Skip chown if already root:root (no point changing to same)
        if detected == "root:root" {
            return;
        }
        detected
    };

    eprintln!("[deploy] Setting ownership to {} on {}", owner, remote_path);
    let chown_cmd = format!("chown -R {} {} 2>/dev/null", shell::quote_arg(&owner), quoted_path);
    let chown_output = ssh_client.execute(&chown_cmd);
    if !chown_output.success {
        eprintln!("[deploy] Warning: chown failed (exit {}): {}", chown_output.exit_code, chown_output.stderr);
        // Don't fail the deploy - chown is best-effort
    }
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
