use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use base64::Engine;

use homeboy_core::engine::temp;
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::resource_lifecycle_index::{
    resource_lifecycle_path_ttl_expired_at, ResourceCleanupPolicy, ResourceEvidenceRetention,
    ResourceLifecycle, ResourceLifecycleRecord, ResourceLifecycleResourceStatus,
};

use super::super::validation_dependencies::{
    sync_validation_dependency_workspaces, RunnerValidationDependencySyncOutput,
};
use super::super::{
    load, source_materialization, RunnerKind, RunnerLifecycleOwner, RunnerWorkspaceLease,
};
use super::git::{
    git_snapshot, materialize_git, materialize_git_from_controller_bundle,
    materialize_git_snapshot_from_controller_bundle,
};
use super::snapshot::{
    effective_snapshot_excludes, ensure_no_runner_workspace_metadata_collision,
    local_snapshot_stats, materialize_snapshot, materialize_snapshot_git,
    materialize_snapshot_incremental, snapshot_identity, snapshot_manifest_delta,
    workspace_content_manifest_for_policy, SnapshotManifestDelta,
    WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
};
use super::types::{
    canonical_workspace_path, ByteFileCounts, LocalGitState, RunnerWorkspaceCurrentSummary,
    RunnerWorkspaceMaterializationPlan, RunnerWorkspaceMetadata, RunnerWorkspacePruneEntry,
    RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry,
    RunnerWorkspaceSnapshotEntry, RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput, DEFAULT_EXCLUDES,
};
use super::util::{
    deterministic_remote_path, git_output, parent_remote_path, ssh_client_for_runner,
    validate_absolute_path,
};
use homeboy_core::engine::shell;
use homeboy_core::server::{is_transient_ssh_error, CommandOutput};

mod snapshots;
#[cfg(test)]
pub(crate) use snapshots::workspace_snapshot_scan_command;
pub use snapshots::{list_workspaces, workspace_snapshots};

