//! Workspace-snapshot hook.
//!
//! Materializing a local directory snapshot into a prepared directory (an
//! archive-and-extract copy that honors exclude globs) is runner-workspace
//! machinery: it shares the runner's tar-pipe materializer with SSH/local
//! offload. Core's hygiene subsystem needs the same local-copy operation to
//! run validation-dependency lifecycles in an isolated workspace, so instead
//! of depending on the runner subsystem directly it calls this provider.
//!
//! When the runner subsystem is absent no provider is registered and the
//! [`NoopProvider`] errors clearly.

use std::path::Path;

use crate::error::{Error, Result};

/// Local workspace-snapshot materialization the hygiene subsystem depends on.
pub trait WorkspaceSnapshotProvider: Send + Sync {
    /// Archive the directory at `local_path` and extract it into `destination`,
    /// honoring the given exclude globs.
    fn copy_snapshot_to_directory(
        &self,
        local_path: &Path,
        destination: &Path,
        excludes: &[String],
    ) -> Result<()>;
}

/// Default provider used when the runner subsystem is not present.
struct NoopProvider;

impl WorkspaceSnapshotProvider for NoopProvider {
    fn copy_snapshot_to_directory(
        &self,
        _local_path: &Path,
        _destination: &Path,
        _excludes: &[String],
    ) -> Result<()> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot materialize a local workspace snapshot",
        ))
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn WorkspaceSnapshotProvider,
    noop: NoopProvider,
    /// Register the workspace-snapshot provider. Called once at startup by the
    /// runner subsystem when it is present.
    register: pub fn register_workspace_snapshot_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// Copy a local directory snapshot into `destination`, delegating to the
/// registered provider (or erroring via the no-op provider when absent).
pub(crate) fn copy_snapshot_to_directory(
    local_path: &Path,
    destination: &Path,
    excludes: &[String],
) -> Result<()> {
    with_provider(|p| p.copy_snapshot_to_directory(local_path, destination, excludes))
}
