use std::fs;
use std::path::Path;

use glob_match::glob_match;
use sha2::{Digest, Sha256};

use crate::engine::shell;
use crate::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::materializer::{WorkspaceMaterializationOperation, WorkspaceMaterializer};
use super::types::SnapshotStats;
use super::util::{
    git_output, hex_prefix, run_shell_capture, run_shell_command, ssh_args, ssh_client_for_runner,
    tar_exclude_args,
};

const RUNNER_WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";
pub(crate) const WORKSPACE_CONTENT_PERMISSION_PORTABLE: &str = "portable-content-only";
pub(crate) const WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE: &str = "unix-executable";

#[cfg(unix)]
pub(crate) const WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY: &str =
    WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE;
#[cfg(not(unix))]
pub(crate) const WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY: &str =
    WORKSPACE_CONTENT_PERMISSION_PORTABLE;

pub(crate) fn snapshot_identity(
    local_path: &Path,
    excludes: &[String],
    includes: &[String],
) -> Result<String> {
    let head =
        git_output(local_path, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "nogit".to_string());
    let status = git_output(local_path, &["status", "--porcelain=v1"])
        .unwrap_or_else(|_| "nogit".to_string());
    let diff = git_output(local_path, &["diff", "--binary", "HEAD"]).unwrap_or_default();
    let staged =
        git_output(local_path, &["diff", "--cached", "--binary", "HEAD"]).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(local_path.display().to_string().as_bytes());
    hasher.update(head.as_bytes());
    hasher.update(status.as_bytes());
    hasher.update(diff.as_bytes());
    hasher.update(staged.as_bytes());
    hash_snapshot_tree(local_path, local_path, excludes, includes, &mut hasher)?;
    Ok(format!("snapshot:{}", hex_prefix(&hasher.finalize(), 16)))
}

/// Stable v2 digest of the files a snapshot materializes. Unlike
/// `snapshot_identity`, this is portable across controller and runner paths.
/// The declared permission policy is part of the algorithm marker so a digest
/// cannot be interpreted under a different platform capability contract.
pub(crate) fn workspace_content_hash(path: &Path, excludes: &[String]) -> Result<String> {
    workspace_content_hash_for_policy(path, excludes, WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY)
}

pub(crate) fn workspace_content_hash_algorithm(policy: &str) -> Option<String> {
    match policy {
        WORKSPACE_CONTENT_PERMISSION_PORTABLE => {
            Some("homeboy-workspace-content-v2+portable-content-only".to_string())
        }
        WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE if cfg!(unix) => {
            Some("homeboy-workspace-content-v2+unix-executable".to_string())
        }
        _ => None,
    }
}

