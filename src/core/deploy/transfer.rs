use std::path::Path;
use std::process::{Command, Output};

use crate::core::defaults;
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::server::SshClient;

use super::types::DeployResult;

pub(super) fn upload_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    rsync_directory(ssh_client, local_path, remote_path)
}

/// Sync a local directory to the remote using rsync with --delete.
///
/// This ensures the remote directory mirrors the source exactly:
/// files removed or moved in the source are removed from the target.
/// Without --delete, stale files accumulate on the server and can
/// shadow new files (e.g. when PHP autoloader loads an old copy).
fn rsync_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Ensure local_path ends with / so rsync copies contents, not the directory itself
    let local_str = format!(
        "{}/",
        local_path.display().to_string().trim_end_matches('/')
    );

    // Ensure remote_path ends with /
    let remote_str = format!("{}/", remote_path.trim_end_matches('/'));

    if ssh_client.is_local {
        // Local deploy: rsync locally without SSH
        log_status!(
            "deploy",
            "Syncing directory (local rsync): {} -> {}",
            local_str,
            remote_str
        );

        let rsync_args = vec![
            "-a".to_string(), // archive mode (recursive, preserves permissions, timestamps, etc.)
            "--delete".to_string(), // remove files on target that don't exist in source
            local_str,
            remote_str,
        ];

        let output = Command::new("rsync").args(&rsync_args).output();
        return match output {
            Ok(output) => Ok(process_output_result(output)),
            Err(err) => Ok(DeployResult::failure(1, format!("rsync failed: {}", err))),
        };
    }

    // Remote deploy: rsync over SSH
    let mut rsync_args = vec!["-a".to_string(), "--delete".to_string()];

    // Build SSH command with the same options as scp
    let mut ssh_cmd_parts = vec!["ssh".to_string()];
    if let Some(identity_file) = &ssh_client.identity_file {
        ssh_cmd_parts.extend(["-i".to_string(), identity_file.clone()]);
    }
    if ssh_client.port != 22 {
        ssh_cmd_parts.extend(["-p".to_string(), ssh_client.port.to_string()]);
    }
    // Use same safety options as SSH client
    ssh_cmd_parts.extend([
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ]);

    rsync_args.extend(["-e".to_string(), ssh_cmd_parts.join(" ")]);
    rsync_args.push(local_str.clone());
    rsync_args.push(format!(
        "{}@{}:{}",
        ssh_client.user, ssh_client.host, remote_str
    ));

    log_status!(
        "deploy",
        "Syncing directory: {} -> {}@{}:{}",
        local_str,
        ssh_client.user,
        ssh_client.host,
        remote_str
    );

    let output = Command::new("rsync").args(&rsync_args).output();
    match output {
        Ok(output) => Ok(process_output_result(output)),
        Err(err) => Ok(DeployResult::failure(1, format!("rsync failed: {}", err))),
    }
}

pub(super) fn upload_file(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Upload to a temporary file in the same directory and atomically replace the destination.
    // This avoids failures like: `scp: ...: Text file busy` when updating an in-use binary.
    scp_file_atomic(ssh_client, local_path, remote_path)
}

/// Core SCP transfer function.
fn scp_transfer(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    recursive: bool,
) -> Result<DeployResult> {
    let label = if recursive { "directory" } else { "file" };

    // Local deploy: use cp instead of scp
    if ssh_client.is_local {
        log_status!(
            "deploy",
            "Copying {} (local): {} -> {}",
            label,
            local_path.display(),
            remote_path
        );

        let mut cp_args = vec!["-f".to_string()];
        if recursive {
            cp_args.push("-r".to_string());
        }
        // Preserve permissions and timestamps
        cp_args.push("-p".to_string());
        cp_args.push(local_path.to_string_lossy().to_string());
        cp_args.push(remote_path.to_string());

        let output = Command::new("cp").args(&cp_args).output();
        return match output {
            Ok(output) => Ok(process_output_result(output)),
            Err(err) => Ok(DeployResult::failure(1, err.to_string())),
        };
    }

    let deploy_defaults = defaults::load_defaults().deploy;
    let mut scp_args: Vec<String> = deploy_defaults.scp_flags.clone();

    if recursive {
        scp_args.push("-r".to_string());
    }

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.extend(["-i".to_string(), identity_file.clone()]);
    }

    if ssh_client.port != deploy_defaults.default_ssh_port {
        scp_args.extend(["-P".to_string(), ssh_client.port.to_string()]);
    }

    scp_args.push(local_path.to_string_lossy().to_string());
    scp_args.push(format!(
        "{}@{}:{}",
        ssh_client.user,
        ssh_client.host,
        shell::quote_path(remote_path)
    ));

    log_status!(
        "deploy",
        "Uploading {}: {} -> {}@{}:{}",
        label,
        local_path.display(),
        ssh_client.user,
        ssh_client.host,
        remote_path
    );

    let output = Command::new("scp").args(&scp_args).output();
    match output {
        Ok(output) => Ok(process_output_result(output)),
        Err(err) => Ok(DeployResult::failure(1, err.to_string())),
    }
}

