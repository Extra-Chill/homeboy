use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use glob_match::glob_match;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::core::engine::{shell, temp};
use crate::core::error::{Error, Result};
use crate::core::server::{self, Server, SshClient};

use super::{load, Runner, RunnerKind};

pub(super) const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".git/**",
    "._*",
    "**/._*",
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa",
    "id_ed25519",
    ".ssh",
    ".ssh/**",
    "*.p12",
    "*.pfx",
    "node_modules",
    "node_modules/**",
    "target",
    "target/**",
    "dist",
    "dist/**",
    ".next",
    ".next/**",
    ".turbo",
    ".turbo/**",
    ".cache",
    ".cache/**",
];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkspaceSyncMode {
    Snapshot,
    Git,
}

impl RunnerWorkspaceSyncMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Git => "git",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunnerWorkspaceSyncOptions {
    pub path: String,
    pub mode: RunnerWorkspaceSyncMode,
    pub controller_routed_git: bool,
    pub changed_since_base: Option<String>,
    pub git_fetch_refs: Vec<String>,
    pub snapshot_includes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceSyncOutput {
    pub command: &'static str,
    pub runner_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub snapshot_identity: String,
    pub files: usize,
    pub bytes: u64,
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
}

pub fn sync_workspace(
    runner_id: &str,
    options: RunnerWorkspaceSyncOptions,
) -> Result<(RunnerWorkspaceSyncOutput, i32)> {
    let runner = load(runner_id)?;
    let local_path = canonical_workspace_path(&options.path)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner workspace sync requires workspace_root",
            Some(runner.id.clone()),
            Some(vec![
                "Set runner.workspace_root to the remote workspace directory.".to_string(),
            ]),
        )
    })?;
    validate_absolute_path("workspace_root", workspace_root)?;

    let mut excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for pattern in &runner.policy.snapshot_excludes {
        if !excludes.contains(pattern) {
            excludes.push(pattern.clone());
        }
    }
    let mut includes = runner.policy.snapshot_includes.clone();
    for pattern in options.snapshot_includes {
        if !includes.contains(&pattern) {
            includes.push(pattern);
        }
    }
    let excludes = effective_snapshot_excludes(excludes, &includes);

    match options.mode {
        RunnerWorkspaceSyncMode::Snapshot => {
            let snapshot = snapshot_identity(&local_path, &excludes, &includes)?;
            let remote_path = temp::unique_name(
                &deterministic_remote_path(workspace_root, &local_path, &snapshot),
                "",
            );
            let stats = local_snapshot_stats(&local_path, &excludes, &includes)?;
            materialize_snapshot(&runner, &local_path, &remote_path, &excludes)?;
            super::validation_dependencies::sync_validation_dependency_workspaces(
                &runner,
                &local_path,
                &remote_path,
                &excludes,
            )?;
            Ok((
                RunnerWorkspaceSyncOutput {
                    command: "runner.workspace.sync",
                    runner_id: runner.id,
                    local_path: local_path.display().to_string(),
                    remote_path,
                    sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                    snapshot_identity: snapshot,
                    files: stats.files,
                    bytes: stats.bytes,
                    excludes,
                    includes,
                },
                0,
            ))
        }
        RunnerWorkspaceSyncMode::Git => {
            let git = git_snapshot(
                &local_path,
                options.changed_since_base.as_deref(),
                options.git_fetch_refs,
            )?;
            let remote_path = deterministic_remote_path(workspace_root, &local_path, &git.head);
            if options.controller_routed_git
                || super::source_materialization::requires_controller_routed_workspace_sync(
                    &git.remote_url,
                )
            {
                materialize_git_from_controller_bundle(
                    &runner,
                    &local_path,
                    &remote_path,
                    &git.head,
                    git.branch.as_deref(),
                    &git.remote_url,
                    git.changed_since_base.as_deref(),
                    &git.git_fetch_refs,
                )?;
            } else {
                if runner.kind != RunnerKind::Local {
                    super::source_materialization::validate_runner_git_materialization(
                        &git.remote_url,
                        &runner.id,
                    )?;
                }
                materialize_git(
                    &runner,
                    &remote_path,
                    &git.remote_url,
                    &git.head,
                    git.changed_since_base.as_deref(),
                    &git.git_fetch_refs,
                )?;
            }
            super::validation_dependencies::sync_validation_dependency_workspaces(
                &runner,
                &local_path,
                &remote_path,
                &excludes,
            )?;
            Ok((
                RunnerWorkspaceSyncOutput {
                    command: "runner.workspace.sync",
                    runner_id: runner.id,
                    local_path: local_path.display().to_string(),
                    remote_path,
                    sync_mode: RunnerWorkspaceSyncMode::Git,
                    snapshot_identity: git.head,
                    files: 0,
                    bytes: 0,
                    excludes,
                    includes,
                },
                0,
            ))
        }
    }
}

