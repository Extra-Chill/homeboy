use std::collections::BTreeSet;
use std::fs;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use homeboy_core::source_snapshot::SourceSnapshot;

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
    local_snapshot_stats, materialize_prepared_workspace_update, materialize_snapshot,
    materialize_snapshot_git, materialize_snapshot_incremental, materialize_snapshot_with_scratch,
    snapshot_identity, snapshot_manifest_delta, workspace_content_manifest_for_policy,
    SnapshotManifestDelta, WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
};
use super::types::{
    canonical_workspace_path, ByteFileCounts, LocalGitState, RunnerWorkspaceCurrentSummary,
    RunnerWorkspaceLivenessEvidence, RunnerWorkspaceMaterializationPlan, RunnerWorkspaceMetadata,
    RunnerWorkspacePruneEntry, RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput,
    RunnerWorkspacePruneSkippedEntry, RunnerWorkspaceSnapshotEntry, RunnerWorkspaceSnapshotFilters,
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
    RunnerWorkspaceTerminalEvidence, RunnerWorkspaceUpdateOptions, RunnerWorkspaceUpdateOutput,
    DEFAULT_EXCLUDES,
};
use super::util::{
    deterministic_remote_path, git_output, parent_remote_path, ssh_client_for_runner,
    validate_absolute_path,
};
use homeboy_core::engine::shell;
use homeboy_core::server::{
    execute_local_command_in_dir_with_timeout, is_transient_ssh_error, CommandOutput,
};

mod snapshots;
use snapshots::workspace_snapshot_for_lease;
#[cfg(test)]
pub(crate) use snapshots::workspace_snapshot_scan_command;
pub use snapshots::{list_workspaces, workspace_snapshots};

