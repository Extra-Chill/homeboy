//! Runner-side implementation of core's `WorkspaceSnapshotProvider` hook.
//!
//! Core's hygiene subsystem materializes a local directory snapshot when
//! running validation-dependency lifecycles in isolation. That copy is the
//! runner's tar-pipe snapshot machinery, so this adapter delegates to
//! `snapshot::copy_snapshot_to_directory` without core depending on runner
//! behavior directly.

use std::path::Path;

use crate::error::Result;
use crate::workspace_snapshot::WorkspaceSnapshotProvider;

/// The runner layer's `WorkspaceSnapshotProvider`. Registered with core at startup.
pub struct RunnerWorkspaceSnapshot;

impl WorkspaceSnapshotProvider for RunnerWorkspaceSnapshot {
    fn copy_snapshot_to_directory(
        &self,
        local_path: &Path,
        destination: &Path,
        excludes: &[String],
    ) -> Result<()> {
        super::snapshot::copy_snapshot_to_directory(local_path, destination, excludes)
    }
}

/// Register the runner workspace-snapshot provider with core. Called once at startup.
pub fn register() {
    crate::workspace_snapshot::register_workspace_snapshot_provider(Box::new(
        RunnerWorkspaceSnapshot,
    ));
}