pub(super) struct SnapshotStats {
    pub(super) files: usize,
    pub(super) bytes: u64,
}

struct GitSnapshot {
    remote_url: String,
    head: String,
    branch: Option<String>,
    changed_since_base: Option<String>,
    git_fetch_refs: Vec<String>,
}

pub(super) fn canonical_workspace_path(path: &str) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(path).to_string();
    let path = Path::new(&expanded);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "path",
            format!("workspace sync path must be an existing directory: {expanded}"),
            None,
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(err.to_string(), Some("canonicalize sync path".to_string()))
    })
}

fn validate_absolute_path(field: &str, path: &str) -> Result<()> {
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

fn deterministic_remote_path(workspace_root: &str, local_path: &Path, snapshot: &str) -> String {
    let name = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    let mut hasher = Sha256::new();
    hasher.update(local_path.display().to_string().as_bytes());
    hasher.update(snapshot.as_bytes());
    let digest = hex_prefix(&hasher.finalize(), 12);
    format!(
        "{}/_lab_workspaces/{}-{}",
        workspace_root.trim_end_matches('/'),
        sanitize_path_segment(name),
        digest
    )
}

pub(super) fn snapshot_identity(
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

fn git_snapshot(
    local_path: &Path,
    changed_since_base: Option<&str>,
    git_fetch_refs: Vec<String>,
) -> Result<GitSnapshot> {
    let status = git_output(local_path, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        if changed_since_base.is_some() {
            return Err(Error::validation_invalid_argument(
                "mode",
                "git workspace sync requires a clean working tree for changed-since remote execution; snapshot sync cannot honor --changed-since because it excludes .git metadata",
                Some("git".to_string()),
                Some(vec![
                    "Commit or stash local changes before remote execution of a --changed-since command."
                        .to_string(),
                    "Run with --force-hot to execute the changed-since command locally."
                        .to_string(),
                    "Omit --changed-since to use snapshot remote execution for dirty local changes."
                        .to_string(),
                ]),
            ));
        }

        return Err(Error::validation_invalid_argument(
            "mode",
            "git workspace sync requires a clean working tree; use --mode snapshot to include dirty local changes",
            Some("git".to_string()),
            None,
        ));
    }
    let head = git_output(local_path, &["rev-parse", "HEAD"])?;
    let branch = git_output(local_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|branch| branch != "HEAD");
    let remote_url = git_output(local_path, &["config", "--get", "remote.origin.url"])?;
    if remote_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "remote.origin.url",
            "git workspace sync requires remote.origin.url",
            None,
            None,
        ));
    }
    Ok(GitSnapshot {
        remote_url,
        head,
        branch,
        changed_since_base: changed_since_base.map(str::to_string),
        git_fetch_refs,
    })
}