pub(crate) const WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";
const MIN_RUNNER_WORKSPACE_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_RUNNER_WORKSPACE_FREE_RATIO: f64 = 0.01;
const METADATA_SSH_RECOVERY_ATTEMPTS: usize = 2;
const WORKSPACE_METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const WORKSPACE_METADATA_OUTPUT_LIMIT: usize = 4 * 1024;
const WORKSPACE_PRUNE_TIMEOUT: Duration = Duration::from_secs(30);
// A page must leave time for its transport timeout and advance past a directory
// whose size cannot be measured. Size is advisory, never a deletion proof.
const WORKSPACE_PRUNE_SCAN_BUDGET: Duration = Duration::from_secs(20);
const WORKSPACE_PRUNE_SIZE_TIMEOUT: Duration = Duration::from_secs(1);
// Prepared sources include hydrated dependency trees, so retain a small fixed
// working set instead of making every historical commit permanent runner state.
const PREPARED_SOURCE_CACHE_MAX_ENTRIES: usize = 8;

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
            let admission = require_snapshot_filesystem_admission(
                &runner,
                &local_path,
                &remote_path,
                &stats,
                content_manifest.entry_count as u64,
            )?;
            let scratch = admission.scratch();
            let git_backed_snapshot = git_output(&local_path, &["rev-parse", "HEAD"]).is_ok();
            let (synthetic_checkout, fallback_reason) = if options.mode
                == RunnerWorkspaceSyncMode::SnapshotGit
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
                        (None, None)
                    }
                    Err(_) => {
                        rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                        if let Err(snapshot_error) = materialize_snapshot_with_scratch(
                            &runner,
                            &local_path,
                            &remote_path,
                            &excludes,
                            Some(scratch),
                        ) {
                            rollback_materialized_workspace(&runner, workspace_root, &remote_path);
                            return Err(snapshot_error);
                        }
                        materialization_plan.snapshot_transfer =
                            Some(super::types::SnapshotTransferStats {
                                reused: ByteFileCounts::default(),
                                transferred: stats.clone(),
                                final_size: stats.clone(),
                            });
                        (
                            None,
                            Some("snapshot_git_checkout_and_controller_bundle_failed".to_string()),
                        )
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
                    Ok(identity) => (Some(identity), None),
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
                        if let Err(error) = materialize_snapshot_with_scratch(
                            &runner,
                            &local_path,
                            &remote_path,
                            &excludes,
                            Some(scratch),
                        ) {
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
                (None, None)
            };
            if fallback_reason.is_some() || options.mode == RunnerWorkspaceSyncMode::Snapshot {
                materialization_plan.actual_materialization_mode =
                    Some("filesystem_snapshot".to_string());
            } else if options.mode == RunnerWorkspaceSyncMode::SnapshotGit && git_backed_snapshot {
                // Snapshot-git deliberately retains Git metadata for callers
                // that need a checkout baseline.
                materialization_plan.actual_materialization_mode =
                    Some(RunnerWorkspaceSyncMode::SnapshotGit.label().to_string());
            }
            materialization_plan.fallback_reason = fallback_reason;
            let metadata = workspace_metadata(
                &runner.id,
                &local_path,
                &remote_path,
                options.mode,
                materialization_plan.actual_materialization_mode.as_deref(),
                materialization_plan.fallback_reason.as_deref(),
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
            let prepared_workspace_lease = metadata.workspace_lease.clone();
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
                    prepared_workspace_lease,
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
            let git = match git_snapshot(
                &local_path,
                options.changed_since_base.as_deref(),
                options.git_fetch_refs.clone(),
                options.controller_routed_git,
            ) {
                Ok(git) => git,
                Err(error) if controller_object_closure_unavailable(&error) => {
                    return materialize_git_fallback_filesystem_snapshot(
                        &runner,
                        workspace_root,
                        &local_path,
                        &excludes,
                        &includes,
                        &options,
                        &error,
                    );
                }
                Err(error) => return Err(error),
            };
            let remote_path = deterministic_remote_path(
                workspace_root,
                &local_path,
                &git.head,
                options.run_isolation_token.as_deref(),
            );
            reject_existing_job_workspace(
                &runner,
                &remote_path,
                options.run_isolation_token.as_deref(),
            )?;
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
            // A prepared source is immutable and keyed by the controller path
            // plus exact commit. Jobs never execute from it: each receives a
            // private copied view which preserves #10105's ownership boundary.
            let prepared_cache = prepared_source_cache_path(workspace_root, &local_path, &git.head);
            let reused_prepared_source =
                materialize_prepared_source_view(&runner, &prepared_cache, &remote_path)?;
            let materialized = if reused_prepared_source {
                materialization_plan.actual_materialization_mode =
                    Some("prepared_source_view".to_string());
                Ok(None)
            } else if options.controller_routed_git
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
                )
                .map(Some)
            } else {
                if runner.kind != RunnerKind::Local {
                    source_materialization::validate_runner_git_materialization(
                        &git.remote_url,
                        &runner.id,
                    )?;
                }
                match materialize_git(
                    &runner,
                    &remote_path,
                    &git.remote_url,
                    &git.head,
                    git.branch.as_deref(),
                    git.changed_since_base.as_deref(),
                    &git.git_fetch_refs,
                    options.allow_dirty_lab_workspace,
                ) {
                    Ok(()) => Ok(None),
                    Err(error) => {
                        if !is_runner_git_auth_or_network_failure(&error) {
                            Err(error)
                        } else {
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
                            )
                            .map(Some)
                        }
                    }
                }
            };
            match materialized {
                Ok(provenance) => materialization_plan.controller_git_bundle = provenance,
                Err(error) if controller_object_closure_unavailable(&error) => {
                    return materialize_git_fallback_filesystem_snapshot(
                        &runner,
                        workspace_root,
                        &local_path,
                        &excludes,
                        &includes,
                        &options,
                        &error,
                    );
                }
                Err(error) => return Err(error),
            }
            let metadata = workspace_metadata(
                &runner.id,
                &local_path,
                &remote_path,
                RunnerWorkspaceSyncMode::Git,
                materialization_plan.actual_materialization_mode.as_deref(),
                materialization_plan.fallback_reason.as_deref(),
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
                    prepared_workspace_lease: None,
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

/// Cache only a source that has completed dependency hydration. The cache lives
/// outside `_lab_workspaces`, is never handed to a job, and has no terminal-job
/// cleanup owner. A private job view is copied from it on a same-commit hit.
pub(crate) fn save_prepared_source_cache(
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<()> {
    let runner = load(runner_id)?;
    // The cache is a pure optimization: a hit lets the next same-commit job skip
    // dependency hydration. A runner with no configured `workspace_root` has
    // nowhere to keep it, which is a reason to skip caching — not to fail an
    // otherwise-successful offload that has already hydrated its workspace.
    let Some(workspace_root) = runner.workspace_root.as_deref() else {
        return Ok(());
    };
    let local_path = canonical_workspace_path(local_path)?;
    let commit = git_output(&local_path, &["rev-parse", "HEAD"])?;
    let cache = prepared_source_cache_path(workspace_root, &local_path, &commit);
    let cache_root = prepared_source_cache_root(workspace_root);
    let command = format!(
        "cache={cache}; cache_root={cache_root}; source={source}; lock=\"$cache.lock\"; mkdir -p \"$cache_root\"; test -f \"$cache/.homeboy/prepared-source-ready\" || {{ mkdir \"$lock\" 2>/dev/null || exit 0; trap 'rmdir \"$lock\"' EXIT; tmp=\"$cache.tmp.$$\"; rm -rf \"$tmp\"; cp -a \"$source\" \"$tmp\"; mkdir -p \"$tmp/.homeboy\"; : > \"$tmp/.homeboy/prepared-source-ready\"; chmod -R a-w \"$tmp\"; mv \"$tmp\" \"$cache\"; }}; kept=0; ls -1dt \"$cache_root\"/* 2>/dev/null | while IFS= read -r candidate; do test -f \"$candidate/.homeboy/prepared-source-ready\" || continue; if [ \"$kept\" -lt {max_entries} ]; then kept=$((kept + 1)); continue; fi; test -e \"$candidate.lock\" && continue; ls \"$candidate\".lease.* >/dev/null 2>&1 && continue; chmod -R u+w \"$candidate\" && rm -rf \"$candidate\"; done",
        cache = shell::quote_arg(&cache),
        cache_root = shell::quote_arg(&cache_root),
        source = shell::quote_arg(remote_path),
        max_entries = PREPARED_SOURCE_CACHE_MAX_ENTRIES,
    );
    run_workspace_shell_command(&runner, &command, "save prepared Lab source cache")
}

fn prepared_source_cache_path(workspace_root: &str, local_path: &Path, commit: &str) -> String {
    let view = deterministic_remote_path(workspace_root, local_path, commit, None);
    let name = Path::new(&view)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    format!("{}/{name}", prepared_source_cache_root(workspace_root))
}

fn prepared_source_cache_root(workspace_root: &str) -> String {
    format!(
        "{}/_lab_prepared_sources",
        workspace_root.trim_end_matches('/')
    )
}

fn materialize_prepared_source_view(
    runner: &super::super::Runner,
    cache: &str,
    remote_path: &str,
) -> Result<bool> {
    let command = format!(
        "cache={cache}; destination={destination}; test -f \"$cache/.homeboy/prepared-source-ready\" && test ! -e \"$destination\" || exit 1; lease=\"$cache.lease.$$\"; : > \"$lease\" || exit 1; trap 'rm -f \"$lease\"' EXIT HUP INT TERM; test -f \"$cache/.homeboy/prepared-source-ready\" && mkdir -p \"$(dirname \"$destination\")\" && cp -a \"$cache\" \"$destination\" && chmod -R u+w \"$destination\" || {{ rm -rf \"$destination\"; exit 1; }}",
        cache = shell::quote_arg(cache),
        destination = shell::quote_arg(remote_path),
    );
    run_workspace_shell_success(runner, &command, "materialize prepared Lab source view")
}

fn run_workspace_shell_success(
    runner: &super::super::Runner,
    command: &str,
    action: &str,
) -> Result<bool> {
    match runner.kind {
        RunnerKind::Local => Ok(std::process::Command::new("sh")
            .args(["-c", command])
            .status()
            .map_err(|error| Error::internal_io(error.to_string(), Some(action.to_string())))?
            .success()),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            Ok(client.execute(command).success)
        }
    }
}

fn run_workspace_shell_command(
    runner: &super::super::Runner,
    command: &str,
    action: &str,
) -> Result<()> {
    if run_workspace_shell_success(runner, command, action)? {
        Ok(())
    } else {
        Err(Error::internal_unexpected(format!("{action} failed")))
    }
}

/// A promisor checkout can lack the object closure needed to build a controller
/// bundle. Its working tree is still authoritative, so ship that content rather
/// than failing before a read-only review can run. The caller removes any
/// changed-since argument because this materialization has no Git baseline.
fn materialize_git_fallback_filesystem_snapshot(
    runner: &crate::Runner,
    workspace_root: &str,
    local_path: &Path,
    excludes: &[String],
    includes: &[String],
    options: &RunnerWorkspaceSyncOptions,
    _closure_error: &Error,
) -> Result<(RunnerWorkspaceSyncOutput, i32)> {
    ensure_no_runner_workspace_metadata_collision(local_path)?;
    let snapshot = snapshot_identity(local_path, excludes, includes)?;
    let remote_path = temp::unique_name(
        &deterministic_remote_path(
            workspace_root,
            local_path,
            &snapshot,
            options.run_isolation_token.as_deref(),
        ),
        "",
    );
    let stats = local_snapshot_stats(local_path, excludes, includes)?;
    let content_manifest = workspace_content_manifest_for_policy(
        local_path,
        excludes,
        WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
    )?;
    let workspace_cleanliness = "filesystem_snapshot_after_git_closure_failure";
    let mut materialization_plan = workspace_materialization_plan(
        workspace_root,
        local_path,
        &remote_path,
        &snapshot,
        options,
        includes,
        workspace_cleanliness,
    );
    materialization_plan
        .declared_inputs
        .requested_changed_since_base = options.changed_since_base.clone();
    materialization_plan.declared_inputs.changed_since_base = None;
    materialization_plan.declared_inputs.git_fetch_refs.clear();
    if let Err(error) = materialize_snapshot(runner, local_path, &remote_path, excludes) {
        rollback_materialized_workspace(runner, workspace_root, &remote_path);
        return Err(error);
    }
    materialization_plan.actual_materialization_mode = Some("filesystem_snapshot".to_string());
    materialization_plan.fallback_reason =
        Some("controller_git_object_closure_unavailable".to_string());
    materialization_plan.snapshot_transfer = Some(super::types::SnapshotTransferStats {
        reused: ByteFileCounts::default(),
        transferred: stats.clone(),
        final_size: stats.clone(),
    });
    let metadata = workspace_metadata(
        &runner.id,
        local_path,
        &remote_path,
        options.mode,
        materialization_plan.actual_materialization_mode.as_deref(),
        materialization_plan.fallback_reason.as_deref(),
        &snapshot,
        excludes,
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
    let prepared_workspace_lease = metadata.workspace_lease.clone();
    let validation_dependencies = match write_metadata_and_sync_validation_dependencies(
        runner,
        metadata,
        local_path,
        &remote_path,
        excludes,
    ) {
        Ok(dependencies) => dependencies,
        Err(error) => {
            rollback_materialized_workspace(runner, workspace_root, &remote_path);
            return Err(error);
        }
    };
    let current_workspace = current_workspace_summary(
        local_path,
        &remote_path,
        RunnerWorkspaceSyncMode::Snapshot,
        true,
        None,
    );
    let workspace_lease = workspace_lease(&runner.id, &current_workspace);
    Ok((
        RunnerWorkspaceSyncOutput {
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: runner.id.clone(),
            local_path: local_path.display().to_string(),
            remote_path,
            materialization_plan,
            current_workspace,
            workspace_lease,
            resource_lifecycle,
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: snapshot,
            prepared_workspace_lease,
            counts: stats,
            excludes: excludes.to_vec(),
            includes: includes.to_vec(),
            workspace_cleanliness: workspace_cleanliness.to_string(),
            validation_dependencies,
        },
        0,
    ))
}

fn controller_object_closure_unavailable(error: &Error) -> bool {
    error.details["reason"].as_str() == Some("controller_git_object_closure_unavailable")
}

/// Advance a prepared snapshot selected by its snapshot lease. The lease is a
/// snapshot identity, not a caller-provided runner path.
pub fn update_workspace(
    runner_id: &str,
    options: RunnerWorkspaceUpdateOptions,
) -> Result<(RunnerWorkspaceUpdateOutput, i32)> {
    let runner = load(runner_id)?;
    let local_path = canonical_workspace_path(&options.path)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner workspace update requires workspace_root",
            Some(runner.id.clone()),
            None,
        )
    })?;
    validate_absolute_path("workspace_root", workspace_root)?;
    let snapshot = workspace_snapshot_for_lease(
        &runner,
        &format!("{}/_lab_workspaces", workspace_root.trim_end_matches('/')),
        &options.lease,
    )?
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease",
            "workspace update requires a current opaque workspace lease for this runner",
            Some(options.lease.clone()),
            None,
        )
    })?;
    let state = local_git_state(&local_path);
    if snapshot.local_path != local_path.display().to_string()
        || snapshot.source_remote_url != state.remote_url
        || snapshot.source_ref != state.ref_name
    {
        return Err(Error::validation_invalid_argument(
            "lease",
            "workspace update rejected an unrelated repository or branch lineage",
            Some(options.lease),
            None,
        ));
    }
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
    let includes = runner.policy.snapshot_includes.clone();
    let excludes = effective_snapshot_excludes(excludes, &includes);
    if snapshot.snapshot_excludes != excludes {
        return Err(Error::validation_invalid_argument(
            "lease",
            "workspace update rejected an incompatible exclude policy",
            Some(snapshot.remote_path),
            None,
        ));
    }
    let previous_manifest = snapshot.content_manifest.ok_or_else(|| {
        Error::validation_invalid_argument(
            "lease",
            "workspace update requires a manifest-backed snapshot lease",
            Some(options.lease.clone()),
            None,
        )
    })?;
    let manifest = workspace_content_manifest_for_policy(
        &local_path,
        &excludes,
        WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
    )?;
    let delta = snapshot_manifest_delta(&manifest, &previous_manifest)?;
    let resulting_snapshot_identity = snapshot_identity(&local_path, &excludes, &includes)?;
    let original_prepared_snapshot_identity = snapshot
        .original_prepared_snapshot_identity
        .clone()
        .unwrap_or_else(|| snapshot.snapshot_identity.clone());
    let mut update_lineage = snapshot.update_lineage.clone();
    update_lineage.push(resulting_snapshot_identity.clone());
    let metadata = workspace_metadata(
        &runner.id,
        &local_path,
        &snapshot.remote_path,
        RunnerWorkspaceSyncMode::Snapshot,
        Some("prepared_workspace_delta"),
        None,
        &resulting_snapshot_identity,
        &excludes,
        Some(manifest),
        snapshot.run_id.as_deref(),
        ResourceCleanupPolicy::DeleteOnSuccess,
    );
    let mut metadata = metadata;
    metadata.workspace_lease = Some(new_workspace_lease());
    metadata.workspace_generation = snapshot.workspace_generation.saturating_add(1);
    metadata.original_prepared_snapshot_identity =
        Some(original_prepared_snapshot_identity.clone());
    metadata.update_lineage = update_lineage.clone();
    materialize_prepared_workspace_update(
        &runner,
        &local_path,
        &snapshot.remote_path,
        &delta,
        snapshot.workspace_lease.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "lease",
                "workspace update requires a lease-backed prepared workspace",
                Some(options.lease.clone()),
                None,
            )
        })?,
        &serde_json::to_string_pretty(&metadata)
            .map_err(|err| Error::internal_json(err.to_string(), None))?,
    )?;
    let exec_command = format!(
        "homeboy runner exec {} --cwd {} --env HOMEBOY_PREPARED_WORKSPACE_ORIGINAL_SNAPSHOT={} --env HOMEBOY_PREPARED_WORKSPACE_UPDATE_LINEAGE={} -- <command>",
        shell_arg(&runner.id),
        shell_arg(&snapshot.remote_path),
        shell_arg(&original_prepared_snapshot_identity),
        shell_arg(&update_lineage.join(",")),
    );
    Ok((
        RunnerWorkspaceUpdateOutput {
            variant: "workspace_update",
            command: "runner.workspace.update",
            runner_id: runner.id.clone(),
            lease: metadata
                .workspace_lease
                .clone()
                .expect("new workspace lease"),
            remote_path: snapshot.remote_path.clone(),
            original_snapshot_identity: original_prepared_snapshot_identity.clone(),
            original_workspace_lease: options.lease,
            resulting_snapshot_identity,
            original_prepared_snapshot_identity,
            update_lineage,
            changed_paths: delta.changed_paths,
            deleted_paths: delta.deleted_paths,
            retained_prepared_assets: true,
            exec_command,
        },
        0,
    ))
}

