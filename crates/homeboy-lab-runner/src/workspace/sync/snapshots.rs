//! Runner-workspace listing and snapshot inspection.
//!
//! Lists lab workspaces (local and over SSH) and builds the snapshot view —
//! per-workspace metadata entries with applied filters. Extracted from the
//! `workspace::sync` module to separate read-only inspection from the
//! materialization/prune/metadata write paths.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::server::{self, SshClient};

use super::super::super::{load, RunnerKind};
use super::super::types::{
    RunnerWorkspaceListEntry, RunnerWorkspaceListOutput, RunnerWorkspaceMetadata,
    RunnerWorkspaceSnapshotAppliedFilters, RunnerWorkspaceSnapshotEntry,
    RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSnapshotInvalidMetadata,
    RunnerWorkspaceSnapshotsOutput,
};
use super::super::util::{ssh_client_for_runner, validate_absolute_path};
use super::{shell_arg, workspace_repo_from_path, WORKSPACE_METADATA_FILE};

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
                "homeboy runner exec --cwd {} {} -- <command>",
                shell_arg(&remote_path),
                shell_arg(&runner.id)
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

pub fn workspace_snapshots(
    runner_id: &str,
    filters: RunnerWorkspaceSnapshotFilters,
) -> Result<(RunnerWorkspaceSnapshotsOutput, i32)> {
    let runner = load(runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner workspace snapshots requires workspace_root",
            Some(runner.id.clone()),
            Some(vec![
                "Set runner.workspace_root to the remote workspace directory.".to_string(),
            ]),
        )
    })?;
    validate_absolute_path("workspace_root", workspace_root)?;
    let lab_workspaces_root = format!("{}/_lab_workspaces", workspace_root.trim_end_matches('/'));
    let limit = filters.limit.max(1);
    let (mut snapshots, skipped_invalid_metadata) = match runner.kind {
        RunnerKind::Local => workspace_snapshots_local(Path::new(&lab_workspaces_root))?,
        RunnerKind::Ssh => workspace_snapshots_ssh(&runner, &lab_workspaces_root)?,
    };
    snapshots.retain(|snapshot| workspace_snapshot_matches(snapshot, &filters));
    snapshots.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.remote_path.cmp(&b.remote_path))
    });
    snapshots.truncate(limit);

    Ok((
        RunnerWorkspaceSnapshotsOutput {
            variant: "workspace_snapshots",
            command: "runner.workspace.snapshots",
            runner_id: runner.id,
            workspace_root: workspace_root.to_string(),
            lab_workspaces_root,
            filters: RunnerWorkspaceSnapshotAppliedFilters {
                repo: filters.repo,
                source_ref: filters.source_ref,
                source_commit: filters.source_commit,
                run_id: filters.run_id,
                limit,
            },
            snapshots,
            skipped_invalid_metadata,
        },
        0,
    ))
}

