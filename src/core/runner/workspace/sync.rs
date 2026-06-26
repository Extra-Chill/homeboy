use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine;

use crate::core::engine::temp;
use crate::core::error::{Error, Result};

use super::super::validation_dependencies::sync_validation_dependency_workspaces;
use super::super::{
    load, source_materialization, RunnerKind, RunnerLifecycleOwner, RunnerWorkspaceLease,
};
use super::git::{git_snapshot, materialize_git, materialize_git_from_controller_bundle};
use super::snapshot::{
    effective_snapshot_excludes, local_snapshot_stats, materialize_snapshot,
    materialize_snapshot_git, snapshot_identity,
};
use super::types::{
    canonical_workspace_path, ByteFileCounts, LocalGitState, RunnerWorkspaceCurrentSummary,
    RunnerWorkspaceListEntry, RunnerWorkspaceListOutput, RunnerWorkspaceMetadata,
    RunnerWorkspacePruneEntry, RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput,
    RunnerWorkspacePruneSkippedEntry, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
    RunnerWorkspaceSyncOutput, DEFAULT_EXCLUDES,
};
use super::util::{
    deterministic_remote_path, git_output, parent_remote_path, ssh_client_for_runner,
    validate_absolute_path,
};
use crate::core::engine::shell;
use crate::core::server::{self, SshClient};

const WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";

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
        RunnerWorkspaceSyncMode::Snapshot | RunnerWorkspaceSyncMode::SnapshotGit => {
            let snapshot = snapshot_identity(&local_path, &excludes, &includes)?;
            let remote_path = temp::unique_name(
                &deterministic_remote_path(
                    workspace_root,
                    &local_path,
                    &snapshot,
                    options.run_isolation_token.as_deref(),
                ),
                "",
            );
            let stats = local_snapshot_stats(&local_path, &excludes, &includes)?;
            let synthetic_checkout_commit = if options.mode == RunnerWorkspaceSyncMode::SnapshotGit
            {
                materialize_snapshot_git(&runner, &local_path, &remote_path, &excludes, &snapshot)?
                    .synthetic_commit
            } else {
                materialize_snapshot(&runner, &local_path, &remote_path, &excludes)?;
                None
            };
            write_workspace_metadata(
                &runner,
                workspace_metadata(
                    &runner.id,
                    &local_path,
                    &remote_path,
                    options.mode,
                    &snapshot,
                ),
            )?;
            let validation_dependencies = sync_validation_dependency_workspaces(
                &runner,
                &local_path,
                &remote_path,
                &excludes,
            )?;
            let current_workspace = current_workspace_summary(
                &local_path,
                &remote_path,
                options.mode,
                true,
                synthetic_checkout_commit,
            );
            let workspace_lease = workspace_lease(&runner.id, &current_workspace);
            Ok((
                RunnerWorkspaceSyncOutput {
                    variant: "workspace_sync",
                    command: "runner.workspace.sync",
                    runner_id: runner.id,
                    local_path: local_path.display().to_string(),
                    remote_path,
                    current_workspace,
                    workspace_lease,
                    sync_mode: options.mode,
                    snapshot_identity: snapshot,
                    counts: stats,
                    excludes,
                    includes,
                    workspace_cleanliness: if options.mode == RunnerWorkspaceSyncMode::SnapshotGit {
                        "snapshot_synthetic_git_unique_workspace".to_string()
                    } else {
                        "snapshot_unique_workspace".to_string()
                    },
                    validation_dependencies,
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
            let remote_path = deterministic_remote_path(
                workspace_root,
                &local_path,
                &git.head,
                options.run_isolation_token.as_deref(),
            );
            if options.controller_routed_git
                || git.branch.is_none()
                || source_materialization::requires_controller_routed_workspace_sync(
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
                    options.allow_dirty_lab_workspace,
                )?;
            } else {
                if runner.kind != RunnerKind::Local {
                    source_materialization::validate_runner_git_materialization(
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
                    options.allow_dirty_lab_workspace,
                )?;
            }
            write_workspace_metadata(
                &runner,
                workspace_metadata(
                    &runner.id,
                    &local_path,
                    &remote_path,
                    RunnerWorkspaceSyncMode::Git,
                    &git.head,
                ),
            )?;
            let validation_dependencies = sync_validation_dependency_workspaces(
                &runner,
                &local_path,
                &remote_path,
                &excludes,
            )?;
            let current_workspace = current_workspace_summary(
                &local_path,
                &remote_path,
                RunnerWorkspaceSyncMode::Git,
                true,
                None,
            );
            let workspace_lease = workspace_lease(&runner.id, &current_workspace);
            Ok((
                RunnerWorkspaceSyncOutput {
                    variant: "workspace_sync",
                    command: "runner.workspace.sync",
                    runner_id: runner.id,
                    local_path: local_path.display().to_string(),
                    remote_path,
                    current_workspace,
                    workspace_lease,
                    sync_mode: RunnerWorkspaceSyncMode::Git,
                    snapshot_identity: git.head,
                    counts: ByteFileCounts::default(),
                    excludes,
                    includes,
                    workspace_cleanliness: if options.allow_dirty_lab_workspace {
                        "dirty_remote_overwrite_allowed".to_string()
                    } else {
                        "clean_remote_required".to_string()
                    },
                    validation_dependencies,
                },
                0,
            ))
        }
    }
}

pub fn prune_workspaces(
    runner_id: &str,
    options: RunnerWorkspacePruneOptions,
) -> Result<(RunnerWorkspacePruneOutput, i32)> {
    let runner = load(runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner workspace prune requires workspace_root",
            Some(runner.id.clone()),
            Some(vec![
                "Set runner.workspace_root to the remote workspace directory.".to_string(),
            ]),
        )
    })?;
    validate_absolute_path("workspace_root", workspace_root)?;
    let lab_workspaces_root = format!("{}/_lab_workspaces", workspace_root.trim_end_matches('/'));
    let limit = options.limit.max(1);
    let candidates = match runner.kind {
        RunnerKind::Local => prune_candidates_local(Path::new(&lab_workspaces_root), &options)?,
        RunnerKind::Ssh => prune_candidates_ssh(&runner, &lab_workspaces_root, &options)?,
    };

    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    let mut candidate_entries = Vec::new();
    for candidate in candidates.into_iter().take(limit) {
        if options.apply {
            match remove_workspace(&runner, &lab_workspaces_root, &candidate.remote_path) {
                Ok(()) => removed.push(candidate),
                Err(err) => skipped.push(RunnerWorkspacePruneSkippedEntry {
                    remote_path: candidate.remote_path,
                    reason: err.to_string(),
                }),
            }
        } else {
            candidate_entries.push(candidate);
        }
    }

    let total_candidate_bytes = candidate_entries.iter().map(|entry| entry.bytes).sum();
    let total_removed_bytes = removed.iter().map(|entry| entry.bytes).sum();
    Ok((
        RunnerWorkspacePruneOutput {
            variant: "workspace_prune",
            command: "runner.workspace.prune",
            runner_id: runner.id,
            dry_run: !options.apply,
            workspace_root: workspace_root.to_string(),
            lab_workspaces_root,
            min_age_hours: options.min_age_hours,
            candidates: candidate_entries,
            removed,
            skipped,
            total_candidate_bytes,
            total_removed_bytes,
        },
        0,
    ))
}

pub fn list_workspaces(runner_id: &str, limit: usize) -> Result<(RunnerWorkspaceListOutput, i32)> {
    let runner = load(runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner workspace list requires workspace_root",
            Some(runner.id.clone()),
            Some(vec![
                "Set runner.workspace_root to the remote workspace directory.".to_string(),
            ]),
        )
    })?;
    validate_absolute_path("workspace_root", workspace_root)?;
    let lab_workspaces_root = format!("{}/_lab_workspaces", workspace_root.trim_end_matches('/'));
    let remote_paths = match runner.kind {
        RunnerKind::Local => list_local_lab_workspaces(Path::new(&lab_workspaces_root), limit)?,
        RunnerKind::Ssh => list_ssh_lab_workspaces(&runner, &lab_workspaces_root, limit)?,
    };
    let workspaces = remote_paths
        .into_iter()
        .map(|remote_path| RunnerWorkspaceListEntry {
            exec_command: format!(
                "homeboy runner exec {} --cwd {} -- <command>",
                shell_arg(&runner.id),
                shell_arg(&remote_path)
            ),
            remote_path,
        })
        .collect();

    Ok((
        RunnerWorkspaceListOutput {
            variant: "workspace_list",
            command: "runner.workspace.list",
            runner_id: runner.id,
            workspace_root: workspace_root.to_string(),
            lab_workspaces_root,
            workspaces,
        },
        0,
    ))
}