/// Hydrate execution provenance from a metadata-backed prepared workspace.
/// Ordinary runner paths remain unchanged; only an exact workspace match gains
/// the original snapshot and ordered delta lineage recorded at promotion time.
pub fn hydrate_prepared_workspace_source_snapshot(
    runner_id: &str,
    remote_path: &str,
    source_snapshot: &mut SourceSnapshot,
) -> Result<()> {
    let runner = load(runner_id)?;
    let Some(workspace_root) = runner.workspace_root.as_deref() else {
        return Ok(());
    };
    let prepared_root = format!("{}/_lab_workspaces/", workspace_root.trim_end_matches('/'));
    if !remote_path.starts_with(&prepared_root) {
        return Ok(());
    }
    let (snapshots, _) = workspace_snapshots(
        runner_id,
        RunnerWorkspaceSnapshotFilters {
            limit: usize::MAX,
            ..Default::default()
        },
    )?;
    let Some(snapshot) = snapshots
        .snapshots
        .into_iter()
        .find(|snapshot| snapshot.remote_path == remote_path)
    else {
        return Ok(());
    };
    let Some(original) = snapshot.original_prepared_snapshot_identity else {
        return Ok(());
    };
    source_snapshot.workspace_snapshot_identity = Some(snapshot.snapshot_identity);
    source_snapshot.prepared_workspace_original_snapshot_identity = Some(original);
    source_snapshot.prepared_workspace_update_lineage = snapshot.update_lineage;
    Ok(())
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
        prepared_workspace_lease: snapshot.workspace_lease,
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
    let mut skipped_live_count = 0;
    let mut skipped_unknown_count = 0;
    let mut stop_commands = Vec::new();
    let mut candidate_entries = Vec::new();
    let mut total_candidate_count = 0;
    let mut total_candidate_bytes = 0;
    let mut scanned_workspace_count = 0;
    let mut scan_complete = true;
    let mut continuation_cursor = options.cursor.clone();
    let remaining_candidates: Vec<RunnerWorkspacePruneEntry> = Vec::new();
    for pass in 0..passes {
        let scan = prune_candidates_for_runner(
            &runner,
            &lab_workspaces_root,
            &options,
            limit,
            continuation_cursor.as_deref(),
        )?;
        scanned_workspace_count += scan.scanned_workspace_count;
        scan_complete = scan.scan_complete;
        continuation_cursor = scan.continuation_cursor;
        let candidates = scan.candidates;
        for withheld in scan.withheld {
            match withheld.liveness.state.as_str() {
                "live" => skipped_live_count += 1,
                _ => skipped_unknown_count += 1,
            }
            stop_commands.extend(
                withheld
                    .liveness
                    .observations
                    .iter()
                    .filter_map(|observation| observation.strip_prefix("active_runner_job:"))
                    .map(|job_id| {
                        format!(
                            "homeboy runner job cancel {} {}",
                            shell_arg(&runner.id),
                            shell_arg(job_id)
                        )
                    }),
            );
            skipped.push(RunnerWorkspacePruneSkippedEntry {
                remote_path: withheld.remote_path,
                reason: format!(
                    "workspace liveness is {}; {}",
                    withheld.liveness.state,
                    withheld.liveness.observations.join(", ")
                ),
            });
        }
        if pass == 0 {
            total_candidate_count = candidates.len();
            total_candidate_bytes = candidates.iter().map(|entry| entry.bytes).sum();
        }
        if candidates.is_empty() {
            if options.apply && !scan_complete {
                continue;
            }
            break;
        }
        for candidate in candidates {
            if !options.apply {
                candidate_entries.push(candidate);
                continue;
            }
            // The scan is only a snapshot. Recheck ownership at the destructive boundary.
            match revalidate_candidate_liveness(&runner, &candidate) {
                Ok(liveness) if liveness.state == "inactive" => {
                    match remove_prune_candidate(&runner, &lab_workspaces_root, &candidate) {
                        Ok(None) => removed.push(candidate),
                        Ok(Some(liveness)) => skipped.push(RunnerWorkspacePruneSkippedEntry {
                            remote_path: candidate.remote_path,
                            reason: format!(
                                "workspace liveness changed to {}; {}",
                                liveness.state,
                                liveness.observations.join(", ")
                            ),
                        }),
                        Err(err) => skipped.push(RunnerWorkspacePruneSkippedEntry {
                            remote_path: candidate.remote_path,
                            reason: err.to_string(),
                        }),
                    }
                }
                Ok(liveness) => skipped.push(RunnerWorkspacePruneSkippedEntry {
                    remote_path: candidate.remote_path,
                    reason: format!(
                        "workspace liveness changed to {}; {}",
                        liveness.state,
                        liveness.observations.join(", ")
                    ),
                }),
                Err(err) => skipped.push(RunnerWorkspacePruneSkippedEntry {
                    remote_path: candidate.remote_path,
                    reason: err.to_string(),
                }),
            }
        }
        if !options.apply || scan_complete {
            break;
        }
    }

    let remaining_candidate_count = remaining_candidates.len();
    let remaining_candidate_bytes = remaining_candidates.iter().map(|entry| entry.bytes).sum();
    let has_more = !scan_complete || remaining_candidate_count > 0 || !skipped.is_empty();
    let runner_arg = shell_arg(&runner.id);
    let cursor_arg = (!scan_complete)
        .then(|| continuation_cursor.as_deref())
        .flatten()
        .map(|cursor| format!(" --cursor {}", shell_arg(cursor)))
        .unwrap_or_default();
    let next_command = has_more.then(|| {
        if options.apply {
            format!(
                "homeboy runner workspace prune {runner_arg} --apply --min-age-hours {} --limit {limit} --passes {passes}{cursor_arg}",
                options.min_age_hours
            )
        } else {
            format!(
                "homeboy runner workspace prune {runner_arg} --min-age-hours {} --limit {limit}{cursor_arg}",
                options.min_age_hours
            )
        }
    });
    let drain_command = format!(
        "homeboy runner workspace prune {runner_arg} --apply --min-age-hours {} --limit {limit} --passes 10{cursor_arg}",
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
            skipped_live_count,
            skipped_unknown_count,
            inspect_command: format!("homeboy runner status {runner_arg}"),
            stop_command: stop_commands.into_iter().next().unwrap_or_else(|| {
                format!("homeboy runner doctor {runner_arg} --scope lab-offload --repair")
            }),
            reconcile_command: format!("homeboy runner job reconcile {runner_arg}"),
            scanned_workspace_count,
            scan_complete,
            continuation_cursor: (!scan_complete).then_some(continuation_cursor).flatten(),
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
    scan_limit: usize,
    cursor: Option<&str>,
) -> Result<PruneCandidateScan> {
    match runner.kind {
        RunnerKind::Local => prune_candidates_local(
            runner,
            Path::new(lab_workspaces_root),
            options,
            scan_limit,
            cursor,
        ),
        RunnerKind::Ssh => {
            prune_candidates_ssh(runner, lab_workspaces_root, options, scan_limit, cursor)
        }
    }
}

struct PruneCandidateScan {
    candidates: Vec<RunnerWorkspacePruneEntry>,
    withheld: Vec<RunnerWorkspacePruneEntry>,
    scanned_workspace_count: usize,
    scan_complete: bool,
    continuation_cursor: Option<String>,
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
    fallback_reason: Option<&str>,
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
        fallback_reason: fallback_reason.map(str::to_string),
        snapshot_identity: snapshot_identity.to_string(),
        workspace_lease: Some(new_workspace_lease()),
        workspace_generation: 0,
        original_prepared_snapshot_identity: Some(snapshot_identity.to_string()),
        update_lineage: Vec::new(),
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
        terminal_evidence: None,
    }
}

fn new_workspace_lease() -> String {
    format!("workspace:{}", uuid::Uuid::new_v4())
}

