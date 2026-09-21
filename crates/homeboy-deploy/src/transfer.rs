use std::path::Path;
use std::process::{Command, Output};

use homeboy_core::defaults;
use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::server::SshClient;

use super::types::DeployResult;

pub(super) fn upload_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    rsync_directory(ssh_client, local_path, remote_path)
}

/// Base rsync arguments shared by the local and remote directory paths.
///
/// `--delete` keeps the target an exact mirror of the source: files removed or
/// moved in the source are removed from the target. Without it, stale files
/// accumulate on the server and can shadow new files (e.g. when a PHP
/// autoloader loads an old copy).
///
/// `--delay-updates` and `--delete-after` exist because the target is
/// usually being read while it is written. A deploy target is a live
/// application directory, not an idle mirror. Plain `rsync -a --delete`
/// mutates that directory in place, file by file, for the whole duration of
/// the transfer, so a concurrent reader can observe a tree that is half old
/// and half new — or, because `--delete` may remove a file before its
/// replacement arrives, a file that is briefly absent entirely.
///
/// That is not theoretical. A component whose release introduced a new
/// function in one file and its first caller in another produced, four
/// minutes after a successful deploy:
///
/// ```text
/// PHP Fatal error: Uncaught Error: Call to undefined function
///   extrachill_cache_filter_headers() in .../inc/page-cache.php on line 156
/// ```
///
/// The definition could not be missing in a consistent tree; the request had
/// bound the new caller against the old definition.
///
/// With these flags rsync stages every transferred file beside its
/// destination and renames them in at the end of the run, and defers
/// deletions until after the transfer. The inconsistency window shrinks from
/// "the entire transfer" to "the final batch of renames".
///
/// `--delete-after` rather than `--delete-delayed`: the latter is not
/// universally available (rsync 3.2.7 rejects it outright with "unknown
/// option"), and `--delete-after` achieves the property that matters here —
/// no file is removed before its replacement has been transferred.
///
/// This is deliberately not a claim of atomicity. Truly atomic replacement of
/// a directory needs `renameat2(RENAME_EXCHANGE)`, which is Linux-specific and
/// not reachable over a shell, or a release-directory-plus-symlink layout that
/// changes the on-server contract for every consumer. A naive two-step
/// `mv old away; mv new in` is worse than what we have now, because between
/// the two renames the application directory does not exist at all. Closing
/// the remaining window is tracked separately; this removes the large one.
fn rsync_base_args() -> Vec<String> {
    vec![
        // archive mode (recursive, preserves permissions, timestamps, etc.)
        "-a".to_string(),
        // remove files on target that don't exist in source
        "--delete".to_string(),
        // stage transferred files and rename them in at the end of the run
        "--delay-updates".to_string(),
        // run deletions after the transfer completes rather than before it,
        // so `--delete` cannot remove a file before its replacement arrives
        "--delete-after".to_string(),
    ]
}

/// Sync a local directory to the remote using rsync.
///
/// See [`rsync_base_args`] for why the flag set is what it is.
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
        homeboy_core::log_status!(
            "deploy",
            "Syncing directory (local rsync): {} -> {}",
            local_str,
            remote_str
        );

        let mut rsync_args = rsync_base_args();
        rsync_args.push(local_str);
        rsync_args.push(remote_str);

        let output = Command::new("rsync").args(&rsync_args).output();
        return match output {
            Ok(output) => Ok(process_output_result(output)),
            Err(err) => Ok(DeployResult::failure(1, format!("rsync failed: {}", err))),
        };
    }

    // Remote deploy: rsync over SSH
    let mut rsync_args = rsync_base_args();

    let mut ssh_cmd_parts = vec!["ssh".to_string()];
    ssh_cmd_parts.extend(homeboy_core::server::ssh_args::client_option_args(
        ssh_client,
        homeboy_core::server::ssh_args::SshArgOptions {
            batch_mode: true,
            connect_timeout: true,
            port_flag: Some(homeboy_core::server::ssh_args::SshPortFlag::Lowercase),
            ..homeboy_core::server::ssh_args::SshArgOptions::default()
        },
    ));

    rsync_args.extend(["-e".to_string(), ssh_cmd_parts.join(" ")]);
    rsync_args.push(local_str.clone());
    rsync_args.push(format!(
        "{}@{}:{}",
        ssh_client.user, ssh_client.host, remote_str
    ));

    homeboy_core::log_status!(
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
        homeboy_core::log_status!(
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

    homeboy_core::log_status!(
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
    use super::{
        process_output_result, rsync_base_args, scp_file, upload_directory, upload_file,
    };
    use crate::test_support::local_client;
    use std::fs;
    use std::process::Command;

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

    /// The deploy target is read while it is written, so the flag set is a
    /// correctness property rather than a tuning preference. Losing
    /// `--delay-updates` returns the inconsistency window to the full duration
    /// of the transfer; losing `--delete-after` lets `--delete` remove a file
    /// before its replacement lands, which a concurrent reader observes as a
    /// missing file. Both produced real production fatals (issue #14861).
    #[test]
    fn rsync_base_args_defer_updates_and_deletes_to_the_end() {
        let args = rsync_base_args();

        assert!(args.contains(&"-a".to_string()), "archive mode is required");
        assert!(
            args.contains(&"--delete".to_string()),
            "target must mirror source exactly or stale files shadow new ones"
        );
        assert!(
            args.contains(&"--delay-updates".to_string()),
            "transferred files must be renamed in at the end, not in place"
        );
        assert!(
            args.contains(&"--delete-after".to_string()),
            "deletions must run after the transfer, not before it"
        );
    }

    /// Deferring the renames must not weaken the mirror guarantee: files added
    /// to the source still arrive, and files removed from the source are still
    /// deleted from the target. A `--delete-after` that quietly stopped
    /// deleting would satisfy the flag assertion above while reintroducing the
    /// stale-file shadowing `--delete` exists to prevent.
    #[test]
    fn delayed_flags_still_mirror_the_source_exactly() {
        if Command::new("rsync").arg("--version").output().is_err() {
            eprintln!("rsync unavailable; skipping mirror check");
            return;
        }

        let base = std::env::temp_dir().join(format!(
            "homeboy-rsync-mirror-{}-{}",
            std::process::id(),
            line!()
        ));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(&src).expect("create source");
        std::fs::create_dir_all(&dst).expect("create target");

        // Target starts with a file the source does not have.
        std::fs::write(dst.join("removed.txt"), b"obsolete").expect("seed target");
        std::fs::write(src.join("kept.txt"), b"current").expect("seed source");

        let mut args = rsync_base_args();
        args.push(format!("{}/", src.display()));
        args.push(format!("{}/", dst.display()));

        let output = Command::new("rsync")
            .args(&args)
            .output()
            .expect("run rsync");
        assert!(
            output.status.success(),
            "rsync rejected the flag set: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(
            dst.join("kept.txt").exists(),
            "source file did not arrive at the target"
        );
        assert!(
            !dst.join("removed.txt").exists(),
            "--delete-after did not delete; mirror guarantee is broken"
        );
        assert!(
            std::fs::read_dir(&dst)
                .expect("read target")
                .filter_map(|e| e.ok())
                .all(|e| !e.file_name().to_string_lossy().starts_with('.')),
            "staging artifacts were left behind in the target"
        );

        let _ = std::fs::remove_dir_all(&base);
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