pub(super) fn git_output(local_path: &Path, args: &[&str]) -> Result<String> {
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

pub(super) fn local_snapshot_stats(
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

fn is_excluded(root: &Path, path: &Path, excludes: &[String], includes: &[String]) -> bool {
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

pub(super) fn materialize_snapshot(
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
    let command = format!(
        "COPYFILE_DISABLE=1 tar -C {src} {exclude} -cf - . | {target_command}",
        src = shell::quote_arg(&local_path.display().to_string()),
        exclude = tar_exclude_args(excludes),
        target_command = target_command,
    );
    run_shell_command(&command, action)
}

pub(super) fn effective_snapshot_excludes(
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

fn snapshot_install_command(remote_path: &str) -> String {
    format!(
        "parent={parent}; dest={dest}; tmp=\"${{dest}}.tmp.$$\"; mkdir -p \"$parent\" && trap 'rm -rf \"$tmp\"' EXIT; rm -rf \"$tmp\" && mkdir -p \"$tmp\" && tar -C \"$tmp\" -xf - && rm -rf \"$dest\" && mv \"$tmp\" \"$dest\"",
        parent = shell::quote_arg(parent_remote_path(remote_path).as_str()),
        dest = shell::quote_arg(remote_path),
    )
}

fn materialize_git(
    runner: &Runner,
    remote_path: &str,
    remote_url: &str,
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
) -> Result<()> {
    let command = materialize_git_command(
        remote_path,
        remote_url,
        head,
        changed_since_base,
        git_fetch_refs,
    );
    match runner.kind {
        RunnerKind::Local => run_shell_command(&command, "materialize local git workspace"),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            let output = client.execute(&command);
            if output.success {
                Ok(())
            } else {
                Err(Error::validation_invalid_argument(
                    "changed_since",
                    "runner dispatch could not make the requested --changed-since base reachable in the runner workspace before dispatch",
                    changed_since_base.map(str::to_string),
                    Some(vec![
                        "Verify the branch and base commit are pushed to origin.".to_string(),
                        "Run with --force-hot to execute the changed-since command locally."
                            .to_string(),
                        format!("Remote git error: {}", output.stderr.trim()),
                    ]),
                ))
            }
        }
    }
}

fn materialize_git_from_controller_bundle(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    head: &str,
    branch: Option<&str>,
    remote_url: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
) -> Result<()> {
    let bundle_dir = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create controller git bundle directory".to_string()),
        )
    })?;
    let bundle_path = bundle_dir.path().join("workspace.bundle");

    let mut refs = vec![
        head.to_string(),
        "--branches".to_string(),
        "--tags".to_string(),
    ];
    if let Some(base) = changed_since_base {
        refs.push(base.to_string());
    }
    refs.extend(git_fetch_refs.iter().cloned());

    let output = Command::new("git")
        .arg("bundle")
        .arg("create")
        .arg(&bundle_path)
        .args(&refs)
        .current_dir(local_path)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("create git bundle".to_string()))
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "create git bundle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let install_command = git_bundle_install_command(remote_path, head, branch, remote_url);
    let result = match runner.kind {
        RunnerKind::Local => materialize_git_bundle_piped(
            &bundle_path,
            &format!("sh -c {}", shell::quote_arg(&install_command)),
            "materialize local git bundle workspace",
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                materialize_git_bundle_piped(
                    &bundle_path,
                    &format!("sh -c {}", shell::quote_arg(&install_command)),
                    "materialize local git bundle workspace",
                )
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let target = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&install_command),
                );
                materialize_git_bundle_piped(
                    &bundle_path,
                    &target,
                    "materialize SSH git bundle workspace",
                )
            }
        }
    };

    result
}

fn materialize_git_bundle_piped(
    bundle_path: &Path,
    target_command: &str,
    action: &str,
) -> Result<()> {
    let command = format!(
        "cat {bundle} | {target_command}",
        bundle = shell::quote_arg(&bundle_path.display().to_string()),
        target_command = target_command,
    );
    run_shell_command(&command, action)
}

fn git_bundle_install_command(
    remote_path: &str,
    head: &str,
    branch: Option<&str>,
    remote_url: &str,
) -> String {
    let checkout = if let Some(branch) = branch {
        format!(
            "git -C \"$tmp\" checkout -B {branch} {head} && git -C \"$tmp\" config branch.{branch}.remote origin && git -C \"$tmp\" config branch.{branch}.merge refs/heads/{branch}",
            branch = shell::quote_arg(branch),
            head = shell::quote_arg(head),
        )
    } else {
        format!(
            "git -C \"$tmp\" checkout --detach {head}",
            head = shell::quote_arg(head)
        )
    };

    format!(
        "parent={parent}; dest={dest}; tmp=\"${{dest}}.tmp.$$\"; bundle=\"${{dest}}.bundle.$$\"; mkdir -p \"$parent\" && trap 'rm -rf \"$tmp\" \"$bundle\"' EXIT; rm -rf \"$tmp\" \"$bundle\" && cat > \"$bundle\" && git clone \"$bundle\" \"$tmp\" && git -C \"$tmp\" remote set-url origin {remote_url} && {checkout} && git -C \"$tmp\" reset --hard {head} && git -C \"$tmp\" clean -ffdqx && rm -rf \"$dest\" && mv \"$tmp\" \"$dest\"",
        parent = shell::quote_arg(parent_remote_path(remote_path).as_str()),
        dest = shell::quote_arg(remote_path),
        remote_url = shell::quote_arg(remote_url),
        checkout = checkout,
        head = shell::quote_arg(head),
    )
}