fn list_local_lab_workspaces(root: &Path, limit: usize) -> Result<Vec<String>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(root)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("list runner workspaces".to_string()))
        })?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            Some((entry.path(), modified))
        })
        .collect::<Vec<(PathBuf, Option<std::time::SystemTime>)>>();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(entries
        .into_iter()
        .take(limit)
        .map(|(path, _)| path.display().to_string())
        .collect())
}

fn list_ssh_lab_workspaces(
    runner: &super::super::Runner,
    lab_workspaces_root: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runner workspace list requires server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env.clone());
    let command = format!(
        "root={}; if [ -d \"$root\" ]; then ls -1td \"$root\"/*/ 2>/dev/null | sed 's#/$##'; fi",
        shell::quote_arg(lab_workspaces_root)
    );
    let output = client.execute(&command);
    if output.exit_code != 0 {
        return Err(Error::internal_unexpected(format!(
            "runner workspace list failed on {server_id}: {}",
            output.stderr.trim()
        )));
    }
    let paths = output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok(paths.into_iter().take(limit).collect())
}

fn workspace_metadata(
    runner_id: &str,
    local_path: &Path,
    remote_path: &str,
    sync_mode: RunnerWorkspaceSyncMode,
    snapshot_identity: &str,
) -> RunnerWorkspaceMetadata {
    RunnerWorkspaceMetadata {
        schema: "homeboy/runner-workspace/v1",
        runner_id: runner_id.to_string(),
        local_path: local_path.display().to_string(),
        remote_path: remote_path.to_string(),
        sync_mode: sync_mode.label().to_string(),
        snapshot_identity: snapshot_identity.to_string(),
        synced_at: chrono::Utc::now().to_rfc3339(),
        run_id: None,
        job_id: None,
    }
}

fn write_workspace_metadata(
    runner: &super::super::Runner,
    metadata: RunnerWorkspaceMetadata,
) -> Result<()> {
    let json = serde_json::to_string_pretty(&metadata)
        .map_err(|err| Error::internal_json(err.to_string(), None))?;
    let metadata_path = format!(
        "{}/{}",
        metadata.remote_path.trim_end_matches('/'),
        WORKSPACE_METADATA_FILE
    );
    match runner.kind {
        RunnerKind::Local => {
            let path = Path::new(&metadata_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some("create workspace metadata dir".to_string()),
                    )
                })?;
            }
            fs::write(path, json).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("write workspace metadata".to_string()),
                )
            })
        }
        RunnerKind::Ssh => {
            let (_server, mut client) = ssh_client_for_runner(runner)?;
            client.env.extend(runner.env.clone());
            let parent = parent_remote_path(&metadata_path);
            let command = format!(
                "mkdir -p {parent} && cat > {path} <<'HOMEBOY_WORKSPACE_METADATA'\n{json}\nHOMEBOY_WORKSPACE_METADATA",
                parent = shell::quote_arg(&parent),
                path = shell::quote_arg(&metadata_path),
                json = json,
            );
            let output = client.execute(&command);
            if output.success {
                Ok(())
            } else {
                Err(Error::internal_unexpected(format!(
                    "write runner workspace metadata failed: {}",
                    output.stderr.trim()
                )))
            }
        }
    }
}

fn prune_candidates_local(
    root: &Path,
    options: &RunnerWorkspacePruneOptions,
) -> Result<Vec<RunnerWorkspacePruneEntry>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read runner workspace root".to_string()),
        )
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read runner workspace entry".to_string()),
            )
        })?;
        let path = entry.path();
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some(candidate) = classify_local_candidate(root, &path, options)? {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| b.age_seconds.cmp(&a.age_seconds))
    });
    Ok(candidates)
}