pub(crate) fn workspace_content_hash_for_policy(
    path: &Path,
    excludes: &[String],
    policy: &str,
) -> Result<String> {
    let algorithm = workspace_content_hash_algorithm(policy).ok_or_else(|| {
        Error::validation_invalid_argument(
            "permission_policy",
            "workspace content hash policy is unsupported on this platform",
            Some(policy.to_string()),
            None,
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(algorithm.as_bytes());
    hasher.update(b"\0");
    let root = content_hash_root(path)?;
    collect_content_hash_entries_v2(
        &root,
        &root,
        Path::new(""),
        excludes,
        &mut vec![root.clone()],
        &mut hasher,
        policy == WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    )?;
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Exact historical v1 algorithm for controllers that emitted
/// `homeboy/lab-workspace-verification/v1` metadata.
pub(crate) fn workspace_content_hash_v1(path: &Path, excludes: &[String]) -> Result<String> {
    let mut entries = Vec::new();
    let root = content_hash_root(path)?;
    collect_content_hash_entries_v1(
        &root,
        &root,
        Path::new(""),
        excludes,
        &mut vec![root.clone()],
        &mut entries,
    )?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    hasher.update(b"homeboy-workspace-content-v1\0");
    for (relative, kind, mode, contents) in entries {
        hasher.update(relative.as_bytes());
        hasher.update(kind.as_bytes());
        hasher.update(mode.to_le_bytes());
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(contents);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn content_hash_root(path: &Path) -> Result<std::path::PathBuf> {
    path.canonicalize().map_err(|err| {
        Error::internal_io(err.to_string(), Some("canonicalize workspace".to_string()))
    })
}

fn collect_content_hash_entries_v2(
    root: &Path,
    path: &Path,
    logical: &Path,
    excludes: &[String],
    ancestors: &mut Vec<std::path::PathBuf>,
    hasher: &mut Sha256,
    include_executable_bit: bool,
) -> Result<()> {
    let mut children = fs::read_dir(path)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
    children.sort_by_key(|entry| entry.path());
    for entry in children {
        let entry_path = entry.path();
        let relative_path = logical.join(entry.file_name());
        let relative = relative_path.to_string_lossy().replace('\\', "/");
        let is_runner_metadata_directory = relative == ".homeboy";
        if relative == ".git"
            || is_excluded(root, &root.join(&relative_path), excludes, &[])
            || relative == RUNNER_WORKSPACE_METADATA_FILE
        {
            continue;
        }
        let link_metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        let resolved = if link_metadata.file_type().is_symlink() {
            entry_path.canonicalize().map_err(|err| {
                Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused an unresolved symlink",
                    Some(err.to_string()),
                    None,
                )
            })?
        } else {
            entry_path.clone()
        };
        let metadata = fs::metadata(&resolved).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            let canonical = resolved
                .canonicalize()
                .map_err(|err| Error::internal_io(err.to_string(), None))?;
            if ancestors.contains(&canonical) {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused a symlink cycle",
                    Some(entry_path.display().to_string()),
                    None,
                ));
            }
            // The runner adds `.homeboy/runner-workspace.json` after transport.
            // Recurse through the directory so user-owned children remain bound,
            // but omit the transport-owned container entry and record itself.
            if !is_runner_metadata_directory {
                hasher.update(relative.as_bytes());
                hasher.update(b"\0dir\0");
            }
            ancestors.push(canonical);
            collect_content_hash_entries_v2(
                root,
                &resolved,
                &relative_path,
                excludes,
                ancestors,
                hasher,
                include_executable_bit,
            )?;
            ancestors.pop();
        } else if metadata.is_file() {
            let contents = fs::read(&resolved).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read sync file".to_string()))
            })?;
            hasher.update(relative.as_bytes());
            hasher.update(b"\0file\0");
            if include_executable_bit {
                hasher.update([file_is_executable(&metadata) as u8]);
            }
            hasher.update((contents.len() as u64).to_le_bytes());
            hasher.update(contents);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn file_is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn file_is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn mode_bits(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode_bits(_metadata: &fs::Metadata) -> u32 {
    0
}

fn collect_content_hash_entries_v1(
    root: &Path,
    path: &Path,
    logical: &Path,
    excludes: &[String],
    ancestors: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<(String, &'static str, u32, Vec<u8>)>,
) -> Result<()> {
    let mut children = fs::read_dir(path)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
    children.sort_by_key(|entry| entry.path());
    for entry in children {
        let entry_path = entry.path();
        let relative_path = logical.join(entry.file_name());
        let relative = relative_path.to_string_lossy().replace('\\', "/");
        let is_runner_metadata_directory = relative == ".homeboy";
        if relative == ".git"
            || is_excluded(root, &root.join(&relative_path), excludes, &[])
            || relative == RUNNER_WORKSPACE_METADATA_FILE
        {
            continue;
        }
        let link_metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        let resolved = if link_metadata.file_type().is_symlink() {
            entry_path.canonicalize().map_err(|err| {
                Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused an unresolved symlink",
                    Some(err.to_string()),
                    None,
                )
            })?
        } else {
            entry_path.clone()
        };
        let metadata = fs::metadata(&resolved).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            let canonical = resolved
                .canonicalize()
                .map_err(|err| Error::internal_io(err.to_string(), None))?;
            if ancestors.contains(&canonical) {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused a symlink cycle",
                    Some(entry_path.display().to_string()),
                    None,
                ));
            }
            if !is_runner_metadata_directory {
                entries.push((relative, "\0dir\0", mode_bits(&metadata), Vec::new()));
            }
            ancestors.push(canonical);
            collect_content_hash_entries_v1(
                root,
                &resolved,
                &relative_path,
                excludes,
                ancestors,
                entries,
            )?;
            ancestors.pop();
        } else if metadata.is_file() {
            let contents = fs::read(&resolved).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read sync file".to_string()))
            })?;
            entries.push((relative, "\0file\0", mode_bits(&metadata), contents));
        }
    }
    Ok(())
}