pub(crate) const WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";
const MIN_RUNNER_WORKSPACE_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_RUNNER_WORKSPACE_FREE_RATIO: f64 = 0.01;
const METADATA_SSH_RECOVERY_ATTEMPTS: usize = 2;
const WORKSPACE_METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const WORKSPACE_METADATA_OUTPUT_LIMIT: usize = 4 * 1024;

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
    require_runner_workspace_disk_headroom(&runner, workspace_root)?;

    let mut excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for pattern in &runner.policy.snapshot_excludes {
        if !excludes.contains(pattern) {
            excludes.push(pattern.clone());
        }
    }
    for pattern in homeboy_core::source_snapshot::declared_sync_excludes_for_path(&local_path) {
        if !excludes.contains(&pattern) {
            excludes.push(pattern);
        }
    }
    let mut includes = runner.policy.snapshot_includes.clone();
    for pattern in &options.snapshot_includes {
        if !includes.contains(pattern) {
            includes.push(pattern.clone());
        }
    }
    let excludes = effective_snapshot_excludes(excludes, &includes);

    match options.mode {
        RunnerWorkspaceSyncMode::Snapshot | RunnerWorkspaceSyncMode::SnapshotGit => {
            ensure_no_runner_workspace_metadata_collision(&local_path)?;
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
            let workspace_cleanliness = if options.mode == RunnerWorkspaceSyncMode::SnapshotGit {
                "snapshot_synthetic_git_unique_workspace"
            } else {
                "snapshot_unique_workspace"
            };
            let mut materialization_plan = workspace_materialization_plan(
                workspace_root,
                &local_path,
                &remote_path,
                &snapshot,
                &options,
                &includes,
                workspace_cleanliness,
            );
            let stats = local_snapshot_stats(&local_path, &excludes, &includes)?;
            let content_manifest = workspace_content_manifest_for_policy(
                &local_path,
                &excludes,
                WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
            )?;
            let git_backed_snapshot = git_output(&local_path, &["rev-parse", "HEAD"]).is_ok();
            let synthetic_checkout = if options.mode == RunnerWorkspaceSyncMode::SnapshotGit
                && git_backed_snapshot
            {
                match materialize_git_snapshot_from_controller_bundle(
                    &runner,
                    &local_path,
                    &remote_path,
                    &excludes,
                ) {
                    Ok(provenance) => {
                        materialization_plan.controller_git_bundle = provenance;
                        None
                    }
                    Err(error) => {
                        rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                        return Err(error);
                    }
                }
            } else if options.mode == RunnerWorkspaceSyncMode::SnapshotGit {
                match materialize_snapshot_git(
                    &runner,
                    &local_path,
                    &remote_path,
                    &excludes,
                    &snapshot,
                ) {
                    Ok(identity) => Some(identity),
                    Err(error) => {
                        rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                        return Err(error);
                    }
                }
            } else {
                let seed = compatible_incremental_snapshot(
                    &runner,
                    &local_path,
                    &excludes,
                    &content_manifest,
                )?;
                materialization_plan.snapshot_transfer = Some(match seed {
                    Some((seed, delta)) => match materialize_snapshot_incremental(
                        &runner,
                        &local_path,
                        &remote_path,
                        &seed.remote_path,
                        &excludes,
                        &delta,
                    ) {
                        Ok(transfer) => transfer,
                        Err(error) => {
                            rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                            return Err(error);
                        }
                    },
                    None => {
                        if let Err(error) =
                            materialize_snapshot(&runner, &local_path, &remote_path, &excludes)
                        {
                            rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                            return Err(error);
                        }
                        super::types::SnapshotTransferStats {
                            reused: ByteFileCounts::default(),
                            transferred: stats.clone(),
                            final_size: stats.clone(),
                        }
                    }
                });
                None
            };
            if options.mode == RunnerWorkspaceSyncMode::SnapshotGit && git_backed_snapshot {
                // Snapshot-git deliberately retains Git metadata for callers
                // that need a checkout baseline.
                materialization_plan.actual_materialization_mode =
                    Some(RunnerWorkspaceSyncMode::SnapshotGit.label().to_string());
            } else if options.mode == RunnerWorkspaceSyncMode::Snapshot {
                // Plain snapshot mode is a readable filesystem transfer. It
                // must not require a Git object closure from partial clones.
                materialization_plan.actual_materialization_mode =
                    Some("filesystem_snapshot".to_string());
            }
            let metadata = workspace_metadata(
                &runner.id,
                &local_path,
                &remote_path,
                options.mode,
                materialization_plan.actual_materialization_mode.as_deref(),
                &snapshot,
                &excludes,
                Some(content_manifest),
                options.run_isolation_token.as_deref(),
                ResourceCleanupPolicy::DeleteOnSuccess,
            );
            let resource_lifecycle = metadata.resource_lifecycle.clone().unwrap_or_else(|| {
                workspace_resource_lifecycle(
                    &runner.id,
                    &remote_path,
                    None,
                    ResourceCleanupPolicy::DeleteOnSuccess,
                )
            });
            let validation_dependencies = match write_metadata_and_sync_validation_dependencies(
                &runner,
                metadata,
                &local_path,
                &remote_path,
                &excludes,
            ) {
                Ok(dependencies) => dependencies,
                Err(err) => {
                    rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                    return Err(err);
                }
            };
            let current_workspace = current_workspace_summary(
                &local_path,
                &remote_path,
                options.mode,
                true,
                synthetic_checkout,
            );
            let workspace_lease = workspace_lease(&runner.id, &current_workspace);
            Ok((
                RunnerWorkspaceSyncOutput {
                    variant: "workspace_sync",
                    command: "runner.workspace.sync",
                    runner_id: runner.id,
                    local_path: local_path.display().to_string(),
                    remote_path,
                    materialization_plan,
                    current_workspace,
                    workspace_lease,
                    resource_lifecycle,
                    sync_mode: options.mode,
                    snapshot_identity: snapshot,
                    counts: stats,
                    excludes,
                    includes,
                    workspace_cleanliness: workspace_cleanliness.to_string(),
                    validation_dependencies,
                },
                0,
            ))
        }
        RunnerWorkspaceSyncMode::Git => {
            let git = git_snapshot(
                &local_path,
                options.changed_since_base.as_deref(),
                options.git_fetch_refs.clone(),
                options.controller_routed_git,
            )?;
            let remote_path = deterministic_remote_path(
                workspace_root,
                &local_path,
                &git.head,
                options.run_isolation_token.as_deref(),
            );
            let workspace_cleanliness = if options.allow_dirty_lab_workspace {
                "dirty_remote_overwrite_allowed"
            } else {
                "clean_remote_required"
            };
            let mut materialization_plan = workspace_materialization_plan(
                workspace_root,
                &local_path,
                &remote_path,
                &git.head,
                &options,
                &includes,
                workspace_cleanliness,
            );
            if options.controller_routed_git
                || git.branch.is_none()
                || source_materialization::requires_controller_routed_workspace_sync(
                    &git.remote_url,
                )
            {
                materialization_plan.controller_git_bundle =
                    Some(materialize_git_from_controller_bundle(
                        &runner,
                        &local_path,
                        &remote_path,
                        &git.head,
                        git.branch.as_deref(),
                        &git.remote_url,
                        git.changed_since_base.as_deref(),
                        &git.git_fetch_refs,
                        options.allow_dirty_lab_workspace,
                    )?);
            } else {
                if runner.kind != RunnerKind::Local {
                    source_materialization::validate_runner_git_materialization(
                        &git.remote_url,
                        &runner.id,
                    )?;
                }
                if let Err(error) = materialize_git(
                    &runner,
                    &remote_path,
                    &git.remote_url,
                    &git.head,
                    git.branch.as_deref(),
                    git.changed_since_base.as_deref(),
                    &git.git_fetch_refs,
                    options.allow_dirty_lab_workspace,
                ) {
                    if !is_runner_git_auth_or_network_failure(&error) {
                        return Err(error);
                    }
                    materialization_plan.controller_git_bundle =
                        Some(materialize_git_from_controller_bundle(
                            &runner,
                            &local_path,
                            &remote_path,
                            &git.head,
                            git.branch.as_deref(),
                            &git.remote_url,
                            git.changed_since_base.as_deref(),
                            &git.git_fetch_refs,
                            options.allow_dirty_lab_workspace,
                        )?);
                }
            }
            let metadata = workspace_metadata(
                &runner.id,
                &local_path,
                &remote_path,
                RunnerWorkspaceSyncMode::Git,
                None,
                &git.head,
                &excludes,
                None,
                options.run_isolation_token.as_deref(),
                ResourceCleanupPolicy::DeleteOnSuccess,
            );
            let resource_lifecycle = metadata.resource_lifecycle.clone().unwrap_or_else(|| {
                workspace_resource_lifecycle(
                    &runner.id,
                    &remote_path,
                    None,
                    ResourceCleanupPolicy::DeleteOnSuccess,
                )
            });
            let validation_dependencies = match write_metadata_and_sync_validation_dependencies(
                &runner,
                metadata,
                &local_path,
                &remote_path,
                &excludes,
            ) {
                Ok(dependencies) => dependencies,
                Err(err) => {
                    rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                    return Err(err);
                }
            };
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
                    materialization_plan,
                    current_workspace,
                    workspace_lease,
                    resource_lifecycle,
                    sync_mode: RunnerWorkspaceSyncMode::Git,
                    snapshot_identity: git.head,
                    counts: ByteFileCounts::default(),
                    excludes,
                    includes,
                    workspace_cleanliness: workspace_cleanliness.to_string(),
                    validation_dependencies,
                },
                0,
            ))
        }
    }
}

