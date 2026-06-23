use std::fs;
use std::path::Path;

use glob_match::glob_match;
use sha2::{Digest, Sha256};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::types::SnapshotStats;
use super::util::{
    git_output, hex_prefix, owner_capture_shell, owner_restore_shell, parent_remote_path,
    run_shell_command, ssh_args, ssh_client_for_runner, tar_exclude_args,
};

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
        pattern == rel || pattern == name || glob_match(pattern, rel) || glob_match(pattern, name)
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
    let parent = parent_remote_path(remote_path);
    format!(
        "parent={parent}; dest={dest}; tmp=\"${{dest}}.tmp.$$\"; {owner_capture}; mkdir -p \"$parent\" && trap 'rm -rf \"$tmp\"' EXIT; rm -rf \"$tmp\" && mkdir -p \"$tmp\" && tar -C \"$tmp\" -xf - && rm -rf \"$dest\" && mv \"$tmp\" \"$dest\" && {owner_restore}",
        parent = shell::quote_arg(parent.as_str()),
        dest = shell::quote_arg(remote_path),
        owner_capture = owner_capture_shell("$parent"),
        owner_restore = owner_restore_shell("$parent", "$dest"),
    )
}