fn classify_local_candidate(
    root: &Path,
    path: &Path,
    options: &RunnerWorkspacePruneOptions,
) -> Result<Option<RunnerWorkspacePruneEntry>> {
    if !path.starts_with(root) || path == root {
        return Ok(None);
    }
    let age_seconds = path_age_seconds(path)?;
    if age_seconds < options.min_age_hours.saturating_mul(3600) {
        return Ok(None);
    }
    if has_pending_apply_back_local(path) {
        return Ok(None);
    }
    let metadata_path = path.join(WORKSPACE_METADATA_FILE);
    let metadata = match fs::read_to_string(&metadata_path) {
        Ok(content) => content,
        Err(_) => return Ok(None),
    };
    let metadata: serde_json::Value = serde_json::from_str(&metadata).map_err(|err| {
        Error::internal_json(err.to_string(), Some(metadata_path.display().to_string()))
    })?;
    if metadata.get("schema").and_then(|value| value.as_str())
        != Some("homeboy/runner-workspace/v1")
    {
        return Ok(None);
    }
    let Some(source_path) = metadata.get("local_path").and_then(|value| value.as_str()) else {
        return Ok(None);
    };
    if Path::new(source_path).exists() {
        return Ok(None);
    }
    Ok(Some(RunnerWorkspacePruneEntry {
        remote_path: path.display().to_string(),
        source_path: source_path.to_string(),
        run_id: metadata
            .get("run_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        job_id: metadata
            .get("job_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        sync_mode: metadata
            .get("sync_mode")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        snapshot_identity: metadata
            .get("snapshot_identity")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        age_seconds,
        bytes: directory_size(path)?,
        reason: "source_path_missing".to_string(),
    }))
}

fn prune_candidates_ssh(
    runner: &super::super::Runner,
    root: &str,
    options: &RunnerWorkspacePruneOptions,
) -> Result<Vec<RunnerWorkspacePruneEntry>> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let min_age = options.min_age_hours.saturating_mul(3600);
    let command = format!(
        "root={root}; meta_rel={meta}; now=$(date +%s); if [ -d \"$root\" ]; then find \"$root\" -mindepth 1 -maxdepth 1 -type d -exec sh -c 'meta_rel=$1; now=$2; min_age=$3; shift 3; for dir do meta=\"$dir/$meta_rel\"; [ -f \"$meta\" ] || continue; mtime=$(stat -c %Y \"$dir\" 2>/dev/null || stat -f %m \"$dir\" 2>/dev/null || echo 0); age=$((now-mtime)); [ \"$age\" -ge \"$min_age\" ] || continue; if find \"$dir/.homeboy\" -type f \\( -name \"*.patch\" -o -name \"*.diff\" -o -name \"*patch*\" \\) 2>/dev/null | grep -q .; then continue; fi; bytes=$(du -sk \"$dir\" 2>/dev/null | awk '\''{{print $1 * 1024}}'\''); printf \"%s\\t%s\\t%s\\t\" \"$age\" \"${{bytes:-0}}\" \"$dir\"; base64 < \"$meta\" | tr -d \"\\n\"; printf \"\\n\"; done' sh {meta_arg} \"$now\" {min_age_arg} {{}} +; fi",
        root = shell::quote_arg(root),
        meta = shell::quote_arg(WORKSPACE_METADATA_FILE),
        meta_arg = shell::quote_arg(WORKSPACE_METADATA_FILE),
        min_age_arg = shell::quote_arg(&min_age.to_string()),
    );
    let output = client.execute(&command);
    if !output.success {
        return Err(Error::internal_unexpected(format!(
            "runner workspace prune scan failed: {}",
            output.stderr.trim()
        )));
    }
    let mut candidates = Vec::new();
    for line in output.stdout.lines() {
        let parts = line.splitn(4, '\t').collect::<Vec<_>>();
        if parts.len() != 4 {
            continue;
        }
        let age_seconds = parts[0].parse::<u64>().unwrap_or(0);
        let bytes = parts[1].parse::<u64>().unwrap_or(0);
        let remote_path = parts[2].to_string();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(parts[3])
            .map_err(|err| Error::internal_json(err.to_string(), None))?;
        let metadata: serde_json::Value = serde_json::from_slice(&decoded)
            .map_err(|err| Error::internal_json(err.to_string(), Some(remote_path.clone())))?;
        let Some(source_path) = metadata.get("local_path").and_then(|value| value.as_str()) else {
            continue;
        };
        if Path::new(source_path).exists() {
            continue;
        }
        candidates.push(RunnerWorkspacePruneEntry {
            remote_path,
            source_path: source_path.to_string(),
            run_id: metadata
                .get("run_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            job_id: metadata
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            sync_mode: metadata
                .get("sync_mode")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            snapshot_identity: metadata
                .get("snapshot_identity")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            age_seconds,
            bytes,
            reason: "source_path_missing".to_string(),
        });
    }
    candidates.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| b.age_seconds.cmp(&a.age_seconds))
    });
    Ok(candidates)
}