/// Return a previously materialized source snapshot only when it is tied to the
/// exact clean controller checkout now being dispatched. This lets callers that
/// already hold a runner snapshot avoid reopening Git transport (and its object
/// closure) merely to hand the same source to a provider.
pub fn reuse_compatible_snapshot_workspace(
    runner_id: &str,
    options: &RunnerWorkspaceSyncOptions,
) -> Result<Option<RunnerWorkspaceSyncOutput>> {
    if options.changed_since_base.is_some() || !options.git_fetch_refs.is_empty() {
        return Ok(None);
    }

    let runner = load(runner_id)?;
    let local_path = canonical_workspace_path(&options.path)?;
    let source_commit = git_output(&local_path, &["rev-parse", "HEAD"]).ok();
    let source_dirty = git_output(&local_path, &["status", "--porcelain=v1"])
        .ok()
        .map(|status| !status.trim().is_empty());
    let Some(source_commit) = source_commit else {
        return Ok(None);
    };
    if source_dirty != Some(false) {
        return Ok(None);
    }

    let (snapshots, _) = workspace_snapshots(
        runner_id,
        RunnerWorkspaceSnapshotFilters {
            limit: usize::MAX,
            ..Default::default()
        },
    )?;
    let local_path_string = local_path.display().to_string();
    let Some(snapshot) = snapshots.snapshots.into_iter().find(|snapshot| {
        snapshot.sync_mode == RunnerWorkspaceSyncMode::Snapshot.label()
            && snapshot.local_path == local_path_string
            && snapshot.source_commit.as_deref() == Some(source_commit.as_str())
            && snapshot.source_dirty == Some(false)
    }) else {
        return Ok(None);
    };

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
    let mut excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for pattern in &runner.policy.snapshot_excludes {
        if !excludes.contains(pattern) {
            excludes.push(pattern.clone());
        }
    }
    for pattern in homeboy_core::source_snapshot::declared_sync_excludes_for_path(&local_path) {
        if !excludes.contains(&pattern) {
            excludes.push(pattern);
        }
    }
    let mut includes = runner.policy.snapshot_includes.clone();
    for pattern in &options.snapshot_includes {
        if !includes.contains(pattern) {
            includes.push(pattern.clone());
        }
    }
    let excludes = effective_snapshot_excludes(excludes, &includes);
    let workspace_cleanliness = "snapshot_reused_clean_workspace";
    let mut snapshot_options = options.clone();
    snapshot_options.mode = RunnerWorkspaceSyncMode::Snapshot;
    snapshot_options.controller_routed_git = false;
    let mut materialization_plan = workspace_materialization_plan(
        workspace_root,
        &local_path,
        &snapshot.remote_path,
        &snapshot.snapshot_identity,
        &snapshot_options,
        &includes,
        workspace_cleanliness,
    );
    materialization_plan.actual_materialization_mode = snapshot.actual_materialization_mode;
    let current_workspace = RunnerWorkspaceCurrentSummary {
        local_path: local_path_string.clone(),
        remote_path: snapshot.remote_path.clone(),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        materialized: true,
        source_commit: snapshot.source_commit.clone(),
        source_ref: snapshot.source_ref.clone(),
        source_dirty: snapshot.source_dirty,
        synthetic_checkout_commit: None,
        synthetic_checkout_ref: None,
        synthetic_checkout_tree: None,
    };
    let resource_lifecycle = snapshot.resource_lifecycle.unwrap_or_else(|| {
        workspace_resource_lifecycle(
            &runner.id,
            &snapshot.remote_path,
            None,
            ResourceCleanupPolicy::DeleteOnSuccess,
        )
    });

    Ok(Some(RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: runner.id.clone(),
        local_path: local_path_string,
        remote_path: snapshot.remote_path,
        materialization_plan,
        workspace_lease: workspace_lease(&runner.id, &current_workspace),
        current_workspace,
        resource_lifecycle,
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: snapshot.snapshot_identity,
        counts: ByteFileCounts::default(),
        excludes,
        includes,
        workspace_cleanliness: workspace_cleanliness.to_string(),
        validation_dependencies: Vec::new(),
    }))
}

/// A seed may differ in source revision, but must have been materialized from
/// the same controller path under the exact effective security/exclude policy.
/// Older metadata lacks that policy and is deliberately ineligible.
fn compatible_incremental_snapshot(
    runner: &super::super::Runner,
    local_path: &Path,
    excludes: &[String],
    controller_manifest: &super::snapshot::WorkspaceContentManifest,
) -> Result<Option<(RunnerWorkspaceSnapshotEntry, SnapshotManifestDelta)>> {
    let (snapshots, _) = workspace_snapshots(
        &runner.id,
        RunnerWorkspaceSnapshotFilters {
            limit: usize::MAX,
            ..Default::default()
        },
    )?;
    let local_path = local_path.display().to_string();
    Ok(snapshots.snapshots.into_iter().find_map(|snapshot| {
        (snapshot.sync_mode == RunnerWorkspaceSyncMode::Snapshot.label()
            && snapshot.local_path == local_path
            && snapshot.snapshot_excludes == excludes)
            .then(|| {
                let manifest = snapshot.content_manifest.clone()?;
                snapshot_manifest_delta(controller_manifest, &manifest)
                    .ok()
                    .map(|delta| (snapshot, delta))
            })
            .flatten()
    }))
}

fn is_runner_git_auth_or_network_failure(error: &Error) -> bool {
    let details = error.details.to_string();
    let evidence = std::iter::once(error.message.as_str())
        .chain(error.hints.iter().map(|hint| hint.message.as_str()))
        .chain(std::iter::once(details.as_str()))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("\n");
    [
        "authentication failed",
        "permission denied",
        "could not read from remote repository",
        "repository not found",
        "failed to connect",
        "could not resolve host",
        "network is unreachable",
        "connection timed out",
        "connection refused",
        "proxy",
    ]
    .iter()
    .any(|needle| evidence.contains(needle))
}