pub(crate) fn local_snapshot_stats(
    path: &Path,
    excludes: &[String],
    includes: &[String],
) -> Result<SnapshotStats> {
    let mut stats = SnapshotStats { files: 0, bytes: 0 };
    collect_stats(path, path, excludes, includes, &mut stats)?;
    Ok(stats)
}

fn hash_snapshot_tree(
    root: &Path,
    path: &Path,
    excludes: &[String],
    includes: &[String],
    hasher: &mut Sha256,
) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let entry_path = entry.path();
        if is_excluded(root, &entry_path, excludes, includes) {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        let rel = entry_path
            .strip_prefix(root)
            .unwrap_or(&entry_path)
            .to_string_lossy();
        hasher.update(rel.as_bytes());
        if metadata.is_dir() {
            hasher.update(b"/dir");
            hash_snapshot_tree(root, &entry_path, excludes, includes, hasher)?;
        } else if metadata.is_file() {
            hasher.update(b"/file");
            hasher.update(metadata.len().to_le_bytes());
            let contents = fs::read(&entry_path).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read sync file".to_string()))
            })?;
            hasher.update(contents);
        }
    }
    Ok(())
}

fn collect_stats(
    root: &Path,
    path: &Path,
    excludes: &[String],
    includes: &[String],
    stats: &mut SnapshotStats,
) -> Result<()> {
    for entry in fs::read_dir(path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
        let entry_path = entry.path();
        if is_excluded(root, &entry_path, excludes, includes) {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            collect_stats(root, &entry_path, excludes, includes, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    Ok(())
}

pub(super) fn is_excluded(
    root: &Path,
    path: &Path,
    excludes: &[String],
    includes: &[String],
) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let rel = rel.trim_start_matches('/');
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if includes.iter().any(|pattern| {
        pattern == rel || pattern == name || glob_match(pattern, rel) || glob_match(pattern, name)
    }) {
        return false;
    }
    excludes.iter().any(|pattern| {
        let directory_pattern = pattern.trim_end_matches('/');
        pattern == rel
            || pattern == name
            || directory_pattern == rel
            || glob_match(pattern, rel)
            || glob_match(pattern, name)
    })
}

pub(crate) fn materialize_snapshot(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<()> {
    match runner.kind {
        RunnerKind::Local => materialize_snapshot_piped(
            local_path,
            &format!(
                "sh -c {}",
                shell::quote_arg(&snapshot_install_command(remote_path))
            ),
            excludes,
            "materialize local workspace snapshot",
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                materialize_snapshot_piped(
                    local_path,
                    &format!(
                        "sh -c {}",
                        shell::quote_arg(&snapshot_install_command(remote_path))
                    ),
                    excludes,
                    "materialize local workspace snapshot",
                )
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let remote_command = snapshot_install_command(remote_path);
                let target = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&remote_command),
                );
                materialize_snapshot_piped(
                    local_path,
                    &target,
                    excludes,
                    "materialize SSH workspace snapshot",
                )
            }
        }
    }
}

pub(crate) fn materialize_snapshot_git(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    snapshot: &str,
) -> Result<SyntheticCheckoutIdentity> {
    materialize_snapshot(runner, local_path, remote_path, excludes)?;
    initialize_synthetic_git_checkout(runner, local_path, remote_path, snapshot)
}