/// A job-owned Git workspace has a stable path so retries can be correlated.
/// Seeing that path before materialization means another execution could still
/// be using it; reject rather than letting Git reset/clean a live cwd.
fn reject_existing_job_workspace(
    runner: &super::super::Runner,
    remote_path: &str,
    owner_run_id: Option<&str>,
) -> Result<()> {
    let Some(owner_run_id) = owner_run_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(());
    };
    let exists = match runner.kind {
        RunnerKind::Local => Path::new(remote_path).exists(),
        RunnerKind::Ssh => {
            let (_, client) = ssh_client_for_runner(runner)?;
            client
                .execute_with_timeout(
                    &format!("test -e {}", shell::quote_arg(remote_path)),
                    WORKSPACE_METADATA_TIMEOUT,
                )
                .success
        }
    };
    if !exists {
        return Ok(());
    }

    Err(Error::new(
        ErrorCode::RunnerWorkspaceOwnershipConflict,
        format!("Lab workspace `{remote_path}` is already owned by active job `{owner_run_id}`"),
        serde_json::json!({
            "runner_id": runner.id,
            "remote_path": remote_path,
            "owner_run_id": owner_run_id,
            "collision_stage": "pre_materialization",
        }),
    )
    .with_hint("Wait for the existing job to terminalize, or dispatch a new job identity."))
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

/// Update the lifecycle record stored beside an already materialized runner
/// workspace. Runner pruning reads this exact metadata, so failure retention
/// stays on the same bounded TTL cleanup path as every other Lab workspace.
pub(crate) fn update_workspace_resource_lifecycle(
    runner_id: &str,
    remote_path: &str,
    resource_lifecycle: ResourceLifecycleRecord,
) -> Result<()> {
    resource_lifecycle.validate(0)?;
    let runner = load(runner_id)?;
    let metadata_path = format!(
        "{}/{}",
        remote_path.trim_end_matches('/'),
        WORKSPACE_METADATA_FILE
    );
    let content = match runner.kind {
        RunnerKind::Local => fs::read_to_string(&metadata_path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {metadata_path}")))
        })?,
        RunnerKind::Ssh => {
            let (_, client) = ssh_client_for_runner(&runner)?;
            let output = client.execute_with_timeout(
                &format!("cat {}", shell::quote_arg(&metadata_path)),
                WORKSPACE_METADATA_TIMEOUT,
            );
            if !output.success {
                return Err(workspace_metadata_ssh_error(&output));
            }
            output.stdout
        }
    };
    let mut metadata: RunnerWorkspaceMetadata =
        serde_json::from_str(&content).map_err(|error| {
            Error::internal_json(error.to_string(), Some(format!("parse {metadata_path}")))
        })?;
    metadata.resource_lifecycle = Some(resource_lifecycle);
    write_workspace_metadata(&runner, metadata)
}

/// Persist the final disposition observed by the run-owned workspace handle.
///
/// Publishing uses the same temp-file-and-rename writer as lifecycle updates,
/// so readers see either the old metadata or one complete terminal record.
/// Repeating an identical terminal update is intentionally idempotent.
pub(crate) fn record_workspace_terminal_evidence(
    runner_id: &str,
    remote_path: &str,
    evidence: RunnerWorkspaceTerminalEvidence,
    lifecycle_status: ResourceLifecycleResourceStatus,
    relinquished: bool,
) -> Result<()> {
    let runner = load(runner_id)?;
    let metadata_path = format!(
        "{}/{}",
        remote_path.trim_end_matches('/'),
        WORKSPACE_METADATA_FILE
    );
    let content = match runner.kind {
        RunnerKind::Local => fs::read_to_string(&metadata_path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {metadata_path}")))
        })?,
        RunnerKind::Ssh => {
            let (_, client) = ssh_client_for_runner(&runner)?;
            let output = client.execute_with_timeout(
                &format!("cat {}", shell::quote_arg(&metadata_path)),
                WORKSPACE_METADATA_TIMEOUT,
            );
            if !output.success {
                return Err(workspace_metadata_ssh_error(&output));
            }
            output.stdout
        }
    };
    let mut metadata: RunnerWorkspaceMetadata =
        serde_json::from_str(&content).map_err(|error| {
            Error::internal_json(error.to_string(), Some(format!("parse {metadata_path}")))
        })?;
    if let Some(lifecycle) = metadata.resource_lifecycle.as_mut() {
        lifecycle.status = lifecycle_status;
        // A detached or uncertain daemon handoff still has a live job owner.
        // Prevent every automatic reclaimer from treating this workspace as a
        // terminal TTL artifact until that owner publishes a terminal result.
        if relinquished {
            lifecycle.cleanup_policy = ResourceCleanupPolicy::Preserve;
            lifecycle.ttl = None;
            lifecycle.cleanup_command = None;
        }
    }
    metadata.terminal_evidence = Some(evidence);
    write_workspace_metadata(&runner, metadata)
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
            let mut staged =
                tempfile::NamedTempFile::new_in(parent_or_current_dir(path)).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some("create workspace metadata staging file".to_string()),
                    )
                })?;
            use std::io::Write;
            staged.write_all(json.as_bytes()).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("write workspace metadata".to_string()),
                )
            })?;
            staged.persist(path).map_err(|err| {
                Error::internal_io(
                    err.error.to_string(),
                    Some("publish workspace metadata".to_string()),
                )
            })?;
            Ok(())
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

fn parent_or_current_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
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

/// Capacity needed while snapshot transport has both the archive-preparation
/// tree and the runner's atomic destination temporary alive.  The fixed margin
/// covers tar metadata and bounded control files; the multiplier deliberately
/// models the two live copies rather than relying on the destination mount's
/// aggregate free space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotFilesystemRequirement {
    bytes: u64,
    inodes: u64,
}

fn snapshot_filesystem_requirement(
    logical_bytes: u64,
    entry_count: u64,
) -> SnapshotFilesystemRequirement {
    const SAFETY_BYTES: u64 = 64 * 1024 * 1024;
    const SAFETY_INODES: u64 = 128;
    SnapshotFilesystemRequirement {
        bytes: logical_bytes.saturating_mul(2).saturating_add(SAFETY_BYTES),
        inodes: entry_count.saturating_mul(2).saturating_add(SAFETY_INODES),
    }
}

/// Refuse before the archive pipeline or runner extraction creates a partial
/// workspace. The controller scratch path and runner destination are probed
/// independently: a roomy root filesystem cannot mask a constrained `/tmp`.
fn require_snapshot_filesystem_admission(
    runner: &super::super::Runner,
    local_path: &Path,
    remote_path: &str,
    stats: &ByteFileCounts,
    entry_count: u64,
) -> Result<SnapshotFilesystemAdmission> {
    let requirement = snapshot_filesystem_requirement(stats.bytes, entry_count);
    let scratch = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // A stale or constrained configured TMPDIR is not an admission dead end.
    // The source parent is deterministic and avoids reconstructing an ad-hoc
    // shell environment; it is selected only when it passes the same probe.
    let fallback_scratch = local_path.parent().unwrap_or(local_path).to_path_buf();
    let scratch_probe = local_snapshot_filesystem_probe(&scratch, "controller snapshot scratch")?;
    let (selected_scratch, scratch_probe) =
        if snapshot_probe_has_capacity(&scratch_probe, requirement) {
            (scratch, scratch_probe)
        } else {
            let fallback_probe = local_snapshot_filesystem_probe(
                &fallback_scratch,
                "controller alternate snapshot scratch",
            )?;
            if !snapshot_probe_has_capacity(&fallback_probe, requirement) {
                return Err(snapshot_capacity_error(
                    &scratch_probe,
                    requirement,
                    runner,
                    Some(&fallback_scratch),
                ));
            }
            (fallback_scratch, fallback_probe)
        };
    let destination_probe = match runner.kind {
        RunnerKind::Local => {
            local_snapshot_filesystem_probe(Path::new(remote_path), "runner snapshot destination")?
        }
        RunnerKind::Ssh => ssh_snapshot_filesystem_probe(runner, remote_path)?,
    };
    let admission = SnapshotFilesystemAdmission::acquire(
        selected_scratch,
        &[scratch_probe, destination_probe],
        requirement,
        runner,
    )
    .map_err(|mut error| {
        error.retryable = Some(true);
        error.details["snapshot_logical_bytes"] = serde_json::json!(stats.bytes);
        error.details["snapshot_entry_count"] = serde_json::json!(entry_count);
        error.details["copy_amplification"] = serde_json::json!(2);
        error.details["safety_margin_bytes"] = serde_json::json!(64 * 1024 * 1024_u64);
        error.details["required_bytes"] = serde_json::json!(requirement.bytes);
        error.details["required_inodes"] = serde_json::json!(requirement.inodes);
        error
    })?;
    Ok(admission)
}

#[derive(Debug, Clone)]
struct SnapshotFilesystemProbe {
    identity: String,
    path: String,
    role: &'static str,
    available_bytes: u64,
    available_inodes: u64,
}

fn snapshot_probe_has_capacity(
    probe: &SnapshotFilesystemProbe,
    requirement: SnapshotFilesystemRequirement,
) -> bool {
    probe.available_bytes >= requirement.bytes && probe.available_inodes >= requirement.inodes
}

#[cfg(unix)]
fn local_snapshot_filesystem_probe(
    path: &Path,
    role: &'static str,
) -> Result<SnapshotFilesystemProbe> {
    let probe_path = existing_ancestor(path).ok_or_else(|| {
        Error::validation_invalid_argument(
            "snapshot_filesystem",
            "snapshot capacity probe has no existing path ancestor",
            Some(path.display().to_string()),
            None,
        )
    })?;
    let c_path = std::ffi::CString::new(probe_path.to_string_lossy().as_bytes()).map_err(|_| {
        Error::validation_invalid_argument(
            "snapshot_filesystem",
            "snapshot capacity probe path contains an interior NUL",
            Some(probe_path.display().to_string()),
            None,
        )
    })?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some("probe snapshot filesystem capacity".to_string()),
        ));
    }
    let stat = unsafe { stat.assume_init() };
    let available_bytes =
        u64::try_from(u128::from(stat.f_bavail).saturating_mul(u128::from(stat.f_frsize.max(1))))
            .unwrap_or(u64::MAX);
    let available_inodes = stat.f_favail as u64;
    Ok(SnapshotFilesystemProbe {
        identity: format!("local:{:?}", stat.f_fsid),
        path: probe_path.display().to_string(),
        role,
        available_bytes,
        available_inodes,
    })
}