pub(crate) fn workspace_materialization_plan(
    workspace_root: &str,
    local_path: &Path,
    remote_path: &str,
    identity: &str,
    options: &RunnerWorkspaceSyncOptions,
    snapshot_includes: &[String],
    workspace_cleanliness: &str,
) -> RunnerWorkspaceMaterializationPlan {
    let local_path_string = local_path.display().to_string();
    let local_basename = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace")
        .to_string();
    let path_strategy = "workspace_root_lab_workspaces_sanitized_basename_identity_digest";
    RunnerWorkspaceMaterializationPlan::from_sync_options(
        workspace_root,
        &local_path_string,
        &local_basename,
        remote_path,
        identity,
        path_strategy,
        options,
        snapshot_includes,
        workspace_cleanliness,
    )
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
    let passes = if options.apply {
        options.passes.max(1)
    } else {
        1
    };
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    let mut candidate_entries = Vec::new();
    let mut total_candidate_count = 0;
    let mut total_candidate_bytes = 0;
    for pass in 0..passes {
        let candidates = prune_candidates_for_runner(&runner, &lab_workspaces_root, &options)?;
        if pass == 0 {
            total_candidate_count = candidates.len();
            total_candidate_bytes = candidates.iter().map(|entry| entry.bytes).sum();
        }
        if candidates.is_empty() {
            break;
        }
        for candidate in candidates.into_iter().take(limit) {
            if !options.apply {
                candidate_entries.push(candidate);
                continue;
            }
            match remove_workspace(&runner, &lab_workspaces_root, &candidate.remote_path) {
                Ok(()) => removed.push(candidate),
                Err(err) => skipped.push(RunnerWorkspacePruneSkippedEntry {
                    remote_path: candidate.remote_path,
                    reason: err.to_string(),
                }),
            }
        }
        if !options.apply || total_candidate_count <= limit {
            break;
        }
    }

    let remaining_candidates = if options.apply {
        prune_candidates_for_runner(&runner, &lab_workspaces_root, &options)?
    } else {
        prune_candidates_for_runner(&runner, &lab_workspaces_root, &options)?
            .into_iter()
            .skip(limit)
            .collect()
    };
    let remaining_candidate_count = remaining_candidates.len();
    let remaining_candidate_bytes = remaining_candidates.iter().map(|entry| entry.bytes).sum();
    let has_more = remaining_candidate_count > 0;
    let runner_arg = shell_arg(&runner.id);
    let next_command = has_more.then(|| {
        if options.apply {
            format!(
                "homeboy runner workspace prune {runner_arg} --apply --min-age-hours {} --limit {limit} --passes {passes}",
                options.min_age_hours
            )
        } else {
            format!(
                "homeboy runner workspace prune {runner_arg} --min-age-hours {} --limit {limit}",
                options.min_age_hours
            )
        }
    });
    let drain_command = format!(
        "homeboy runner workspace prune {runner_arg} --apply --min-age-hours {} --limit {limit} --passes 10",
        options.min_age_hours
    );
    let total_removed_bytes = removed.iter().map(|entry| entry.bytes).sum();
    let runner_id = runner.id.clone();
    let workspace_root = workspace_root.to_string();
    Ok((
        RunnerWorkspacePruneOutput {
            variant: "workspace_prune",
            command: "runner.workspace.prune",
            runner_id,
            dry_run: !options.apply,
            workspace_root,
            lab_workspaces_root,
            min_age_hours: options.min_age_hours,
            candidates: candidate_entries,
            removed,
            skipped,
            total_candidate_count,
            total_candidate_bytes,
            total_removed_bytes,
            remaining_candidate_count,
            remaining_candidate_bytes,
            has_more,
            next_command,
            drain_command,
        },
        0,
    ))
}

fn prune_candidates_for_runner(
    runner: &super::super::Runner,
    lab_workspaces_root: &str,
    options: &RunnerWorkspacePruneOptions,
) -> Result<Vec<RunnerWorkspacePruneEntry>> {
    match runner.kind {
        RunnerKind::Local => prune_candidates_local(Path::new(lab_workspaces_root), options),
        RunnerKind::Ssh => prune_candidates_ssh(runner, lab_workspaces_root, options),
    }
}

/// Reap a single run-scoped materialized workspace (and its sibling Homeboy
/// artifact directory) created during an offloaded run.
///
/// This is the success-path teardown invoked by the run-owned
/// [`MaterializedWorkspace`](super::materialized::MaterializedWorkspace) RAII
/// handle. Historically the only teardown for `_lab_workspaces/<snapshot>`
/// checkouts was the operator-driven [`prune_workspaces`] CLI, so every
/// offloaded run left scraps on the lab (#6678).
///
/// Safety mirrors [`prune_workspaces`]: the target must live under
/// `<workspace_root>/_lab_workspaces`, and removal is delegated to
/// [`remove_workspace`], which refuses to delete the root itself or anything
/// outside it. Local deletion uses the shared resource lifecycle root-bound
/// delete helper. The controller owns the run lifecycle
/// (`RunnerLifecycleOwner::Controller`, surfaced via the workspace lease built
/// by [`workspace_lease`]), so reaping the exact path this run materialized is
/// safe without the source-path-missing heuristic the bulk orphan prune applies.
pub fn reap_run_workspace(
    runner_id: &str,
    remote_path: &str,
    artifact_dir: Option<&str>,
) -> Result<()> {
    let runner = load(runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner workspace reap requires workspace_root",
            Some(runner.id.clone()),
            None,
        )
    })?;
    validate_absolute_path("workspace_root", workspace_root)?;
    let lab_workspaces_root = format!("{}/_lab_workspaces", workspace_root.trim_end_matches('/'));
    remove_workspace(&runner, &lab_workspaces_root, remote_path)?;
    // The sibling Homeboy artifact directory (`<checkout>-homeboy-artifacts`)
    // also lives under `_lab_workspaces`, so it passes the same containment
    // guard. It only exists when the run requested `--output`, so a
    // missing-directory removal error here is expected and non-fatal: the
    // run-scoped checkout is already reaped above.
    if let Some(artifact_dir) = artifact_dir {
        let _ = remove_workspace(&runner, &lab_workspaces_root, artifact_dir);
    }
    Ok(())
}