/// Identity of the synthetic git checkout materialized for a `snapshot-git`
/// sync. Surfaced as run evidence so write-capable agent-task dispatches can
/// trace the dirty controller-side worktree back to the synthetic commit that
/// carries the snapshot into the runner workspace.
#[derive(Debug, Clone)]
pub(crate) struct SyntheticCheckoutIdentity {
    /// Commit SHA of the synthetic checkout created in the runner workspace.
    pub(crate) synthetic_commit: String,
    pub(crate) synthetic_ref: String,
    pub(crate) synthetic_tree: String,
}

fn initialize_synthetic_git_checkout(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    snapshot: &str,
) -> Result<SyntheticCheckoutIdentity> {
    let remote_url = git_output(local_path, &["config", "--get", "remote.origin.url"]).ok();
    let source_head = git_output(local_path, &["rev-parse", "HEAD"]).ok();
    let command = synthetic_git_checkout_command(
        remote_path,
        snapshot,
        remote_url.as_deref(),
        source_head.as_deref(),
    );

    match runner.kind {
        RunnerKind::Local => {
            run_shell_command(&command, "initialize synthetic snapshot git checkout")?;
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                run_shell_command(&command, "initialize synthetic snapshot git checkout")?;
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let ssh_command = format!(
                    "ssh {ssh_args} {remote} {command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    command = shell::quote_arg(&command),
                );
                run_shell_command(
                    &ssh_command,
                    "initialize SSH synthetic snapshot git checkout",
                )?;
            }
        }
    }

    // The readback is the provenance contract for later harvest verification.
    // A sync without it must fail rather than return an unverifiable workspace.
    let synthetic_commit = synthetic_checkout_value(runner, remote_path, "rev-parse HEAD")?;
    let synthetic_ref = synthetic_checkout_value(runner, remote_path, "symbolic-ref --quiet HEAD")?;
    let synthetic_tree = synthetic_checkout_value(runner, remote_path, "rev-parse HEAD^{tree}")?;

    Ok(SyntheticCheckoutIdentity {
        synthetic_commit,
        synthetic_ref,
        synthetic_tree,
    })
}

pub(super) fn synthetic_checkout_value(
    runner: &Runner,
    remote_path: &str,
    command: &str,
) -> Result<String> {
    let value = match runner.kind {
        RunnerKind::Local => run_shell_capture(&format!(
            "git -C {} {command}",
            shell::quote_arg(remote_path)
        )),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                run_shell_capture(&format!(
                    "git -C {} {command}",
                    shell::quote_arg(remote_path)
                ))
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let remote_command = format!(
                    "git -C {remote_path} {command}",
                    remote_path = shell::quote_arg(remote_path),
                );
                let ssh_command = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&remote_command),
                );
                run_shell_capture(&ssh_command)
            }
        }
    };
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::internal_io(
                format!(
                "could not read `{command}` from synthetic snapshot-git checkout at `{remote_path}`"
            ),
                Some("capture synthetic snapshot-git checkout provenance".to_string()),
            )
        })
}

fn synthetic_git_checkout_command(
    remote_path: &str,
    snapshot: &str,
    remote_url: Option<&str>,
    source_head: Option<&str>,
) -> String {
    let remote_path = shell::quote_arg(remote_path);
    let snapshot = shell::quote_arg(snapshot);
    let source_head = shell::quote_arg(source_head.unwrap_or("unknown"));
    let set_remote = remote_url
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            format!(
                " && git -C {remote_path} remote add origin {remote_url}",
                remote_url = shell::quote_arg(value)
            )
        })
        .unwrap_or_default();

    format!(
        "git -C {remote_path} init && git -C {remote_path} config user.email homeboy-snapshot@localhost && git -C {remote_path} config user.name 'Homeboy Snapshot' && git -C {remote_path} add -A && env GIT_AUTHOR_NAME='Homeboy Snapshot' GIT_AUTHOR_EMAIL=homeboy-snapshot@localhost GIT_COMMITTER_NAME='Homeboy Snapshot' GIT_COMMITTER_EMAIL=homeboy-snapshot@localhost GIT_AUTHOR_DATE='1970-01-01T00:00:00Z' GIT_COMMITTER_DATE='1970-01-01T00:00:00Z' git -C {remote_path} commit --allow-empty -m {message} --no-gpg-sign && git -C {remote_path} notes --ref=homeboy-snapshot add -m {note} HEAD{set_remote}",
        message = shell::quote_arg(&format!("Homeboy snapshot {snapshot}")),
        note = shell::quote_arg(&format!("snapshot_identity={snapshot}\nsource_head={source_head}")),
    )
}

