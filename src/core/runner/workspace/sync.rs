use std::path::Path;

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
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
    DEFAULT_EXCLUDES,
};
use super::util::{deterministic_remote_path, git_output, validate_absolute_path};

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