fn workspace_metadata(
    runner_id: &str,
    local_path: &Path,
    remote_path: &str,
    sync_mode: RunnerWorkspaceSyncMode,
    actual_materialization_mode: Option<&str>,
    snapshot_identity: &str,
    snapshot_excludes: &[String],
    content_manifest: Option<super::snapshot::WorkspaceContentManifest>,
    run_id: Option<&str>,
    cleanup_policy: ResourceCleanupPolicy,
) -> RunnerWorkspaceMetadata {
    let git_state = local_git_state(local_path);
    let resource_lifecycle =
        workspace_resource_lifecycle(runner_id, remote_path, run_id, cleanup_policy);
    RunnerWorkspaceMetadata {
        schema: "homeboy/runner-workspace/v1".to_string(),
        runner_id: runner_id.to_string(),
        repo: Some(workspace_repo_from_path(&local_path.display().to_string())),
        local_path: local_path.display().to_string(),
        remote_path: remote_path.to_string(),
        sync_mode: sync_mode.label().to_string(),
        actual_materialization_mode: actual_materialization_mode.map(str::to_string),
        snapshot_identity: snapshot_identity.to_string(),
        snapshot_excludes: snapshot_excludes.to_vec(),
        content_manifest,
        synced_at: chrono::Utc::now().to_rfc3339(),
        source_ref: git_state.ref_name,
        source_commit: git_state.commit,
        source_remote_url: git_state.remote_url,
        source_dirty: git_state.dirty,
        run_id: run_id.map(str::to_string),
        job_id: None,
        resource_lifecycle: Some(resource_lifecycle),
    }
}

pub(crate) fn workspace_resource_lifecycle(
    runner_id: &str,
    remote_path: &str,
    run_id: Option<&str>,
    cleanup_policy: ResourceCleanupPolicy,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: "runner.workspace".to_string(),
        run_id: run_id.unwrap_or("materialized-workspace").to_string(),
        runner_id: Some(runner_id.to_string()),
        path: remote_path.to_string(),
        root_bound: None,
        kind: "runner_workspace".to_string(),
        ttl: None,
        cleanup_policy,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: Default::default(),
        cleanup_command: run_id
            .map(|run_id| format!("homeboy runs resources --run-id {run_id} --cleanup-plan")),
        status: ResourceLifecycleResourceStatus::Active,
    }
}

pub(crate) fn workspace_repo_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
        .split('@')
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Write the run checkout's tracking metadata, then materialize its
/// validation-dependency siblings.
///
/// These are the two sync steps that run *after* the remote checkout directory
/// already exists on the runner. Grouping them lets [`sync_workspace`] roll the
/// materialized checkout back as a unit if either fails, so a partially-synced
/// run never leaves an orphaned remote directory behind (#6752).
fn write_metadata_and_sync_validation_dependencies(
    runner: &super::super::Runner,
    metadata: RunnerWorkspaceMetadata,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<Vec<RunnerValidationDependencySyncOutput>> {
    write_workspace_metadata(runner, metadata)?;
    sync_validation_dependency_workspaces(runner, local_path, remote_path, excludes)
}

/// Remove a just-materialized run checkout after a later sync step fails.
///
/// Materialization creates the remote `_lab_workspaces/<checkout>` directory
/// before metadata is written and before validation-dependency siblings sync.
/// If one of those later steps fails, [`sync_workspace`] returns an error and
/// never hands the caller a `remote_path` to wrap in the run-owned
/// [`MaterializedWorkspace`](super::materialized::MaterializedWorkspace) RAII
/// handle — so without this rollback the checkout is orphaned: invisible to the
/// reap handle and untracked by inventory until a bulk orphan prune eventually
/// notices the missing source path (#6752).
///
/// This is best-effort cleanup: the original sync error is the actionable
/// failure, so a removal error here is swallowed rather than masking it. The
/// containment guard in [`remove_workspace`] still refuses to remove anything
/// outside `_lab_workspaces`.
fn rollback_materialized_workspace(
    runner: &super::super::Runner,
    workspace_root: &str,
    remote_path: &str,
) {
    let lab_workspaces_root = format!("{}/_lab_workspaces", workspace_root.trim_end_matches('/'));
    let _ = remove_workspace(runner, &lab_workspaces_root, remote_path);
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
            exclude_homeboy_metadata_from_git_status(Path::new(&metadata.remote_path))?;
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
            let parent = parent_remote_path(&metadata_path);
            let staged_metadata_path = temp::unique_name(&metadata_path, ".tmp");
            let metadata_file = tempfile::NamedTempFile::new().map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("create workspace metadata staging file".to_string()),
                )
            })?;
            fs::write(metadata_file.path(), json).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("write workspace metadata staging file".to_string()),
                )
            })?;
            let prepare_command = format!(
                "remote_path={remote_path}; if [ -d \"$remote_path/.git\" ]; then mkdir -p \"$remote_path/.git/info\" && touch \"$remote_path/.git/info/exclude\" && grep -qxF '.homeboy/' \"$remote_path/.git/info/exclude\" || printf '\\n.homeboy/\\n' >> \"$remote_path/.git/info/exclude\"; fi; mkdir -p {parent}",
                remote_path = shell::quote_arg(&metadata.remote_path),
                parent = shell::quote_arg(&parent),
            );
            let publish_command = format!(
                "mv -f {staged_path} {path}",
                staged_path = shell::quote_arg(&staged_metadata_path),
                path = shell::quote_arg(&metadata_path),
            );

            // Metadata is staged outside the live path, so the complete
            // prepare-upload-publish transaction is safe to retry after a
            // transport reset. A fresh client avoids reusing a broken channel.
            let output = retry_idempotent_ssh_operation(|| {
                let (_server, client) = ssh_client_for_runner(runner)?;
                let prepare =
                    client.execute_with_timeout(&prepare_command, WORKSPACE_METADATA_TIMEOUT);
                if !prepare.success {
                    return Ok(prepare);
                }
                let upload = client.upload_file(
                    &metadata_file.path().display().to_string(),
                    &staged_metadata_path,
                );
                if !upload.success {
                    return Ok(upload);
                }
                Ok(client.execute_with_timeout(&publish_command, WORKSPACE_METADATA_TIMEOUT))
            })?;
            if output.success {
                Ok(())
            } else {
                Err(workspace_metadata_ssh_error(&output))
            }
        }
    }
}

fn retry_idempotent_ssh_operation(
    mut operation: impl FnMut() -> Result<CommandOutput>,
) -> Result<CommandOutput> {
    for attempt in 1..=METADATA_SSH_RECOVERY_ATTEMPTS {
        let mut output = operation()?;
        if output.success || !is_transient_ssh_error(&output) {
            return Ok(output);
        }
        if attempt == METADATA_SSH_RECOVERY_ATTEMPTS {
            let detail = output.stderr.trim();
            output.stderr = format!(
                "idempotent runner workspace metadata SSH recovery exhausted after {attempt} fresh-client attempts: {detail}"
            );
            return Ok(output);
        }
    }
    unreachable!("bounded SSH recovery always returns from its final attempt")
}