#[cfg(not(unix))]
fn local_snapshot_filesystem_probe(
    _path: &Path,
    role: &'static str,
) -> Result<SnapshotFilesystemProbe> {
    Ok(SnapshotFilesystemProbe {
        identity: format!("local:{role}"),
        path: String::new(),
        role,
        available_bytes: u64::MAX,
        available_inodes: u64::MAX,
    })
}

fn ssh_snapshot_filesystem_probe(
    runner: &super::super::Runner,
    path: &str,
) -> Result<SnapshotFilesystemProbe> {
    let (_, client) = ssh_client_for_runner(runner)?;
    let command = format!(
        "p={}; while [ ! -e \"$p\" ] && [ \"$p\" != / ]; do p=$(dirname \"$p\"); done; df -Pk \"$p\" | awk 'NR==2 {{print $1 \" \" $4}}'; df -Pi \"$p\" | awk 'NR==2 {{print $4}}'",
        shell::quote_arg(path)
    );
    let output = client.execute_with_timeout(&command, WORKSPACE_PRUNE_TIMEOUT);
    let mut values = output.stdout.split_whitespace();
    let identity = values.next().unwrap_or("unknown");
    let available_bytes = values
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.saturating_mul(1024));
    let available_inodes = values.next().and_then(|value| value.parse::<u64>().ok());
    match (available_bytes, available_inodes) {
        (Some(available_bytes), Some(available_inodes)) => Ok(SnapshotFilesystemProbe {
            // A runner ID scopes a device name to its host. `df` device names
            // are only meaningful within one runner's namespace.
            identity: format!("runner:{}:{identity}", runner.id),
            path: path.to_string(),
            role: "runner snapshot destination",
            available_bytes,
            available_inodes,
        }),
        _ => Err(Error::internal_unexpected(
            "runner did not return a bounded filesystem capacity probe",
        )
        .with_retryable(true)),
    }
}

fn snapshot_capacity_error(
    probe: &SnapshotFilesystemProbe,
    requirement: SnapshotFilesystemRequirement,
    runner: &super::super::Runner,
    alternate_scratch: Option<&Path>,
) -> Error {
    let alternate_command = format!(
        "Retry with `TMPDIR={}` after ensuring that filesystem has capacity.",
        shell_arg(
            &alternate_scratch
                .unwrap_or_else(|| Path::new(&probe.path))
                .display()
                .to_string()
        )
    );
    let cleanup_command = format!(
        "Reclaim stale Lab workspaces with `homeboy runner workspace prune {} --apply`.",
        shell_arg(&runner.id)
    );
    let mut error = Error::validation_invalid_argument(
        "snapshot_filesystem",
        format!(
            "{} filesystem `{}` lacks capacity for snapshot materialization",
            probe.role, probe.identity
        ),
        Some(probe.path.clone()),
        Some(vec![alternate_command.clone(), cleanup_command.clone()]),
    )
    .with_hint(alternate_command)
    .with_hint(cleanup_command);
    error.retryable = Some(true);
    error.details["constrained_path"] = serde_json::json!(probe.path);
    error.details["filesystem_identity"] = serde_json::json!(probe.identity);
    error.details["available_bytes"] = serde_json::json!(probe.available_bytes);
    error.details["available_inodes"] = serde_json::json!(probe.available_inodes);
    error.details["required_bytes"] = serde_json::json!(requirement.bytes);
    error.details["required_inodes"] = serde_json::json!(requirement.inodes);
    error.details["active_reservations"] = serde_json::json!([]);
    error
}

const SNAPSHOT_RESERVATION_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SnapshotFilesystemReservationRecord {
    lease_id: String,
    controller_pid: u32,
    created_unix_seconds: u64,
    bytes: u64,
    inodes: u64,
}

/// Admission stays live only during materialization. The ledger survives an
/// interrupted controller so the next admission can recover a dead lease.
#[derive(Debug)]
struct SnapshotFilesystemAdmission {
    scratch: std::path::PathBuf,
    leases: Vec<(std::path::PathBuf, String)>,
}

impl SnapshotFilesystemAdmission {
    fn scratch(&self) -> &Path {
        &self.scratch
    }

    fn acquire(
        scratch: std::path::PathBuf,
        probes: &[SnapshotFilesystemProbe],
        requirement: SnapshotFilesystemRequirement,
        runner: &super::super::Runner,
    ) -> Result<Self> {
        let mut unique = std::collections::BTreeMap::new();
        for probe in probes {
            unique.entry(probe.identity.clone()).or_insert(probe);
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        let mut leases = Vec::new();
        for probe in unique.into_values() {
            let path = snapshot_reservation_path(&probe.identity)?;
            let _lock = snapshot_reservation_lock(&path)?;
            let mut records = read_snapshot_reservation_records(&path)?;
            records.retain(snapshot_reservation_is_live);
            let reserved_bytes = records.iter().map(|record| record.bytes).sum::<u64>();
            let reserved_inodes = records.iter().map(|record| record.inodes).sum::<u64>();
            if probe.available_bytes.saturating_sub(reserved_bytes) < requirement.bytes
                || probe.available_inodes.saturating_sub(reserved_inodes) < requirement.inodes
            {
                let mut error = snapshot_capacity_error(probe, requirement, runner, None);
                error.details["active_reservations"] = serde_json::json!(records);
                error.details["reserved_bytes"] = serde_json::json!(reserved_bytes);
                error.details["reserved_inodes"] = serde_json::json!(reserved_inodes);
                drop(leases);
                return Err(error);
            }
            records.push(SnapshotFilesystemReservationRecord {
                lease_id: lease_id.clone(),
                controller_pid: std::process::id(),
                created_unix_seconds: snapshot_reservation_now(),
                bytes: requirement.bytes,
                inodes: requirement.inodes,
            });
            write_snapshot_reservation_records_unlocked(&path, &records)?;
            leases.push((path, lease_id.clone()));
        }
        Ok(Self { scratch, leases })
    }
}

impl Drop for SnapshotFilesystemAdmission {
    fn drop(&mut self) {
        for (path, lease_id) in &self.leases {
            let Ok(_lock) = snapshot_reservation_lock(path) else {
                continue;
            };
            if let Ok(mut records) = read_snapshot_reservation_records(path) {
                records.retain(|record| {
                    record.lease_id != *lease_id && snapshot_reservation_is_live(record)
                });
                let _ = write_snapshot_reservation_records_unlocked(path, &records);
            }
        }
    }
}

fn snapshot_reservation_path(identity: &str) -> Result<std::path::PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    identity.hash(&mut hasher);
    let root = homeboy_core::paths::homeboy_data()?.join("snapshot-filesystem-reservations");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create snapshot reservation ledger".to_string()),
        )
    })?;
    Ok(root.join(format!("{:016x}.json", hasher.finish())))
}

#[cfg(test)]
fn snapshot_reservation_records(path: &Path) -> Result<Vec<SnapshotFilesystemReservationRecord>> {
    let _lock = snapshot_reservation_lock(path)?;
    read_snapshot_reservation_records(path)
}

fn read_snapshot_reservation_records(
    path: &Path,
) -> Result<Vec<SnapshotFilesystemReservationRecord>> {
    match fs::read_to_string(path) {
        Ok(value) => serde_json::from_str(&value).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse snapshot reservation ledger".to_string()),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some("read snapshot reservation ledger".to_string()),
        )),
    }
}

fn write_snapshot_reservation_records_unlocked(
    path: &Path,
    records: &[SnapshotFilesystemReservationRecord],
) -> Result<()> {
    let json = serde_json::to_vec(records).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("encode snapshot reservation ledger".to_string()),
        )
    })?;
    let mut staged =
        tempfile::NamedTempFile::new_in(parent_or_current_dir(path)).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("stage snapshot reservation ledger".to_string()),
            )
        })?;
    staged.write_all(&json).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write snapshot reservation ledger".to_string()),
        )
    })?;
    staged.persist(path).map_err(|error| {
        Error::internal_io(
            error.error.to_string(),
            Some("publish snapshot reservation ledger".to_string()),
        )
    })?;
    Ok(())
}

fn snapshot_reservation_lock(path: &Path) -> Result<std::fs::File> {
    let lock = path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("lock snapshot reservation ledger".to_string()),
            )
        })?;
    #[cfg(unix)]
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
        return Err(Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some("lock snapshot reservation ledger".to_string()),
        ));
    }
    Ok(file)
}

fn snapshot_reservation_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn snapshot_reservation_is_live(record: &SnapshotFilesystemReservationRecord) -> bool {
    let fresh = snapshot_reservation_now().saturating_sub(record.created_unix_seconds)
        < SNAPSHOT_RESERVATION_TTL.as_secs();
    fresh && snapshot_reservation_pid_is_live(record.controller_pid)
}

