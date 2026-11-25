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

    let mut scp_args: Vec<String> = defaults::load_defaults().deploy.scp_flags;

    if recursive {
        scp_args.push("-r".to_string());
    }

    scp_args.extend(homeboy_core::server::ssh_args::client_option_args(
        ssh_client,
        homeboy_core::server::ssh_args::SshArgOptions {
            batch_mode: true,
            connect_timeout: true,
            port_flag: Some(homeboy_core::server::ssh_args::SshPortFlag::Uppercase),
            ..homeboy_core::server::ssh_args::SshArgOptions::default()
        },
    ));

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
    use super::{process_output_result, rsync_base_args, upload_directory, upload_file};
    use crate::test_support::local_client;
    use std::fs;
    use std::process::Command;

    /// The deploy target is read while it is written, so the flag set is a
    /// correctness property rather than a tuning preference. Losing
    /// `--delay-updates` returns the inconsistency window to the full duration
    /// of the transfer; losing `--delete-after` lets `--delete` remove a file
    /// before its replacement lands, which a concurrent reader observes as a
    /// missing file. Both produced real production fatals (issue #14861).
    /// Deferring the renames must not weaken the mirror guarantee either:
    /// added files still arrive and removed files are still deleted.
    #[test]
    fn upload_directory_mirrors_source_with_deferred_updates() {
        let args = rsync_base_args();
        for flag in ["-a", "--delete", "--delay-updates", "--delete-after"] {
            assert!(args.contains(&flag.to_string()), "rsync must pass {flag}");
        }

        if Command::new("rsync").arg("--version").output().is_err() {
            eprintln!("rsync unavailable; skipping mirror check");
            return;
        }

        let temp = tempfile::tempdir().expect("create tempdir");
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).expect("create source");
        fs::create_dir_all(&dst).expect("create target");
        // Target starts with a file the source does not have.
        fs::write(dst.join("removed.txt"), b"obsolete").expect("seed target");
        fs::write(src.join("kept.txt"), b"current").expect("seed source");

        let result =
            upload_directory(&local_client(), &src, dst.to_str().unwrap()).expect("upload");

        assert!(
            result.success,
            "rsync rejected the flag set: {:?}",
            result.error
        );
        assert_eq!(fs::read_to_string(dst.join("kept.txt")).unwrap(), "current");
        assert!(
            !dst.join("removed.txt").exists(),
            "--delete-after did not delete; mirror guarantee is broken"
        );
        assert!(
            fs::read_dir(&dst)
                .expect("read target")
                .filter_map(|e| e.ok())
                .all(|e| !e.file_name().to_string_lossy().starts_with('.')),
            "staging artifacts were left behind in the target"
        );
    }

    #[test]
    fn upload_file_replaces_destination_without_leaving_temp_files() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let source = temp.path().join("source.txt");
        let target = temp.path().join("target.txt");
        fs::write(&source, "hello").expect("write source file");
        fs::write(&target, "old").expect("seed target file");

        let result =
            upload_file(&local_client(), &source, target.to_str().unwrap()).expect("upload file");

        assert!(result.success);
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        assert!(fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .all(|e| !e
                .file_name()
                .to_string_lossy()
                .starts_with(".homeboy-upload-")));
    }

    #[test]
    fn process_output_result_maps_exit_status_and_stderr() {
        for (script, success, exit_code, error) in [
            ("exit 0", true, 0, None),
            (
                "printf 'copy failed' >&2; exit 7",
                false,
                7,
                Some("copy failed"),
            ),
        ] {
            let output = Command::new("sh")
                .args(["-c", script])
                .output()
                .expect("run fixture");

            let result = process_output_result(output);

            assert_eq!(result.success, success, "{script}");
            assert_eq!(result.exit_code, exit_code, "{script}");
            assert_eq!(result.error.as_deref(), error, "{script}");
        }
    }
}