/// A local runner workspace directory is reusable unless it is a partial git
/// checkout — a `.git` directory whose `HEAD` does not resolve. A cancelled or
/// timed-out git materialization can leave exactly that (see #8886), and a
/// staging `.tmp.$$` directory that survived a crash mid-clone is caught the
/// same way. Non-git directories (e.g. snapshot workspaces) remain reusable.
fn local_workspace_is_reusable(path: &Path) -> bool {
    if !path.join(".git").exists() {
        return true;
    }
    std::process::Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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
            // Never advertise a partial checkout as reusable. A cancelled or
            // timed-out git materialization can leave a directory (or a leftover
            // `.tmp.$$` staging path) that has no valid HEAD; exec-ing against it
            // fails with "ambiguous argument 'HEAD'" (#8886). A directory is
            // only reusable if it is not a git checkout at all, or is one whose
            // HEAD resolves.
            let path = entry.path();
            if !local_workspace_is_reusable(&path) {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            Some((path, modified))
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
    runner: &super::super::super::Runner,
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
    // Mirror the local reusability rule (#8886): advertise a directory only if
    // it is not a git checkout, or is one whose HEAD resolves. A cancelled/
    // timed-out clone (or leftover `.tmp.$$` staging dir) with an unresolved
    // HEAD must not be listed as reusable.
    let command = format!(
        "root={}; if [ -d \"$root\" ]; then ls -1td \"$root\"/*/ 2>/dev/null | sed 's#/$##' | while IFS= read -r ws; do if [ ! -d \"$ws/.git\" ] || git -C \"$ws\" rev-parse --verify -q HEAD >/dev/null 2>&1; then printf '%s\\n' \"$ws\"; fi; done; fi",
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

fn workspace_snapshots_local(
    root: &Path,
) -> Result<(
    Vec<RunnerWorkspaceSnapshotEntry>,
    Vec<RunnerWorkspaceSnapshotInvalidMetadata>,
)> {
    if !root.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut snapshots = Vec::new();
    let mut skipped_invalid_metadata = Vec::new();
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
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let metadata_path = entry.path().join(WORKSPACE_METADATA_FILE);
        let Ok(content) = fs::read_to_string(&metadata_path) else {
            continue;
        };
        let metadata: RunnerWorkspaceMetadata = match serde_json::from_str(&content) {
            Ok(metadata) => metadata,
            Err(error) => {
                skipped_invalid_metadata.push(invalid_workspace_metadata(
                    &metadata_path.display().to_string(),
                    error,
                ));
                continue;
            }
        };
        if let Some(snapshot) = workspace_snapshot_entry(metadata) {
            snapshots.push(snapshot);
        }
    }
    Ok((snapshots, skipped_invalid_metadata))
}

fn workspace_snapshots_ssh(
    runner: &super::super::super::Runner,
    root: &str,
) -> Result<(
    Vec<RunnerWorkspaceSnapshotEntry>,
    Vec<RunnerWorkspaceSnapshotInvalidMetadata>,
)> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let command = workspace_snapshot_scan_command(root);
    let output = client.execute(&command);
    if !output.success {
        return Err(Error::internal_unexpected(format!(
            "runner workspace snapshot scan failed: {}",
            output.stderr.trim()
        )));
    }
    let mut snapshots = Vec::new();
    let mut skipped_invalid_metadata = Vec::new();
    for line in output.stdout.lines() {
        let parts = line.splitn(2, '\t').collect::<Vec<_>>();
        if parts.len() != 2 {
            continue;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(parts[1])
            .map_err(|error| invalid_workspace_metadata(parts[0], error));
        let Ok(decoded) = decoded else {
            skipped_invalid_metadata.push(decoded.expect_err("base64 decode failed"));
            continue;
        };
        let metadata: RunnerWorkspaceMetadata = match serde_json::from_slice(&decoded) {
            Ok(metadata) => metadata,
            Err(error) => {
                skipped_invalid_metadata.push(invalid_workspace_metadata(parts[0], error));
                continue;
            }
        };
        if let Some(snapshot) = workspace_snapshot_entry(metadata) {
            snapshots.push(snapshot);
        }
    }
    Ok((snapshots, skipped_invalid_metadata))
}

pub(super) fn workspace_snapshot_for_lease(
    runner: &super::super::super::Runner,
    root: &str,
    lease: &str,
) -> Result<Option<RunnerWorkspaceSnapshotEntry>> {
    match runner.kind {
        RunnerKind::Local => workspace_snapshot_for_lease_local(Path::new(root), lease),
        RunnerKind::Ssh => workspace_snapshot_for_lease_ssh(runner, root, lease),
    }
}

fn workspace_snapshot_for_lease_local(
    root: &Path,
    lease: &str,
) -> Result<Option<RunnerWorkspaceSnapshotEntry>> {
    if !root.is_dir() {
        return Ok(None);
    }
    for entry in fs::read_dir(root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read runner workspace root".to_string()),
        )
    })? {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(content) = fs::read_to_string(entry.path().join(WORKSPACE_METADATA_FILE)) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_str::<RunnerWorkspaceMetadata>(&content) else {
            continue;
        };
        if metadata.workspace_lease.as_deref() == Some(lease) {
            return Ok(workspace_snapshot_entry(metadata));
        }
    }
    Ok(None)
}

fn workspace_snapshot_for_lease_ssh(
    runner: &super::super::super::Runner,
    root: &str,
    lease: &str,
) -> Result<Option<RunnerWorkspaceSnapshotEntry>> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let output = client.execute(&workspace_snapshot_lease_command(root, lease));
    if !output.success || output.stdout.trim().is_empty() {
        return Ok(None);
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(output.stdout.trim()) {
        Ok(decoded) => decoded,
        Err(_) => return Ok(None),
    };
    let metadata = match serde_json::from_slice::<RunnerWorkspaceMetadata>(&decoded) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(None),
    };
    Ok((metadata.workspace_lease.as_deref() == Some(lease))
        .then(|| workspace_snapshot_entry(metadata))
        .flatten())
}

fn invalid_workspace_metadata(
    source: &str,
    error: impl std::fmt::Display,
) -> RunnerWorkspaceSnapshotInvalidMetadata {
    const MAX_PARSE_ERROR_BYTES: usize = 512;

    let error = error.to_string();
    let error = if error.len() > MAX_PARSE_ERROR_BYTES {
        let mut end = MAX_PARSE_ERROR_BYTES;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... [truncated]", &error[..end])
    } else {
        error
    };
    RunnerWorkspaceSnapshotInvalidMetadata {
        source: source.to_string(),
        field: WORKSPACE_METADATA_FILE,
        error,
    }
}