fn workspace_metadata_ssh_error(output: &CommandOutput) -> Error {
    let stdout = bounded_workspace_metadata_output(&output.stdout);
    let stderr = bounded_workspace_metadata_output(&output.stderr);
    let transport_closed = homeboy_core::server::is_transient_ssh_error(output);
    let close_reason = transport_closed.then(|| {
        stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("SSH transport closed without stderr")
            .to_string()
    });
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!(
            "write runner workspace metadata failed during `workspace_metadata_write` (exit status {}): {}",
            output.exit_code,
            close_reason.as_deref().unwrap_or_else(|| {
                stderr
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("the command exited without stdout or stderr")
            })
        ),
        serde_json::json!({
            "phase": "workspace_metadata_write",
            "command": "write Homeboy runner workspace metadata",
            "timeout_seconds": WORKSPACE_METADATA_TIMEOUT.as_secs(),
            "exit_code": output.exit_code,
            "timed_out": output.timed_out,
            "stdout": stdout,
            "stderr": stderr,
            "transport_close_reason": close_reason,
        }),
    )
    .with_retryable(transport_closed || output.timed_out)
}

fn bounded_workspace_metadata_output(value: &str) -> String {
    if value.len() <= WORKSPACE_METADATA_OUTPUT_LIMIT {
        return value.trim().to_string();
    }
    let mut end = WORKSPACE_METADATA_OUTPUT_LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... [truncated]", value[..end].trim())
}

#[cfg(test)]
mod metadata_write_tests {
    use super::*;

    #[test]
    fn closed_ssh_metadata_write_is_diagnosable_and_retryable() {
        let error = workspace_metadata_ssh_error(&CommandOutput {
            stdout: "partial output".to_string(),
            stderr: "Connection to 192.168.86.63 closed by remote host. client_loop: send disconnect: Broken pipe".to_string(),
            success: false,
            exit_code: -1,
            timed_out: false,
            child_resource: None,
        });

        assert_eq!(error.code, ErrorCode::RunnerLabTransportFailure);
        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["phase"], "workspace_metadata_write");
        assert_eq!(
            error.details["command"],
            "write Homeboy runner workspace metadata"
        );
        assert_eq!(error.details["timeout_seconds"], 30);
        assert_eq!(error.details["exit_code"], -1);
        assert_eq!(error.details["stdout"], "partial output");
        assert!(error.details["stderr"]
            .as_str()
            .unwrap()
            .contains("Broken pipe"));
        assert!(error.details["transport_close_reason"]
            .as_str()
            .unwrap()
            .contains("Connection to 192.168.86.63 closed"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunnerWorkspaceDiskProbe {
    available_bytes: u64,
    total_bytes: u64,
}

fn require_runner_workspace_disk_headroom(
    runner: &super::super::Runner,
    workspace_root: &str,
) -> Result<()> {
    let Some(probe) = runner_workspace_disk_probe(runner, workspace_root)? else {
        return Ok(());
    };
    if !runner_workspace_disk_is_critical(probe) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "workspace_root",
        format!(
            "runner workspace filesystem for `{}` is critically low on free space: {} available of {} total; refusing to sync another Lab workspace",
            runner.id,
            human_bytes(probe.available_bytes),
            human_bytes(probe.total_bytes)
        ),
        Some(workspace_root.to_string()),
        Some(vec![
            format!(
                "Preview safe cleanup candidates with `homeboy runner workspace prune {}`.",
                shell_arg(&runner.id)
            ),
            format!(
                "Remove safe cleanup candidates with `homeboy runner workspace prune {} --apply`.",
                shell_arg(&runner.id)
            ),
            "Increase runner.workspace_root capacity before retrying the Lab run.".to_string(),
        ]),
    ))
}

fn runner_workspace_disk_is_critical(probe: RunnerWorkspaceDiskProbe) -> bool {
    if probe.available_bytes < MIN_RUNNER_WORKSPACE_FREE_BYTES {
        return true;
    }
    probe.total_bytes > 0
        && (probe.available_bytes as f64 / probe.total_bytes as f64)
            < MIN_RUNNER_WORKSPACE_FREE_RATIO
}

fn runner_workspace_disk_probe(
    runner: &super::super::Runner,
    workspace_root: &str,
) -> Result<Option<RunnerWorkspaceDiskProbe>> {
    match runner.kind {
        RunnerKind::Local => Ok(local_runner_workspace_disk_probe(Path::new(workspace_root))),
        RunnerKind::Ssh => ssh_runner_workspace_disk_probe(runner, workspace_root),
    }
}

#[cfg(unix)]
fn local_runner_workspace_disk_probe(path: &Path) -> Option<RunnerWorkspaceDiskProbe> {
    let probe_path = existing_ancestor(path)?;
    let c_path = std::ffi::CString::new(probe_path.to_string_lossy().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = u128::from(stat.f_frsize.max(1));
    Some(RunnerWorkspaceDiskProbe {
        available_bytes: u64::try_from(u128::from(stat.f_bavail).saturating_mul(block_size))
            .ok()?,
        total_bytes: u64::try_from(u128::from(stat.f_blocks).saturating_mul(block_size)).ok()?,
    })
}

#[cfg(not(unix))]
fn local_runner_workspace_disk_probe(_path: &Path) -> Option<RunnerWorkspaceDiskProbe> {
    None
}

fn existing_ancestor(path: &Path) -> Option<&Path> {
    let mut current = path;
    loop {
        if current.exists() {
            return Some(current);
        }
        current = current.parent()?;
    }
}

fn ssh_runner_workspace_disk_probe(
    runner: &super::super::Runner,
    workspace_root: &str,
) -> Result<Option<RunnerWorkspaceDiskProbe>> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let command = format!(
        "p={path}; while [ ! -e \"$p\" ] && [ \"$p\" != / ]; do p=$(dirname \"$p\"); done; df -Pk \"$p\" 2>/dev/null | awk 'NR==2 {{print $2 \" \" $4}}'",
        path = shell::quote_arg(workspace_root),
    );
    let output = client.execute(&command);
    if !output.success {
        return Ok(None);
    }
    let mut parts = output.stdout.split_whitespace();
    let total_kb = match parts.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(value) => value,
        None => return Ok(None),
    };
    let available_kb = match parts.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(value) => value,
        None => return Ok(None),
    };
    Ok(Some(RunnerWorkspaceDiskProbe {
        available_bytes: available_kb.saturating_mul(1024),
        total_bytes: total_kb.saturating_mul(1024),
    }))
}