fn process_output_result(output: Output) -> DeployResult {
    if output.status.success() {
        return DeployResult::success(0);
    }

    DeployResult::failure(
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

pub(super) fn scp_file(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    scp_transfer(ssh_client, local_path, remote_path, false)
}

fn scp_file_atomic(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    let remote = Path::new(remote_path);
    let remote_dir = remote.parent().and_then(|p| p.to_str()).unwrap_or(".");
    let remote_filename = remote.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "remotePath",
            "Remote path must include a file name",
            Some(remote_path.to_string()),
            None,
        )
    })?;

    let tmp_path = format!(
        "{}/.homeboy-upload-{}.tmp.{}",
        remote_dir,
        remote_filename,
        std::process::id()
    );

    let upload_result = scp_transfer(ssh_client, local_path, &tmp_path, false)?;
    if !upload_result.success {
        return Ok(upload_result);
    }

    // Atomic replace: mv temp -> destination (same directory)
    let mv_cmd = format!(
        "mv -f {} {}",
        shell::quote_path(&tmp_path),
        shell::quote_path(remote_path)
    );
    let mv_output = ssh_client.execute(&mv_cmd);

    if !mv_output.success {
        let error_detail = if mv_output.stderr.is_empty() {
            mv_output.stdout
        } else {
            mv_output.stderr
        };
        return Ok(DeployResult::failure(
            mv_output.exit_code,
            format!("Failed to move uploaded file into place: {}", error_detail),
        ));
    }

    Ok(DeployResult::success(0))
}

#[cfg(test)]
mod tests {
    use super::{process_output_result, scp_file, upload_directory, upload_file};
    use crate::core::server::SshClient;
    use std::collections::HashMap;
    use std::fs;
    use std::process::Command;

    fn local_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "test".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        }
    }

    #[test]
    fn test_upload_directory() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(&source).expect("create source dir");
        fs::create_dir_all(&target).expect("create target dir");
        fs::write(source.join("file.txt"), "hello").expect("write source file");

        let result = upload_directory(&local_client(), &source, target.to_str().unwrap())
            .expect("upload directory");

        assert!(result.success);
        assert_eq!(
            fs::read_to_string(target.join("file.txt")).expect("read copied file"),
            "hello"
        );
    }

    #[test]
    fn test_upload_file() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let source = temp.path().join("source.txt");
        let target = temp.path().join("target.txt");
        fs::write(&source, "hello").expect("write source file");

        let result =
            upload_file(&local_client(), &source, target.to_str().unwrap()).expect("upload file");

        assert!(result.success);
        assert_eq!(
            fs::read_to_string(&target).expect("read copied file"),
            "hello"
        );
    }

    #[test]
    fn test_scp_file() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let source = temp.path().join("source.txt");
        let target = temp.path().join("target.txt");
        fs::write(&source, "hello").expect("write source file");

        let result =
            scp_file(&local_client(), &source, target.to_str().unwrap()).expect("scp file");

        assert!(result.success);
        assert_eq!(
            fs::read_to_string(&target).expect("read copied file"),
            "hello"
        );
    }

    #[test]
    fn process_output_result_returns_success_for_zero_exit() {
        let output = Command::new("sh")
            .args(["-c", "exit 0"])
            .output()
            .expect("run success fixture");

        let result = process_output_result(output);

        assert!(result.success);
        assert_eq!(result.exit_code, 0);
        assert!(result.error.is_none());
    }

    #[test]
    fn process_output_result_captures_stderr_for_failed_exit() {
        let output = Command::new("sh")
            .args(["-c", "printf 'copy failed' >&2; exit 7"])
            .output()
            .expect("run failure fixture");

        let result = process_output_result(output);

        assert!(!result.success);
        assert_eq!(result.exit_code, 7);
        assert_eq!(result.error.as_deref(), Some("copy failed"));
    }
}