pub(crate) fn workspace_snapshot_scan_command(root: &str) -> String {
    // Promotion replaces a temporary child atomically. `find` reports a
    // disappeared child as an error even though the snapshot root remains
    // valid, so read each candidate defensively and verify the root afterwards.
    format!(
        "root={root}; meta_rel={meta}; if [ -d \"$root\" ]; then for dir in \"$root\"/*; do [ -d \"$dir\" ] || continue; meta=\"$dir/$meta_rel\"; [ -f \"$meta\" ] || continue; encoded=$(base64 < \"$meta\" 2>/dev/null) || continue; encoded=$(printf '%s' \"$encoded\" | tr -d '\\n'); printf \"%s\\t%s\\n\" \"$dir\" \"$encoded\"; done; [ -d \"$root\" ] || {{ printf '%s\\n' \"runner workspace snapshot root disappeared during scan: $root\" >&2; exit 1; }}; fi",
        root = shell::quote_arg(root),
        meta = shell::quote_arg(WORKSPACE_METADATA_FILE),
    )
}

fn workspace_snapshot_lease_command(root: &str, lease: &str) -> String {
    let lease = serde_json::to_string(lease).expect("serialize workspace lease");
    let needle = format!("\"workspace_lease\":{lease}");
    format!(
        "root={root}; meta_rel={meta}; needle={needle}; if [ -d \"$root\" ]; then for dir in \"$root\"/*; do meta=\"$dir/$meta_rel\"; [ -f \"$meta\" ] || continue; tr -d '[:space:]' < \"$meta\" | grep -Fq -- \"$needle\" || continue; base64 < \"$meta\" 2>/dev/null | tr -d '\\n'; exit 0; done; fi",
        root = shell::quote_arg(root),
        meta = shell::quote_arg(WORKSPACE_METADATA_FILE),
        needle = shell::quote_arg(&needle),
    )
}

fn workspace_snapshot_entry(
    metadata: RunnerWorkspaceMetadata,
) -> Option<RunnerWorkspaceSnapshotEntry> {
    if metadata.schema != "homeboy/runner-workspace/v1" {
        return None;
    }
    let repo = metadata
        .repo
        .clone()
        .unwrap_or_else(|| workspace_repo_from_path(&metadata.local_path));
    Some(RunnerWorkspaceSnapshotEntry {
        exec_command: format!(
            "homeboy runner exec --cwd {} {} -- <command>",
            shell_arg(&metadata.remote_path),
            shell_arg(&metadata.runner_id)
        ),
        runner_id: metadata.runner_id,
        repo,
        local_path: metadata.local_path,
        remote_path: metadata.remote_path,
        sync_mode: metadata.sync_mode,
        actual_materialization_mode: metadata.actual_materialization_mode,
        fallback_reason: metadata.fallback_reason,
        snapshot_identity: metadata.snapshot_identity,
        workspace_lease: metadata.workspace_lease,
        workspace_generation: metadata.workspace_generation,
        original_prepared_snapshot_identity: metadata.original_prepared_snapshot_identity,
        update_lineage: metadata.update_lineage,
        snapshot_excludes: metadata.snapshot_excludes,
        content_manifest: metadata.content_manifest,
        created_at: metadata.synced_at,
        source_ref: metadata.source_ref,
        source_commit: metadata.source_commit,
        source_remote_url: metadata.source_remote_url,
        source_dirty: metadata.source_dirty,
        run_id: metadata.run_id,
        job_id: metadata.job_id,
        resource_lifecycle: metadata.resource_lifecycle,
    })
}

fn workspace_snapshot_matches(
    snapshot: &RunnerWorkspaceSnapshotEntry,
    filters: &RunnerWorkspaceSnapshotFilters,
) -> bool {
    if let Some(repo) = filters.repo.as_deref() {
        if snapshot.repo != repo {
            return false;
        }
    }
    if let Some(source_ref) = filters.source_ref.as_deref() {
        if snapshot.source_ref.as_deref() != Some(source_ref) {
            return false;
        }
    }
    if let Some(source_commit) = filters.source_commit.as_deref() {
        if snapshot.source_commit.as_deref() != Some(source_commit) {
            return false;
        }
    }
    if let Some(run_id) = filters.run_id.as_deref() {
        if snapshot.run_id.as_deref() != Some(run_id) {
            return false;
        }
    }
    true
}