fn materialize_git_command(
    remote_path: &str,
    remote_url: &str,
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
) -> String {
    let dest = shell::quote_arg(remote_path);
    let fetch_changed_since = changed_since_base
        .map(|base| {
            format!(
                " && (git -C {dest} rev-parse --verify -q {} >/dev/null || git -C {dest} fetch origin {})",
                shell::quote_arg(&format!("{base}^{{commit}}")),
                shell::quote_arg(base)
            )
        })
        .unwrap_or_default();
    let fetch_extra_refs = git_fetch_refs
        .iter()
        .map(|git_ref| {
            format!(
                " && git -C {dest} fetch origin {}",
                shell::quote_arg(git_ref)
            )
        })
        .collect::<String>();

    format!(
        "mkdir -p {parent} && if [ -d {dest}/.git ]; then git -C {dest} reset --hard && git -C {dest} clean -ffdqx && git -C {dest} fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; else rm -rf {dest} && git clone {url} {dest} && git -C {dest} fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; fi{fetch_changed_since}{fetch_extra_refs} && git -C {dest} checkout --detach {head} && git -C {dest} reset --hard {head} && git -C {dest} clean -ffdqx",
        parent = shell::quote_arg(parent_remote_path(remote_path).as_str()),
        dest = dest,
        url = shell::quote_arg(remote_url),
        head = shell::quote_arg(head),
        fetch_changed_since = fetch_changed_since,
        fetch_extra_refs = fetch_extra_refs,
    )
}

