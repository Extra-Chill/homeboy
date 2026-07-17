use crate::defaults;
use crate::engine::shell;
use crate::error::{Error, Result};
use crate::server::{CommandOutput, SshClient};

/// Fix file permissions after deployment.
pub(crate) fn fix_deployed_permissions(
    ssh_client: &SshClient,
    remote_path: &str,
    remote_owner: Option<&str>,
) -> Result<()> {
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
/// Uses configured remote_owner if provided, otherwise auto-detects from the parent directory.
fn fix_deployed_ownership(
    ssh_client: &SshClient,
    remote_path: &str,
    remote_owner: Option<&str>,
    quoted_path: &str,
) {
    let owner = if let Some(configured) = remote_owner {
        configured.to_string()
    } else {
        // Auto-detect ownership from the PARENT directory, not the target itself.
        // After deployment, the target dir is owned by whoever ran the deploy (usually root).
        // The parent directory (e.g. wp-content/plugins/) retains the correct web server
        // ownership (e.g. www-data:www-data) and is the reliable source of truth.
        let parent_path = remote_path
            .trim_end_matches('/')
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or(remote_path);
        let quoted_parent = shell::quote_path(parent_path);
        let stat_cmd = format!(
            "stat -c '%U:%G' {} 2>/dev/null || stat -f '%Su:%Sg' {} 2>/dev/null",
            quoted_parent, quoted_parent
        );
        let stat_output = ssh_client.execute(&stat_cmd);
        if !stat_output.success || stat_output.stdout.trim().is_empty() {
            log_status!(
                "deploy",
                "Could not detect ownership of parent {}, skipping chown",
                parent_path
            );
            return;
        }
        let detected = stat_output.stdout.trim().to_string();
        // If the parent is root:root, there's nothing meaningful to inherit —
        // the web server ownership is unknown, so skip chown.
        if detected == "root:root" {
            log_status!(
                "deploy",
                "Parent directory {} is root:root — set remote_owner on the component to fix ownership",
                parent_path
            );
            return;
        }
        log_status!(
            "deploy",
            "Auto-detected ownership {} from parent {}",
            detected,
            parent_path
        );
        detected
    };

    log_status!(
        "deploy",
        "Setting ownership to {} on {}",
        owner,
        remote_path
    );
    let chown_cmd = format!(
        "chown -R {} {} 2>/dev/null",
        shell::quote_arg(&owner),
        quoted_path
    );
    let chown_output = ssh_client.execute(&chown_cmd);
    if !chown_output.success {
        log_status!(
            "deploy",
            "Warning: chown failed (exit {}): {}",
            chown_output.exit_code,
            chown_output.stderr
        );

        // An unprivileged deploy user cannot change the owner, but can often
        // apply the target parent's group. Preserve multi-writer access even
        // when the best-effort owner normalization is unavailable.
        if let Some((_, group)) = owner.rsplit_once(':') {
            let chgrp_cmd = format!(
                "chgrp -R {} {} 2>/dev/null",
                shell::quote_arg(group),
                quoted_path
            );
            let chgrp_output = ssh_client.execute(&chgrp_cmd);
            if !chgrp_output.success {
                log_status!(
                    "deploy",
                    "Warning: chgrp failed (exit {}): {}",
                    chgrp_output.exit_code,
                    chgrp_output.stderr
                );
            }
        }
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
