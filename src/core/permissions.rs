use std::process::Command;

use crate::shell;
use crate::ssh::SshClient;

/// Fix local file permissions before build.
///
/// Ensures files have group read/write so the zip archive contains correct permissions.
/// This addresses the issue where Claude Code sometimes creates files with 600 permissions.
pub fn fix_local_permissions(local_path: &str) {
    eprintln!("[build] Fixing local file permissions");

    let quoted_path = shell::quote_path(local_path);

    // Fix files (rw for group)
    let file_cmd = format!(
        "find {} -type f -exec chmod g+rw {{}} + 2>/dev/null || true",
        quoted_path
    );
    Command::new("sh").args(["-c", &file_cmd]).output().ok();

    // Fix directories (rwx for group)
    let dir_cmd = format!(
        "find {} -type d -exec chmod g+rwx {{}} + 2>/dev/null || true",
        quoted_path
    );
    Command::new("sh").args(["-c", &dir_cmd]).output().ok();
}

/// Fix file permissions after deployment.
///
/// Attempts chmod on both directories and files, gracefully handling failures.
/// This ensures deployments work across different hosting environments:
/// - Some hosts (like Cloudways) don't allow directory permission changes
/// - Files should always be changeable
///
/// The `2>/dev/null || true` pattern ensures the command never fails,
/// even if some files/directories can't be modified.
pub fn fix_deployed_permissions(ssh_client: &SshClient, remote_path: &str) {
    let quoted_path = shell::quote_path(remote_path);

    // Try directories first (may fail on some hosts like Cloudways)
    let dir_cmd = format!(
        "find {} -type d -exec chmod g+w {{}} + 2>/dev/null || true",
        quoted_path
    );
    ssh_client.execute(&dir_cmd);

    // Then files (should always work)
    let file_cmd = format!(
        "find {} -type f -exec chmod g+w {{}} + 2>/dev/null || true",
        quoted_path
    );
    ssh_client.execute(&file_cmd);
}