#[cfg(unix)]
fn snapshot_reservation_pid_is_live(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
fn snapshot_reservation_pid_is_live(_pid: u32) -> bool {
    true
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
    let output = client.execute_with_timeout(&command, WORKSPACE_PRUNE_TIMEOUT);
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
    runner: &super::super::Runner,
    root: &Path,
    options: &RunnerWorkspacePruneOptions,
    scan_limit: usize,
    cursor: Option<&str>,
) -> Result<PruneCandidateScan> {
    if !root.is_dir() {
        return Ok(PruneCandidateScan {
            candidates: Vec::new(),
            withheld: Vec::new(),
            scanned_workspace_count: 0,
            scan_complete: true,
            continuation_cursor: None,
        });
    }
    let after = cursor
        .map(|cursor| decode_prune_cursor(root, cursor))
        .transpose()?;
    let mut candidates = Vec::new();
    let mut withheld = Vec::new();
    let entries = fs::read_dir(root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read runner workspace root".to_string()),
        )
    })?;
    let mut paths = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read runner workspace entry".to_string()),
            )
        })?;
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if after.as_ref().is_some_and(|after| path <= *after) {
            continue;
        }
        paths.insert(path);
        if paths.len() > scan_limit.saturating_add(1) {
            paths.pop_last();
        }
    }
    let mut scan_complete = paths.len() <= scan_limit;
    if !scan_complete {
        paths.pop_last();
    }
    let inspected = paths.into_iter().collect::<Vec<_>>();
    let mut scanned_workspace_count = 0;
    let deadline = Instant::now() + WORKSPACE_PRUNE_SCAN_BUDGET;
    let mut last_inspected = None;
    for path in inspected {
        if last_inspected.is_some() && Instant::now() >= deadline {
            scan_complete = false;
            break;
        }
        last_inspected = Some(path.clone());
        scanned_workspace_count += 1;
        if let Some(candidate) = classify_local_candidate(runner, root, &path, options)? {
            if candidate.liveness.state == "inactive" {
                candidates.push(candidate);
            } else {
                withheld.push(candidate);
            }
        }
    }
    let continuation_cursor = (!scan_complete)
        .then(|| last_inspected.as_ref())
        .flatten()
        .map(|path| encode_prune_cursor(path));
    candidates.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| b.age_seconds.cmp(&a.age_seconds))
            .then_with(|| a.remote_path.cmp(&b.remote_path))
    });
    Ok(PruneCandidateScan {
        candidates,
        withheld,
        scanned_workspace_count,
        scan_complete,
        continuation_cursor,
    })
}

fn classify_local_candidate(
    runner: &super::super::Runner,
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
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&metadata) else {
        return Ok(None);
    };
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
    let size = bounded_directory_size(path);
    let liveness = match size {
        Some(_) => workspace_liveness(runner, &metadata, path),
        None => liveness(
            "unknown",
            vec!["workspace_size_measurement_unavailable".to_string()],
        ),
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
        bytes: size.unwrap_or(0),
        reason,
        liveness,
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

/// Bulk pruning is intentionally stricter than run-owned reaping.  Age and a
/// missing controller source identify an orphan candidate, but they cannot
/// prove that no independently surviving workload still owns its files.
fn workspace_liveness(
    runner: &super::super::Runner,
    metadata: &serde_json::Value,
    path: &Path,
) -> RunnerWorkspaceLivenessEvidence {
    if (metadata
        .get("job_id")
        .and_then(|value| value.as_str())
        .is_some()
        || metadata
            .get("run_id")
            .and_then(|value| value.as_str())
            .is_some())
        && metadata
            .get("resource_lifecycle")
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str())
            == Some("active")
    {
        return liveness("live", vec!["active_resource_lifecycle_lease".to_string()]);
    }

    if let Some(job_id) = metadata.get("job_id").and_then(|value| value.as_str()) {
        match super::super::status(&runner.id) {
            Ok(report)
                if report.active_job_state == super::super::RunnerActiveJobState::Available =>
            {
                if report.active_jobs.iter().any(|job| job.job_id == job_id) {
                    return liveness("live", vec![format!("active_runner_job:{job_id}")]);
                }
            }
            Ok(_) => return liveness("unknown", vec!["runner_job_probe_unavailable".to_string()]),
            Err(_) => return liveness("unknown", vec!["runner_job_probe_failed".to_string()]),
        }
    }

    match runner.kind {
        RunnerKind::Local => local_process_liveness(path),
        RunnerKind::Ssh => ssh_process_liveness(runner, &path.display().to_string()),
    }
}

fn revalidate_candidate_liveness(
    runner: &super::super::Runner,
    candidate: &RunnerWorkspacePruneEntry,
) -> Result<RunnerWorkspaceLivenessEvidence> {
    // Job state and process ownership can both change after the bounded scan.
    // Keep this check immediately adjacent to the delete call.
    let metadata = match runner.kind {
        RunnerKind::Local => {
            let path = Path::new(&candidate.remote_path).join(WORKSPACE_METADATA_FILE);
            let contents = fs::read(&path).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read workspace metadata".to_string()))
            })?;
            serde_json::from_slice(&contents).map_err(|err| {
                Error::internal_json(err.to_string(), Some(path.display().to_string()))
            })?
        }
        // Remote metadata and process ownership are revalidated atomically with
        // deletion below. Runner job state is controller-owned and checked here.
        RunnerKind::Ssh => return runner_job_liveness(runner, candidate.job_id.as_deref()),
    };
    Ok(workspace_liveness(
        runner,
        &metadata,
        Path::new(&candidate.remote_path),
    ))
}

fn runner_job_liveness(
    runner: &super::super::Runner,
    job_id: Option<&str>,
) -> Result<RunnerWorkspaceLivenessEvidence> {
    let Some(job_id) = job_id else {
        return Ok(liveness("inactive", Vec::new()));
    };
    match super::super::status(&runner.id) {
        Ok(report) if report.active_job_state == super::super::RunnerActiveJobState::Available => {
            if report.active_jobs.iter().any(|job| job.job_id == job_id) {
                Ok(liveness(
                    "live",
                    vec![format!("active_runner_job:{job_id}")],
                ))
            } else {
                Ok(liveness("inactive", Vec::new()))
            }
        }
        Ok(_) => Ok(liveness(
            "unknown",
            vec!["runner_job_probe_unavailable".to_string()],
        )),
        Err(_) => Ok(liveness(
            "unknown",
            vec!["runner_job_probe_failed".to_string()],
        )),
    }
}

fn liveness(state: &str, observations: Vec<String>) -> RunnerWorkspaceLivenessEvidence {
    RunnerWorkspaceLivenessEvidence {
        state: state.to_string(),
        observations,
    }
}

#[cfg(target_os = "linux")]
fn local_process_liveness(path: &Path) -> RunnerWorkspaceLivenessEvidence {
    const MAX_PROCESSES: usize = 4096;
    const MAX_FDS_PER_PROCESS: usize = 1024;
    let target = path.to_string_lossy();
    let Ok(processes) = fs::read_dir("/proc") else {
        return liveness(
            "unknown",
            vec!["process_probe_unavailable:/proc".to_string()],
        );
    };
    let mut seen = 0usize;
    for process in processes.flatten() {
        let pid = process.file_name();
        if pid.to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        seen += 1;
        if seen > MAX_PROCESSES {
            return liveness("unknown", vec!["process_probe_process_limit".to_string()]);
        }
        let process_path = process.path();
        let cwd = match fs::read_link(process_path.join("cwd")) {
            Ok(cwd) => cwd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return liveness("unknown", vec!["process_probe_cwd_failed".to_string()]),
        };
        let command = match fs::read(process_path.join("cmdline")) {
            Ok(command) => command,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return liveness("unknown", vec!["process_probe_argv_failed".to_string()]),
        };
        if cwd.starts_with(path) || String::from_utf8_lossy(&command).contains(target.as_ref()) {
            return liveness("live", vec![format!("process:{}", pid.to_string_lossy())]);
        }
        let fds = match fs::read_dir(process_path.join("fd")) {
            Ok(fds) => fds,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return liveness("unknown", vec!["process_probe_fd_failed".to_string()]),
        };
        for (index, fd) in fds.flatten().enumerate() {
            if index >= MAX_FDS_PER_PROCESS {
                return liveness("unknown", vec!["process_probe_fd_limit".to_string()]);
            }
            if fs::read_link(fd.path())
                .ok()
                .is_some_and(|open| open.starts_with(path))
            {
                return liveness(
                    "live",
                    vec![format!("process_open_file:{}", pid.to_string_lossy())],
                );
            }
        }
    }
    liveness("inactive", Vec::new())
}

#[cfg(not(target_os = "linux"))]
fn local_process_liveness(path: &Path) -> RunnerWorkspaceLivenessEvidence {
    let output = Command::new("lsof")
        .args(["-Fn", "+D", &path.display().to_string()])
        .output();
    match output {
        Ok(output) if output.status.success() && output.stdout.is_empty() => {
            liveness("inactive", Vec::new())
        }
        Ok(output) if !output.stdout.is_empty() => {
            liveness("live", vec!["process_open_file".to_string()])
        }
        // lsof uses status 1 for a clean no-match result. Diagnostics make the
        // result ambiguous, and must therefore prevent deletion.
        Ok(output) if output.status.code() == Some(1) && output.stderr.is_empty() => {
            liveness("inactive", Vec::new())
        }
        _ => liveness(
            "unknown",
            vec!["process_probe_unavailable:lsof".to_string()],
        ),
    }
}

fn ssh_process_liveness(
    runner: &super::super::Runner,
    path: &str,
) -> RunnerWorkspaceLivenessEvidence {
    let Ok((_server, mut client)) = ssh_client_for_runner(runner) else {
        return liveness(
            "unknown",
            vec!["process_probe_connection_failed".to_string()],
        );
    };
    client.env.extend(runner.env.clone());
    let command = ssh_process_liveness_command(path);
    let output = client.execute_with_timeout(&command, WORKSPACE_PRUNE_TIMEOUT);
    match output.stdout.trim() {
        "inactive" if output.success => liveness("inactive", Vec::new()),
        "live" if output.success => liveness("live", vec!["remote_process_ownership".to_string()]),
        _ => liveness("unknown", vec!["remote_process_probe_failed".to_string()]),
    }
}

