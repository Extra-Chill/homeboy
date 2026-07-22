//! Runner workspace synchronization.
//!
//! Materializes a controller-side source checkout into a deterministic remote
//! runner workspace, either by streaming a filtered snapshot tarball or by
//! shipping a git bundle / cloning the remote. Split into concern-focused
//! submodules; this module re-exports the surface consumed elsewhere in the
//! runner subsystem so the historical `workspace::*` paths keep resolving.

mod git;
mod materialized;
mod materializer;
mod provenance;
mod pull;
mod snapshot;
mod snapshot_provider;
mod sync;
mod types;
mod util;

pub use pull::{plan_workspace_pull, pull_workspace};
#[cfg(test)]
pub(crate) use sync::workspace_resource_lifecycle;
pub use sync::{
    hydrate_prepared_workspace_source_snapshot, list_workspaces, prune_workspaces,
    reap_run_workspace, reuse_compatible_snapshot_workspace, workspace_snapshots,
};
pub use sync::{sync_workspace, update_workspace};
pub use types::{
    ByteFileCounts, RunnerWorkspaceCurrentSummary, RunnerWorkspaceListEntry,
    RunnerWorkspaceListOutput, RunnerWorkspaceMaterializationContract,
    RunnerWorkspaceMaterializationPlan, RunnerWorkspaceOutputPaths, RunnerWorkspacePruneEntry,
    RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry,
    RunnerWorkspacePullOptions, RunnerWorkspacePullOutput, RunnerWorkspacePullPlan,
    RunnerWorkspaceSnapshotAppliedFilters, RunnerWorkspaceSnapshotEntry,
    RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSnapshotsOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput, RunnerWorkspaceUpdateOptions,
    RunnerWorkspaceUpdateOutput,
};

pub(crate) use materialized::{MaterializedWorkspace, WorkspaceCleanupPolicy};
pub(crate) use materializer::{
    dependency_cache_manifest_command, dependency_cache_restore_command,
    dependency_cache_save_command,
};
pub(crate) use provenance::{
    materialize_verified_lab_snapshot_git_baseline, verify_lab_workspace,
    verify_lab_workspace_from_env, verify_lab_workspace_git_root, VerifiedLabWorkspaceProvenance,
};
pub(crate) use snapshot::{
    copy_snapshot_to_directory, effective_snapshot_excludes, local_snapshot_stats,
    materialize_snapshot, materialize_snapshot_git, snapshot_identity, workspace_content_hash,
    workspace_content_hash_algorithm, workspace_content_manifest_for_policy,
    WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
};
pub use snapshot::{WorkspaceContentManifest, WorkspaceContentManifestEntry};
pub use snapshot_provider::register as register_workspace_snapshot_provider;
pub(crate) use types::{canonical_workspace_path, DEFAULT_EXCLUDES};
pub(crate) use util::{
    git_output, parent_remote_path, run_shell_capture, run_shell_command, sanitize_path_segment,
    shell_command_for_runner,
};

#[cfg(test)]
mod tests;