pub(super) fn ssh_client_for_runner(runner: &Runner) -> Result<(Server, SshClient)> {
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

fn run_shell_command(command: &str, action: &str) -> Result<()> {
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

fn tar_exclude_args(excludes: &[String]) -> String {
    excludes
        .iter()
        .map(|pattern| format!("--exclude {}", shell::quote_arg(pattern)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ssh_args(client: &SshClient) -> String {
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

pub(super) fn parent_remote_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
        .to_string()
}

pub(super) fn sanitize_path_segment(value: &str) -> String {
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

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_path_stays_under_workspace_root() {
        let path = Path::new("/Users/chubes/Developer/homeboy@fix-runner-workspace-sync");
        let remote = deterministic_remote_path("/srv/homeboy", path, "snapshot:abc");

        assert!(
            remote.starts_with("/srv/homeboy/_lab_workspaces/homeboy-fix-runner-workspace-sync-")
        );
    }

    #[test]
    fn default_excludes_filter_generated_outputs_and_secrets() {
        let root = Path::new("/repo");
        let excludes = DEFAULT_EXCLUDES
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();

        assert!(is_excluded(
            root,
            Path::new("/repo/node_modules/pkg/index.js"),
            &excludes,
            &[]
        ));
        assert!(is_excluded(
            root,
            Path::new("/repo/.env.local"),
            &excludes,
            &[]
        ));
        assert!(is_excluded(
            root,
            Path::new("/repo/target/debug/homeboy"),
            &excludes,
            &[]
        ));
        assert!(is_excluded(
            root,
            Path::new("/repo/src/__tests__/._index.js"),
            &excludes,
            &[]
        ));
        assert!(!is_excluded(
            root,
            Path::new("/repo/src/main.rs"),
            &excludes,
            &[]
        ));
        assert!(!is_excluded(
            root,
            Path::new("/repo/vendor/autoload.php"),
            &excludes,
            &[]
        ));
    }

    #[test]
    fn runner_snapshot_includes_override_generated_output_excludes() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source tempdir");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(source.path().join("packages/cli/dist")).expect("dist dir");
            fs::write(
                source.path().join("packages/cli/dist/homeboy.js"),
                "built\n",
            )
            .expect("built output");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-includes","kind":"local","workspace_root":"{}","policy":{{"snapshot_includes":["packages/cli/dist","packages/cli/dist/**"]}}}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local-includes",
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert!(output
                .includes
                .contains(&"packages/cli/dist/**".to_string()));
            assert!(!output.excludes.contains(&"dist".to_string()));
            assert!(Path::new(&output.remote_path)
                .join("packages/cli/dist/homeboy.js")
                .exists());
        });
    }

    #[test]
    fn runner_snapshot_excludes_extend_default_snapshot_policy() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source tempdir");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(source.path().join("src")).expect("src dir");
            fs::create_dir_all(source.path().join("generated-state")).expect("state dir");
            fs::write(source.path().join("src/source.txt"), "source\n").expect("source file");
            fs::write(source.path().join("generated-state/cache.bin"), "cache\n")
                .expect("excluded state file");
            fs::write(source.path().join("local.state"), "state\n").expect("excluded marker");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}","policy":{{"snapshot_excludes":["generated-state","generated-state/**","*.state"]}}}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local",
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert_eq!(output.files, 1);
            assert!(output.excludes.contains(&"generated-state/**".to_string()));
            assert!(Path::new(&output.remote_path)
                .join("src/source.txt")
                .exists());
            assert!(!Path::new(&output.remote_path)
                .join("generated-state/cache.bin")
                .exists());
            assert!(!Path::new(&output.remote_path).join("local.state").exists());
        });
    }

    #[test]
    fn test_sync_workspace() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source tempdir");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(source.path().join("src")).expect("src dir");
            fs::create_dir_all(source.path().join("build")).expect("root build dir");
            fs::create_dir_all(source.path().join("vendor")).expect("vendor dir");
            fs::create_dir_all(source.path().join("wordpress/scripts/build"))
                .expect("extension scripts build dir");
            fs::create_dir_all(source.path().join(".git")).expect("git dir");
            fs::create_dir_all(source.path().join("target/debug")).expect("target dir");
            fs::write(source.path().join("src/main.rs"), "fn main() {}\n").expect("source file");
            fs::write(source.path().join("build/bundle.js"), "artifact").expect("build file");
            fs::write(source.path().join("vendor/autoload.php"), "<?php\n").expect("vendor file");
            fs::write(
                source.path().join("wordpress/scripts/build/setup.sh"),
                "#!/bin/sh\n",
            )
            .expect("extension setup source file");
            fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n")
                .expect("git metadata");
            fs::write(source.path().join("src/._main.rs"), "appledouble").expect("sidecar file");
            fs::write(source.path().join(".env.local"), "SECRET=1\n").expect("secret file");
            fs::write(source.path().join("target/debug/homeboy"), "binary").expect("build file");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local",
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Snapshot);
            assert_eq!(output.files, 4);
            assert!(Path::new(&output.remote_path).join("src/main.rs").exists());
            assert!(Path::new(&output.remote_path)
                .join("vendor/autoload.php")
                .exists());
            assert!(Path::new(&output.remote_path)
                .join("wordpress/scripts/build/setup.sh")
                .exists());
            assert!(!Path::new(&output.remote_path).join(".git").exists());
            assert!(Path::new(&output.remote_path)
                .join("build/bundle.js")
                .exists());
            assert!(!Path::new(&output.remote_path)
                .join("src/._main.rs")
                .exists());
            assert!(!Path::new(&output.remote_path).join(".env.local").exists());
            assert!(!Path::new(&output.remote_path)
                .join("target/debug/homeboy")
                .exists());
        });
    }

    #[test]
    fn git_sync_for_private_remote_materializes_controller_bundle_checkout() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source tempdir");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            git(source.path(), &["init"]);
            git(source.path(), &["config", "user.email", "test@example.com"]);
            git(source.path(), &["config", "user.name", "Test User"]);
            fs::write(source.path().join("file.txt"), "base\n").expect("write file");
            git(source.path(), &["add", "."]);
            git(source.path(), &["commit", "-m", "base"]);
            git(
                source.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "git@github.a8c.com:chubes4/conductor.git",
                ],
            );

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-git-bundle","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local-git-bundle",
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Git,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Git);
            let remote = Path::new(&output.remote_path);
            assert_eq!(
                git_output(remote, &["rev-parse", "--is-inside-work-tree"]).unwrap(),
                "true"
            );
            assert_eq!(
                fs::read_to_string(remote.join("file.txt")).expect("read synced file"),
                "base\n"
            );
        });
    }

    #[test]
    fn controller_routed_git_sync_materializes_bundle_for_public_remote() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source tempdir");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            git(source.path(), &["init"]);
            git(source.path(), &["config", "user.email", "test@example.com"]);
            git(source.path(), &["config", "user.name", "Test User"]);
            git(source.path(), &["checkout", "-b", "fix/source-upgrade"]);
            fs::write(source.path().join("file.txt"), "source-upgrade\n").expect("write file");
            git(source.path(), &["add", "."]);
            git(source.path(), &["commit", "-m", "source upgrade"]);
            git(
                source.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/Extra-Chill/homeboy.git",
                ],
            );

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-controller-git","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local-controller-git",
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Git,
                    controller_routed_git: true,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Git);
            let remote = Path::new(&output.remote_path);
            assert_eq!(
                git_output(remote, &["rev-parse", "--is-inside-work-tree"]).unwrap(),
                "true"
            );
            assert_eq!(
                git_output(remote, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
                "fix/source-upgrade"
            );
            assert_eq!(
                git_output(remote, &["config", "--get", "remote.origin.url"]).unwrap(),
                "https://github.com/Extra-Chill/homeboy.git"
            );
            assert_eq!(
                fs::read_to_string(remote.join("file.txt")).expect("read synced file"),
                "source-upgrade\n"
            );
        });
    }

    #[test]
    fn snapshot_sync_uses_unique_clean_workspace_for_same_snapshot() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source tempdir");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::write(source.path().join("Cargo.toml"), "[package]\nname='app'\n")
                .expect("manifest");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let options = RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
            };
            let (first, _) = sync_workspace("lab-local", options.clone()).expect("first sync");
            let remote_path = Path::new(&first.remote_path);
            assert!(remote_path.join("Cargo.toml").exists());

            fs::write(remote_path.join("sentinel.txt"), "kept\n").expect("sentinel");

            let (second, _) = sync_workspace("lab-local", options).expect("second sync");
            let second_remote_path = Path::new(&second.remote_path);

            assert_ne!(second.remote_path, first.remote_path);
            assert!(second_remote_path.join("Cargo.toml").exists());
            assert!(!second_remote_path.join("sentinel.txt").exists());
            assert!(remote_path.join("sentinel.txt").exists());
        });
    }

    #[test]
    fn git_materialization_fetches_changed_since_base_before_checkout() {
        let command = materialize_git_command(
            "/srv/homeboy/_lab_workspaces/homeboy-abc",
            "https://github.com/Extra-Chill/homeboy.git",
            "abc123",
            Some("def456"),
            &[],
        );

        assert!(command.contains("fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'"));
        assert!(command.contains("rev-parse --verify -q 'def456^{commit}'"));
        assert!(command.contains("fetch origin def456"));
        assert!(command.contains("checkout --detach abc123"));
        assert!(command.contains("reset --hard"));
        assert!(command.contains("reset --hard abc123"));
    }

    #[test]
    fn git_materialization_fetches_extra_refs_before_checkout() {
        let command = materialize_git_command(
            "/srv/homeboy/_lab_workspaces/homeboy-abc",
            "https://github.com/Extra-Chill/homeboy.git",
            "abc123",
            None,
            &["refs/pull/5530/head".to_string()],
        );

        assert!(command.contains("fetch origin refs/pull/5530/head"));
        assert!(command.contains("checkout --detach abc123"));
    }

    #[test]
    fn dirty_git_sync_without_changed_since_suggests_snapshot_mode() {
        let source = dirty_git_repo();

        let err = match git_snapshot(source.path(), None, Vec::new()) {
            Ok(_) => panic!("dirty git sync should fail"),
            Err(err) => err,
        };

        assert!(err.message.contains("use --mode snapshot"));
    }

    #[test]
    fn dirty_changed_since_git_sync_explains_snapshot_is_unavailable() {
        let source = dirty_git_repo();

        let err = match git_snapshot(source.path(), Some("abc123"), Vec::new()) {
            Ok(_) => panic!("dirty changed-since git sync should fail"),
            Err(err) => err,
        };

        assert!(err.message.contains("requires a clean working tree"));
        assert!(err
            .message
            .contains("snapshot sync cannot honor --changed-since"));
        assert!(err.message.contains("because it excludes .git metadata"));
        assert!(!err.message.contains("use --mode snapshot"));
        let hint_text = err.details["tried"]
            .as_array()
            .expect("changed-since error includes recovery options")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(hint_text.contains("--force-hot"));
        assert!(hint_text.contains("Omit --changed-since"));
    }

    fn dirty_git_repo() -> tempfile::TempDir {
        let source = tempfile::tempdir().expect("source tempdir");
        git(source.path(), &["init"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        fs::write(source.path().join("file.txt"), "base\n").expect("write base");
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "base"]);
        fs::write(source.path().join("file.txt"), "dirty\n").expect("write dirty file");
        source
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