pub(crate) fn ssh_process_liveness_command(path: &str) -> String {
    format!(
        "p={}; command -v ps >/dev/null 2>&1 && command -v lsof >/dev/null 2>&1 || {{ printf unknown; exit 0; }}; if ps -eo pid=,ppid=,args= | awk -v p=\"$p\" -v self=\"$$\" -v parent=\"$PPID\" '$1 != self && $1 != parent && $2 != self && index($0, p) {{ found=1 }} END {{ exit !found }}' || lsof -Fn -a -d cwd +D \"$p\" 2>/dev/null | grep -q . || lsof -Fn +D \"$p\" 2>/dev/null | grep -q .; then printf live; else printf inactive; fi",
        shell::quote_arg(path)
    )
}

fn prune_candidates_ssh(
    runner: &super::super::Runner,
    root: &str,
    options: &RunnerWorkspacePruneOptions,
    scan_limit: usize,
    cursor: Option<&str>,
) -> Result<PruneCandidateScan> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let min_age = options.min_age_hours.saturating_mul(3600);
    let after = cursor
        .map(|cursor| decode_prune_cursor(Path::new(root), cursor))
        .transpose()?;
    let command = prune_scan_command(root, min_age, scan_limit, after.as_deref());
    let output = client.execute_with_timeout(&command, WORKSPACE_PRUNE_TIMEOUT);
    if !output.success {
        return Err(Error::internal_unexpected(format!(
            "runner workspace prune scan failed: {}",
            output.stderr.trim()
        )));
    }
    let mut candidates = Vec::new();
    let mut withheld = Vec::new();
    let mut scan_status = None;
    for line in output.stdout.lines() {
        if let Some(status) = line.strip_prefix("__homeboy_prune_scan__\t") {
            let parts = status.split('\t').collect::<Vec<_>>();
            let [scanned_workspace_count, completion, last_inspected] = parts.as_slice() else {
                return Err(Error::internal_unexpected(
                    "runner workspace prune scan returned an invalid completion status".to_string(),
                ));
            };
            let scanned_workspace_count = scanned_workspace_count.parse().map_err(|err| {
                Error::internal_unexpected(format!(
                    "runner workspace prune scan returned an invalid inspected count: {err}"
                ))
            })?;
            let scan_complete = match *completion {
                "complete" => true,
                "partial" => false,
                _ => {
                    return Err(Error::internal_unexpected(
                        "runner workspace prune scan returned an invalid completion status"
                            .to_string(),
                    ));
                }
            };
            let continuation_cursor = match (*completion, *last_inspected) {
                ("complete", "") => None,
                ("partial", path) if !path.is_empty() => Some(encode_prune_cursor(Path::new(path))),
                _ => {
                    return Err(Error::internal_unexpected(
                        "runner workspace prune scan returned an invalid continuation cursor"
                            .to_string(),
                    ))
                }
            };
            scan_status = Some((scanned_workspace_count, scan_complete, continuation_cursor));
            continue;
        }
        let parts = line.splitn(5, '\t').collect::<Vec<_>>();
        if parts.len() != 5 {
            continue;
        }
        let age_seconds = parts[0].parse::<u64>().unwrap_or(0);
        let bytes = parts[1].parse::<u64>().unwrap_or(0);
        let remote_path = parts[2].to_string();
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(parts[3]) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&decoded) else {
            continue;
        };
        if metadata.get("schema").and_then(|value| value.as_str())
            != Some("homeboy/runner-workspace/v1")
        {
            continue;
        }
        let Some(source_path) = metadata.get("local_path").and_then(|value| value.as_str()) else {
            continue;
        };
        let reason = prune_candidate_reason_from_decoded_metadata(&metadata, age_seconds);
        let Some(reason) = reason else {
            continue;
        };
        let liveness = if parts[4] == "measured" {
            workspace_liveness(runner, &metadata, Path::new(&parts[2]))
        } else {
            liveness(
                "unknown",
                vec!["workspace_size_measurement_unavailable".to_string()],
            )
        };
        let entry = RunnerWorkspacePruneEntry {
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
            liveness,
        };
        if entry.liveness.state == "inactive" {
            candidates.push(entry);
        } else {
            withheld.push(entry);
        }
    }
    candidates.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| b.age_seconds.cmp(&a.age_seconds))
            .then_with(|| a.remote_path.cmp(&b.remote_path))
    });
    let (scanned_workspace_count, scan_complete, continuation_cursor) =
        scan_status.ok_or_else(|| {
            Error::internal_unexpected(
                "runner workspace prune scan did not return a completion status".to_string(),
            )
        })?;
    Ok(PruneCandidateScan {
        candidates,
        withheld,
        scanned_workspace_count,
        scan_complete,
        continuation_cursor,
    })
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

pub(crate) fn prune_scan_command(
    root: &str,
    min_age: u64,
    scan_limit: usize,
    after: Option<&Path>,
) -> String {
    format!(
        "root={root}; after={after}; meta_rel={meta}; min_age={min_age}; now=$(date +%s); deadline=$((now + {scan_budget})); if [ -d \"$root\" ]; then LC_ALL=C find \"$root\" -mindepth 1 -maxdepth 1 -type d -print | LC_ALL=C sort | {{ scanned=0; complete=complete; last=; while IFS= read -r dir; do [ -z \"$after\" ] || [ \"$dir\" \\> \"$after\" ] || continue; if [ \"$scanned\" -ge {scan_limit} ] || {{ [ \"$scanned\" -gt 0 ] && [ \"$(date +%s)\" -ge \"$deadline\" ]; }}; then complete=partial; break; fi; scanned=$((scanned + 1)); last=$dir; meta=\"$dir/$meta_rel\"; [ -f \"$meta\" ] || continue; mtime=$(stat -c %Y \"$dir\" 2>/dev/null || stat -f %m \"$dir\" 2>/dev/null || echo 0); age=$((now-mtime)); [ \"$age\" -ge \"$min_age\" ] || continue; if find \"$dir/.homeboy\" -type f \\( -name \"*.patch\" -o -name \"*.diff\" -o -name \"*patch*\" \\) 2>/dev/null | grep -q .; then continue; fi; blocks=; size_file=$(mktemp \"${{TMPDIR:-/tmp}}/homeboy-prune.XXXXXX\") || size_file=; if [ -n \"$size_file\" ]; then du -sk \"$dir\" > \"$size_file\" 2>/dev/null & measure_pid=$!; {{ sleep {size_timeout}; kill \"$measure_pid\" 2>/dev/null; }} & measure_watchdog=$!; wait \"$measure_pid\"; measure_status=$?; kill \"$measure_watchdog\" 2>/dev/null; wait \"$measure_watchdog\" 2>/dev/null; [ \"$measure_status\" -eq 0 ] && blocks=$(cat \"$size_file\"); rm -f \"$size_file\"; fi; blocks=${{blocks%%[!0-9]*}}; bytes=$((blocks * 1024)); measurement=measured; [ -n \"$blocks\" ] || measurement=unknown; printf \"%s\\t%s\\t%s\\t\" \"$age\" \"${{bytes:-0}}\" \"$dir\"; base64 < \"$meta\" | tr -d \"\\n\"; printf \"\\t%s\\n\" \"$measurement\"; done; [ \"$complete\" = partial ] || last=; printf \"__homeboy_prune_scan__\\t%s\\t%s\\t%s\\n\" \"$scanned\" \"$complete\" \"$last\"; }}; else printf \"__homeboy_prune_scan__\\t0\\tcomplete\\t\\n\"; fi",
        root = shell::quote_arg(root),
        after = shell::quote_arg(after.and_then(Path::to_str).unwrap_or("")),
        meta = shell::quote_arg(WORKSPACE_METADATA_FILE),
        min_age = shell::quote_arg(&min_age.to_string()),
        scan_limit = scan_limit,
        scan_budget = WORKSPACE_PRUNE_SCAN_BUDGET.as_secs(),
        size_timeout = WORKSPACE_PRUNE_SIZE_TIMEOUT.as_secs(),
    )
}

const PRUNE_CURSOR_PREFIX: &str = "homeboy-workspace-prune/v1\0";

fn encode_prune_cursor(path: &Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{PRUNE_CURSOR_PREFIX}{}", path.display()))
}

fn decode_prune_cursor(root: &Path, cursor: &str) -> Result<std::path::PathBuf> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| invalid_prune_cursor())?;
    let decoded = String::from_utf8(decoded).map_err(|_| invalid_prune_cursor())?;
    let path = decoded
        .strip_prefix(PRUNE_CURSOR_PREFIX)
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .ok_or_else(invalid_prune_cursor)?;
    if path.parent() != Some(root) || path.file_name().is_none() {
        return Err(invalid_prune_cursor());
    }
    Ok(path.to_path_buf())
}

fn invalid_prune_cursor() -> Error {
    Error::validation_invalid_argument(
        "cursor",
        "workspace prune cursor is invalid for this workspace root",
        None,
        None,
    )
}

fn remove_prune_candidate(
    runner: &super::super::Runner,
    root: &str,
    candidate: &RunnerWorkspacePruneEntry,
) -> Result<Option<RunnerWorkspaceLivenessEvidence>> {
    match runner.kind {
        RunnerKind::Local => {
            remove_workspace(runner, root, &candidate.remote_path)?;
            Ok(None)
        }
        RunnerKind::Ssh => remove_ssh_prune_candidate(runner, root, &candidate.remote_path),
    }
}