fn human_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

fn exclude_homeboy_metadata_from_git_status(workspace_path: &Path) -> Result<()> {
    let git_dir = workspace_path.join(".git");
    if !git_dir.is_dir() {
        return Ok(());
    }

    let exclude_path = git_dir.join("info/exclude");
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("create workspace git exclude dir".to_string()),
            )
        })?;
    }

    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == ".homeboy/") {
        return Ok(());
    }

    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(".homeboy/\n");
    fs::write(&exclude_path, next).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write workspace git exclude".to_string()),
        )
    })
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
    let reason = prune_candidate_reason(&metadata, path, source_path)?;
    let Some(reason) = reason else {
        return Ok(None);
    };
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
        reason,
    }))
}

fn prune_candidate_reason(
    metadata: &serde_json::Value,
    path: &Path,
    source_path: &str,
) -> Result<Option<String>> {
    if let Some(resource) = metadata.get("resource_lifecycle") {
        let resource: ResourceLifecycleRecord =
            serde_json::from_value(resource.clone()).map_err(|err| {
                Error::internal_json(err.to_string(), Some(path.display().to_string()))
            })?;
        if matches!(
            resource.cleanup_policy,
            ResourceCleanupPolicy::DeleteAfterTtl
        ) {
            if let Some(ttl) = resource.ttl.as_deref() {
                let modified = fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .map_err(|err| {
                        Error::internal_io(
                            err.to_string(),
                            Some("read workspace mtime".to_string()),
                        )
                    })?;
                if resource_lifecycle_path_ttl_expired_at(ttl, modified, chrono::Utc::now()) {
                    return Ok(Some("resource_ttl_expired".to_string()));
                }
            }
        }
    }

    if !Path::new(source_path).exists() {
        return Ok(Some("source_path_missing".to_string()));
    }
    Ok(None)
}

fn prune_candidates_ssh(
    runner: &super::super::Runner,
    root: &str,
    options: &RunnerWorkspacePruneOptions,
) -> Result<Vec<RunnerWorkspacePruneEntry>> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let min_age = options.min_age_hours.saturating_mul(3600);
    let command = prune_scan_command(root, min_age);
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
        let reason = prune_candidate_reason_from_decoded_metadata(&metadata, age_seconds);
        let Some(reason) = reason else {
            continue;
        };
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
            reason,
        });
    }
    candidates.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| b.age_seconds.cmp(&a.age_seconds))
    });
    Ok(candidates)
}

fn prune_candidate_reason_from_decoded_metadata(
    metadata: &serde_json::Value,
    age_seconds: u64,
) -> Option<String> {
    if let Some(resource) = metadata.get("resource_lifecycle") {
        if resource
            .get("cleanup_policy")
            .and_then(|value| value.as_str())
            == Some("delete_after_ttl")
        {
            if let Some(ttl) = resource.get("ttl").and_then(|value| value.as_str()) {
                let modified = std::time::SystemTime::now()
                    .checked_sub(std::time::Duration::from_secs(age_seconds))?;
                if resource_lifecycle_path_ttl_expired_at(ttl, modified, chrono::Utc::now()) {
                    return Some("resource_ttl_expired".to_string());
                }
            }
        }
    }

    let source_path = metadata
        .get("local_path")
        .and_then(|value| value.as_str())?;
    (!Path::new(source_path).exists()).then(|| "source_path_missing".to_string())
}