pub(crate) fn copy_snapshot_to_directory(
    local_path: &Path,
    destination: &Path,
    excludes: &[String],
) -> Result<()> {
    materialize_snapshot_piped(
        local_path,
        &format!(
            "sh -c {}",
            shell::quote_arg(&snapshot_install_command(
                &destination.display().to_string()
            ))
        ),
        excludes,
        "prepare local workspace snapshot",
    )
}

pub(crate) fn ensure_no_runner_workspace_metadata_collision(local_path: &Path) -> Result<()> {
    let metadata_path = local_path.join(RUNNER_WORKSPACE_METADATA_FILE);
    match fs::symlink_metadata(&metadata_path) {
        Ok(_) => Err(Error::validation_invalid_argument(
            "workspace",
            "source workspace contains the reserved runner metadata path `.homeboy/runner-workspace.json`; remove or rename it before syncing",
            Some(metadata_path.display().to_string()),
            Some(vec![
                "Remove or rename the source file before syncing; Homeboy writes this path only after runner materialization.".to_string(),
            ]),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some("inspect reserved runner metadata path".to_string()),
        )),
    }
}

fn materialize_snapshot_piped(
    local_path: &Path,
    target_command: &str,
    excludes: &[String],
    action: &str,
) -> Result<()> {
    let command = snapshot_archive_command(local_path, target_command, excludes);
    run_shell_command(&command, action)
}

pub(super) fn snapshot_archive_command(
    local_path: &Path,
    target_command: &str,
    excludes: &[String],
) -> String {
    // `-h`/`--dereference` follows symlinks and archives their target contents
    // instead of recording the link itself. Controller-native workspaces often
    // wire local dependencies into the tree via symlinks (e.g. a `.ci/<dep>`
    // entry pointing at a sibling checkout/worktree). Archiving those as plain
    // links produces a runner snapshot whose links dangle, so embedded plan
    // paths that traverse a symlinked dependency resolve to missing files on the
    // runner. Dereferencing materializes the real dependency contents into the
    // snapshot so offloaded plans find them at the remapped path (#3913).
    format!(
        "COPYFILE_DISABLE=1 tar --no-xattrs -h -C {src} {exclude} -cf - . | {target_command}",
        src = shell::quote_arg(&local_path.display().to_string()),
        exclude = tar_exclude_args(excludes),
        target_command = target_command,
    )
}

pub(crate) fn effective_snapshot_excludes(
    excludes: Vec<String>,
    includes: &[String],
) -> Vec<String> {
    if includes.is_empty() {
        return excludes;
    }

    excludes
        .into_iter()
        .filter(|exclude| !includes_override_exclude(includes, exclude))
        .collect()
}

fn includes_override_exclude(includes: &[String], exclude: &str) -> bool {
    let excluded_name = exclude
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/');
    if excluded_name.is_empty() || excluded_name.contains('*') || excluded_name.contains('/') {
        return false;
    }

    includes.iter().any(|include| {
        include
            .trim_start_matches("./")
            .split('/')
            .any(|segment| segment == excluded_name)
    })
}

pub(super) fn snapshot_install_command(remote_path: &str) -> String {
    WorkspaceMaterializer::new(remote_path)
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::CleanupOnExit(vec![
            "\"$tmp\"".to_string(),
        ]))
        .op(WorkspaceMaterializationOperation::RecreateTempDir)
        .op(WorkspaceMaterializationOperation::ExtractTarStdinToTemp)
        .op(WorkspaceMaterializationOperation::AtomicReplaceTemp)
        .restore_owner()
        .command()
}
