//! Runner workspace synchronization.
//!
//! Materializes a controller-side source checkout into a deterministic remote
//! runner workspace, either by streaming a filtered snapshot tarball or by
//! shipping a git bundle / cloning the remote. Split into concern-focused
//! submodules; this module re-exports the surface consumed elsewhere in the
//! runner subsystem so the historical `workspace::*` paths keep resolving.

mod git;
mod pull;
mod snapshot;
mod sync;
mod types;
mod util;

pub use pull::{plan_workspace_pull, pull_workspace};
pub use sync::sync_workspace;
pub use sync::{list_workspaces, prune_workspaces};
pub use types::{
    ByteFileCounts, RunnerWorkspaceCurrentSummary, RunnerWorkspaceListEntry,
    RunnerWorkspaceListOutput, RunnerWorkspacePruneEntry, RunnerWorkspacePruneOptions,
    RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry, RunnerWorkspacePullOptions,
    RunnerWorkspacePullOutput, RunnerWorkspacePullPlan, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

pub(crate) use snapshot::{
    copy_snapshot_to_directory, effective_snapshot_excludes, local_snapshot_stats,
    materialize_snapshot, materialize_snapshot_git, snapshot_identity,
};
pub(crate) use types::{canonical_workspace_path, DEFAULT_EXCLUDES};
pub(crate) use util::{git_output, parent_remote_path, sanitize_path_segment};

#[cfg(test)]
mod tests;