pub(crate) fn prune_scan_command(root: &str, min_age: u64) -> String {
    format!(
        "root={root}; meta_rel={meta}; now=$(date +%s); if [ -d \"$root\" ]; then find \"$root\" -mindepth 1 -maxdepth 1 -type d -exec sh -c 'meta_rel=$1; now=$2; min_age=$3; shift 3; for dir do meta=\"$dir/$meta_rel\"; [ -f \"$meta\" ] || continue; mtime=$(stat -c %Y \"$dir\" 2>/dev/null || stat -f %m \"$dir\" 2>/dev/null || echo 0); age=$((now-mtime)); [ \"$age\" -ge \"$min_age\" ] || continue; if find \"$dir/.homeboy\" -type f \\( -name \"*.patch\" -o -name \"*.diff\" -o -name \"*patch*\" \\) 2>/dev/null | grep -q .; then continue; fi; blocks=$(du -sk \"$dir\" 2>/dev/null); blocks=${{blocks%%[!0-9]*}}; bytes=$((blocks * 1024)); printf \"%s\\t%s\\t%s\\t\" \"$age\" \"${{bytes:-0}}\" \"$dir\"; base64 < \"$meta\" | tr -d \"\\n\"; printf \"\\n\"; done' sh {meta_arg} \"$now\" {min_age_arg} {{}} +; fi",
        root = shell::quote_arg(root),
        meta = shell::quote_arg(WORKSPACE_METADATA_FILE),
        meta_arg = shell::quote_arg(WORKSPACE_METADATA_FILE),
        min_age_arg = shell::quote_arg(&min_age.to_string()),
    )
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
        RunnerKind::Local => remove_local_workspace_with_lifecycle(root_path, path),
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

fn remove_local_workspace_with_lifecycle(root: &Path, path: &Path) -> Result<()> {
    let resource = ResourceLifecycleRecord {
        owner: "runner.workspace".to_string(),
        run_id: "materialized-workspace".to_string(),
        runner_id: None,
        path: path.display().to_string(),
        root_bound: Some(root.display().to_string()),
        kind: "runner_workspace".to_string(),
        ttl: None,
        cleanup_policy: ResourceCleanupPolicy::DeleteOnSuccess,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: Default::default(),
        cleanup_command: None,
        status: ResourceLifecycleResourceStatus::CleanupPending,
    };
    let cleanup_path = ResourceLifecycle::cleanup_path(root, &resource).map_err(|reason| {
        Error::validation_invalid_argument(
            "remote_path",
            format!("refusing to remove runner workspace: {reason}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    ResourceLifecycle::delete_path(&cleanup_path)
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

pub(crate) fn shell_arg(value: &str) -> String {
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
    synthetic_checkout: Option<super::snapshot::SyntheticCheckoutIdentity>,
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
        synthetic_checkout_commit: synthetic_checkout
            .as_ref()
            .map(|identity| identity.synthetic_commit.clone()),
        synthetic_checkout_ref: synthetic_checkout
            .as_ref()
            .map(|identity| identity.synthetic_ref.clone()),
        synthetic_checkout_tree: synthetic_checkout.map(|identity| identity.synthetic_tree),
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
    let remote_url = git_output(local_path, &["config", "--get", "remote.origin.url"])
        .ok()
        .filter(|value| !value.trim().is_empty());

    LocalGitState {
        commit,
        ref_name,
        dirty,
        remote_url,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_runner_git_auth_or_network_failure, retry_idempotent_ssh_operation,
        runner_workspace_disk_is_critical, RunnerWorkspaceDiskProbe,
    };
    use homeboy_core::error::{Error, ErrorCode};
    use homeboy_core::server::CommandOutput;

    fn command_output(success: bool, exit_code: i32, stderr: &str) -> CommandOutput {
        CommandOutput {
            stdout: String::new(),
            stderr: stderr.to_string(),
            success,
            exit_code,
            timed_out: false,
            child_resource: None,
        }
    }

    #[test]
    fn runner_git_network_failure_in_hint_activates_controller_fallback() {
        let error = Error::validation_invalid_argument(
            "changed_since",
            "runner dispatch could not make the requested --changed-since base reachable in the runner workspace before dispatch",
            None,
            Some(vec![
                "Remote git error: ssh: Could not resolve hostname git.example.test: Temporary failure in name resolution\nfatal: Could not read from remote repository."
                    .to_string(),
            ]),
        );

        assert!(is_runner_git_auth_or_network_failure(&error));
    }

    #[test]
    fn runner_git_network_failure_in_structured_details_activates_controller_fallback() {
        let error = Error::new(
            ErrorCode::RunnerLabTransportFailure,
            "runner Git materialization failed",
            serde_json::json!({
                "stderr": "fatal: unable to access source: Failed to connect to git.example.test",
            }),
        );

        assert!(is_runner_git_auth_or_network_failure(&error));
    }

    #[test]
    fn runner_git_non_transport_failure_does_not_activate_controller_fallback() {
        let error = Error::validation_invalid_argument(
            "changed_since",
            "runner dispatch could not make the requested --changed-since base reachable in the runner workspace before dispatch",
            None,
            Some(vec!["Remote git error: fatal: invalid object name 'missing-ref'".to_string()]),
        );

        assert!(!is_runner_git_auth_or_network_failure(&error));
    }

    #[test]
    fn metadata_ssh_recovery_restarts_the_staged_write_after_a_transport_reset() {
        let mut attempts = 0;
        let mut steps = Vec::new();
        let output = retry_idempotent_ssh_operation(|| {
            attempts += 1;
            steps.push(format!("prepare-{attempts}"));
            steps.push(format!("stage-{attempts}"));
            if attempts == 1 {
                return Ok(command_output(
                    false,
                    255,
                    "Connection to runner.example.test closed by remote host.\nclient_loop: send disconnect: Broken pipe",
                ));
            }
            steps.push(format!("publish-{attempts}"));
            Ok(command_output(true, 0, ""))
        })
        .expect("retry operation");

        assert!(output.success);
        assert_eq!(attempts, 2);
        assert_eq!(
            steps,
            ["prepare-1", "stage-1", "prepare-2", "stage-2", "publish-2"]
        );
    }

    #[test]
    fn metadata_ssh_recovery_refuses_remote_command_failures() {
        let mut attempts = 0;
        let output = retry_idempotent_ssh_operation(|| {
            attempts += 1;
            Ok(command_output(
                false,
                1,
                "permission denied writing metadata",
            ))
        })
        .expect("retry operation");

        assert!(!output.success);
        assert_eq!(attempts, 1);
        assert_eq!(output.stderr, "permission denied writing metadata");
    }

    #[test]
    fn metadata_ssh_recovery_reports_transport_exhaustion() {
        let mut attempts = 0;
        let output = retry_idempotent_ssh_operation(|| {
            attempts += 1;
            Ok(command_output(false, 255, "Broken pipe"))
        })
        .expect("retry operation");

        assert!(!output.success);
        assert_eq!(attempts, 2);
        assert!(output
            .stderr
            .contains("recovery exhausted after 2 fresh-client attempts"));
        assert!(output.stderr.contains("Broken pipe"));
    }

    #[test]
    fn runner_workspace_disk_pressure_blocks_low_absolute_free_space() {
        assert!(runner_workspace_disk_is_critical(
            RunnerWorkspaceDiskProbe {
                available_bytes: 512 * 1024 * 1024,
                total_bytes: 500 * 1024 * 1024 * 1024,
            }
        ));
    }

    #[test]
    fn runner_workspace_disk_pressure_blocks_low_free_ratio() {
        assert!(runner_workspace_disk_is_critical(
            RunnerWorkspaceDiskProbe {
                available_bytes: 2 * 1024 * 1024 * 1024,
                total_bytes: 500 * 1024 * 1024 * 1024,
            }
        ));
    }

    #[test]
    fn runner_workspace_disk_pressure_allows_headroom() {
        assert!(!runner_workspace_disk_is_critical(
            RunnerWorkspaceDiskProbe {
                available_bytes: 20 * 1024 * 1024 * 1024,
                total_bytes: 500 * 1024 * 1024 * 1024,
            }
        ));
    }
}