fn remove_ssh_prune_candidate(
    runner: &super::super::Runner,
    root: &str,
    remote_path: &str,
) -> Result<Option<RunnerWorkspaceLivenessEvidence>> {
    let (_server, mut client) = ssh_client_for_runner(runner)?;
    client.env.extend(runner.env.clone());
    let output = client.execute_with_timeout(
        &ssh_prune_delete_command(root, remote_path),
        WORKSPACE_PRUNE_TIMEOUT,
    );
    if !output.success {
        return Err(Error::internal_unexpected(format!(
            "remove runner workspace failed: {}",
            output.stderr.trim()
        )));
    }
    match output.stdout.trim() {
        "removed" => Ok(None),
        "live:active_resource_lifecycle_lease" => Ok(Some(liveness(
            "live",
            vec!["active_resource_lifecycle_lease".to_string()],
        ))),
        "live:remote_process_ownership" => Ok(Some(liveness(
            "live",
            vec!["remote_process_ownership".to_string()],
        ))),
        state if state.starts_with("unknown:") => Ok(Some(liveness(
            "unknown",
            vec![state.trim_start_matches("unknown:").to_string()],
        ))),
        _ => Ok(Some(liveness(
            "unknown",
            vec!["remote_process_probe_failed".to_string()],
        ))),
    }
}

pub(crate) fn ssh_prune_delete_command(root: &str, remote_path: &str) -> String {
    format!(
        "root={root}; p={path}; meta_rel={meta}; case \"$p\" in \"$root\"/*) ;; *) printf unknown:workspace_path; exit 0 ;; esac; meta=\"$p/$meta_rel\"; [ -f \"$meta\" ] && grep -Eq '\"schema\"[[:space:]]*:[[:space:]]*\"homeboy/runner-workspace/v1\"' \"$meta\" || {{ printf unknown:metadata; exit 0; }}; if grep -Eq '\"status\"[[:space:]]*:[[:space:]]*\"active\"' \"$meta\"; then printf live:active_resource_lifecycle_lease; exit 0; fi; command -v ps >/dev/null 2>&1 && command -v lsof >/dev/null 2>&1 || {{ printf unknown:process_probe_unavailable; exit 0; }}; ps_output=$(ps -eo pid=,ppid=,args=) || {{ printf unknown:process_probe_failed; exit 0; }}; printf '%s\\n' \"$ps_output\" | awk -v p=\"$p\" -v self=\"$$\" -v parent=\"$PPID\" '$1 != self && $1 != parent && $2 != self && index($0, p) {{ found=1 }} END {{ exit !found }}'; state=$?; [ \"$state\" -eq 0 ] && {{ printf live:remote_process_ownership; exit 0; }}; [ \"$state\" -eq 1 ] || {{ printf unknown:process_probe_failed; exit 0; }}; cwd=$(lsof -Fn -a -d cwd +D \"$p\" 2>&1); state=$?; [ \"$state\" -eq 0 ] && [ -n \"$cwd\" ] && {{ printf live:remote_process_ownership; exit 0; }}; {{ [ \"$state\" -eq 1 ] && [ -z \"$cwd\" ]; }} || {{ printf unknown:process_probe_failed; exit 0; }}; open=$(lsof -Fn +D \"$p\" 2>&1); state=$?; [ \"$state\" -eq 0 ] && [ -n \"$open\" ] && {{ printf live:remote_process_ownership; exit 0; }}; {{ [ \"$state\" -eq 1 ] && [ -z \"$open\" ]; }} || {{ printf unknown:process_probe_failed; exit 0; }}; rm -rf -- \"$p\" && printf removed || printf unknown:delete_failed",
        root = shell::quote_arg(root),
        path = shell::quote_arg(remote_path),
        meta = shell::quote_arg(WORKSPACE_METADATA_FILE),
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
            let output = client.execute_with_timeout(&command, WORKSPACE_PRUNE_TIMEOUT);
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

fn bounded_directory_size(path: &Path) -> Option<u64> {
    let command = format!("du -sk {}", shell::quote_arg(&path.display().to_string()));
    let output = execute_local_command_in_dir_with_timeout(
        &command,
        None,
        None,
        WORKSPACE_PRUNE_SIZE_TIMEOUT,
    );
    output.success.then_some(())?;
    output
        .stdout
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .map(|blocks| blocks.saturating_mul(1024))
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

pub(crate) use crate::shell_quote::shell_arg;

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

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn existing_job_owned_workspace_is_rejected_before_git_materialization() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("runner root");
            crate::create(
                &format!(
                    r#"{{"id":"workspace-ownership-conflict","kind":"local","workspace_root":"{}"}}"#,
                    root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let remote_path = root.path().join("_lab_workspaces/existing-job");
            fs::create_dir_all(&remote_path).expect("existing workspace");
            let runner = load("workspace-ownership-conflict").expect("load runner");

            let error = reject_existing_job_workspace(
                &runner,
                &remote_path.display().to_string(),
                Some("job-1"),
            )
            .expect_err("active job path must not be overwritten");

            assert_eq!(error.code, ErrorCode::RunnerWorkspaceOwnershipConflict);
            assert_eq!(error.details["collision_stage"], "pre_materialization");
            assert!(remote_path.exists());
        });
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
        runner_workspace_disk_is_critical, snapshot_capacity_error,
        snapshot_filesystem_requirement, snapshot_reservation_path, snapshot_reservation_records,
        RunnerWorkspaceDiskProbe, SnapshotFilesystemAdmission, SnapshotFilesystemProbe,
        SnapshotFilesystemRequirement,
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

    #[test]
    fn snapshot_requirement_models_two_live_copies_and_inode_margin() {
        let requirement = snapshot_filesystem_requirement(5 * 1024 * 1024 * 1024, 257_000);
        assert_eq!(
            requirement.bytes,
            10 * 1024 * 1024 * 1024 + 64 * 1024 * 1024
        );
        assert_eq!(requirement.inodes, 514_128);
    }

    #[test]
    fn snapshot_requirement_rejects_inode_exhaustion_independently_of_bytes() {
        let requirement = snapshot_filesystem_requirement(1024, 10_000);
        assert!(requirement.bytes < 64 * 1024 * 1024 + 4096);
        assert!(requirement.inodes > 10_000);
    }

    fn probe(identity: &str, bytes: u64, inodes: u64) -> SnapshotFilesystemProbe {
        SnapshotFilesystemProbe {
            identity: identity.to_string(),
            path: format!("/{identity}"),
            role: "snapshot test",
            available_bytes: bytes,
            available_inodes: inodes,
        }
    }

    fn reservation_runner() -> crate::Runner {
        serde_json::from_value(serde_json::json!({ "id": "snapshot-reservation", "kind": "local" }))
            .expect("runner")
    }

    #[test]
    fn snapshot_admission_reports_constrained_tmpfs_with_retry_actions() {
        let runner = reservation_runner();
        let requirement = SnapshotFilesystemRequirement {
            bytes: 100,
            inodes: 100,
        };
        let error =
            snapshot_capacity_error(&probe("tmpfs", 10_000, 10), requirement, &runner, None);

        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["filesystem_identity"], "tmpfs");
        assert_eq!(error.details["constrained_path"], "/tmpfs");
        assert_eq!(error.details["required_inodes"], 100);
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("TMPDIR=")));
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("workspace prune")));
    }

    #[test]
    fn snapshot_reservations_prevent_overcommit_then_release() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let runner = reservation_runner();
            let requirement = SnapshotFilesystemRequirement {
                bytes: 100,
                inodes: 100,
            };
            let filesystem = probe("tmpfs-concurrent", 150, 150);
            let first = SnapshotFilesystemAdmission::acquire(
                tempfile::tempdir().expect("scratch").keep(),
                &[filesystem.clone()],
                requirement,
                &runner,
            )
            .expect("first reservation");
            let error = SnapshotFilesystemAdmission::acquire(
                tempfile::tempdir().expect("scratch").keep(),
                &[filesystem.clone()],
                requirement,
                &runner,
            )
            .expect_err("second reservation must not overcommit");
            assert_eq!(error.retryable, Some(true));
            assert_eq!(
                error.details["active_reservations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            drop(first);
            SnapshotFilesystemAdmission::acquire(
                tempfile::tempdir().expect("scratch").keep(),
                &[filesystem],
                requirement,
                &runner,
            )
            .expect("released reservation admits retry");
        });
    }

    #[test]
    fn snapshot_reservation_reclaims_dead_lease() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let runner = reservation_runner();
            let requirement = SnapshotFilesystemRequirement {
                bytes: 100,
                inodes: 100,
            };
            let filesystem = probe("root-recovery", 150, 150);
            let path = snapshot_reservation_path(&filesystem.identity).expect("ledger path");
            let _lock = super::snapshot_reservation_lock(&path).expect("lock");
            super::write_snapshot_reservation_records_unlocked(
                &path,
                &[super::SnapshotFilesystemReservationRecord {
                    lease_id: "dead".to_string(),
                    controller_pid: u32::MAX,
                    created_unix_seconds: 0,
                    bytes: 100,
                    inodes: 100,
                }],
            )
            .expect("write stale lease");
            drop(_lock);

            let admission = SnapshotFilesystemAdmission::acquire(
                tempfile::tempdir().expect("scratch").keep(),
                &[filesystem],
                requirement,
                &runner,
            )
            .expect("dead lease must be recovered");
            assert_eq!(
                snapshot_reservation_records(&path).expect("records").len(),
                1
            );
            drop(admission);
            assert!(snapshot_reservation_records(&path)
                .expect("records")
                .is_empty());
        });
    }
}