fn remove_workspace(runner: &super::super::Runner, root: &str, remote_path: &str) -> Result<()> {
    let root_path = Path::new(root);
    let path = Path::new(remote_path);
    if !path.starts_with(root_path) || path == root_path || remote_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "remote_path",
            "refusing to remove runner workspace outside _lab_workspaces root",
            Some(remote_path.to_string()),
            None,
        ));
    }
    match runner.kind {
        RunnerKind::Local => fs::remove_dir_all(path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("remove runner workspace".to_string()))
        }),
        RunnerKind::Ssh => {
            let (_server, mut client) = ssh_client_for_runner(runner)?;
            client.env.extend(runner.env.clone());
            let command = format!(
                "root={root}; path={path}; case \"$path\" in \"$root\"/*) [ \"$path\" != \"$root\" ] && rm -rf -- \"$path\" ;; *) echo refused >&2; exit 2 ;; esac",
                root = shell::quote_arg(root),
                path = shell::quote_arg(remote_path),
            );
            let output = client.execute(&command);
            if output.success {
                Ok(())
            } else {
                Err(Error::internal_unexpected(format!(
                    "remove runner workspace failed: {}",
                    output.stderr.trim()
                )))
            }
        }
    }
}

fn path_age_seconds(path: &Path) -> Result<u64> {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read workspace mtime".to_string()))
        })?;
    Ok(SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        .as_secs())
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read workspace size".to_string()))
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read workspace size entry".to_string()),
            )
        })?;
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read workspace size metadata".to_string()),
            )
        })?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn has_pending_apply_back_local(path: &Path) -> bool {
    let homeboy = path.join(".homeboy");
    let Ok(entries) = fs::read_dir(homeboy) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        name.contains("patch") || name.ends_with(".patch") || name.ends_with(".diff")
    })
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn workspace_lease(
    runner_id: &str,
    current: &RunnerWorkspaceCurrentSummary,
) -> RunnerWorkspaceLease {
    RunnerWorkspaceLease {
        runner_id: runner_id.to_string(),
        local_path: current.local_path.clone(),
        remote_path: current.remote_path.clone(),
        sync_mode: current.sync_mode.label().to_string(),
        materialized: current.materialized,
        lifecycle_owner: RunnerLifecycleOwner::Controller,
        source_commit: current.source_commit.clone(),
        source_ref: current.source_ref.clone(),
        source_dirty: current.source_dirty,
    }
}

fn current_workspace_summary(
    local_path: &Path,
    remote_path: &str,
    sync_mode: RunnerWorkspaceSyncMode,
    materialized: bool,
    synthetic_checkout_commit: Option<String>,
) -> RunnerWorkspaceCurrentSummary {
    let git_state = local_git_state(local_path);
    RunnerWorkspaceCurrentSummary {
        local_path: local_path.display().to_string(),
        remote_path: remote_path.to_string(),
        sync_mode,
        materialized,
        source_commit: git_state.commit,
        source_ref: git_state.ref_name,
        source_dirty: git_state.dirty,
        synthetic_checkout_commit,
    }
}

fn local_git_state(local_path: &Path) -> LocalGitState {
    let commit = git_output(local_path, &["rev-parse", "HEAD"]).ok();
    let ref_name = git_output(local_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|value| value != "HEAD");
    let dirty = git_output(local_path, &["status", "--porcelain=v1"])
        .ok()
        .map(|status| !status.trim().is_empty());

    LocalGitState {
        commit,
        ref_name,
        dirty,
    }
}
