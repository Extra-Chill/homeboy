//! Lab-workspace provenance provider hook.
//!
//! When the agent-task scheduler harvests a cook attempt's workspace it must
//! verify the lab-materialized workspace's provenance (and, for git-mode,
//! synthesize a baseline). That verification is coupled to the runner's
//! workspace-snapshot machinery, but runner is an optional Lab-offload feature,
//! so core defines the [`LabWorkspaceProvenanceProvider`] contract here and the
//! runner layer registers an implementation at startup.
//!
//! With no provider registered there is no lab workspace to verify, so the
//! [`NoopLabWorkspaceProvenanceProvider`] reports a clear error — this path is
//! only reached when a snapshot was actually signaled by a runner.

use std::path::Path;
use std::sync::Mutex;

use crate::source_snapshot::SourceSnapshot;

/// The verified provenance of a lab-materialized workspace, slimmed to the
/// fields the scheduler records. The full provenance stays runner-internal.
#[derive(Debug, Clone, Default)]
pub struct LabWorkspaceProvenanceInfo {
    pub source_revision: String,
    pub materialization_mode: String,
    pub runner_id: String,
    pub workspace_identity: String,
    pub snapshot_hash: String,
}

/// The lab-workspace provenance contract the agent-task scheduler depends on.
/// Implemented by the runner layer and registered at startup.
pub trait LabWorkspaceProvenanceProvider: Send + Sync {
    /// Verify a materialized lab workspace and return its provenance. When
    /// `require_git_root` is set, also verify the workspace's git-root metadata
    /// (the paired `verify_lab_workspace_git_root` check).
    fn verify_lab_workspace(
        &self,
        expected_remote_component_path: &str,
        materialized_workspace_path: &Path,
        snapshot: SourceSnapshot,
        lab: serde_json::Value,
        require_git_root: bool,
    ) -> std::result::Result<LabWorkspaceProvenanceInfo, String>;

    /// Materialize a synthetic Git baseline for a verified snapshot workspace,
    /// returning the baseline commit.
    fn materialize_verified_lab_snapshot_git_baseline(
        &self,
        expected_remote_component_path: &str,
        materialized_workspace_path: &Path,
        snapshot: SourceSnapshot,
        lab: serde_json::Value,
    ) -> std::result::Result<String, String>;

    /// Verify a lab-materialized workspace using the lab-offload and
    /// source-snapshot metadata carried in the process environment (the
    /// `verify_lab_workspace_from_env` check used by trace canonicality).
    fn verify_lab_workspace_from_env(
        &self,
        expected_remote_component_path: &str,
        materialized_workspace_path: &Path,
    ) -> std::result::Result<LabWorkspaceProvenanceInfo, String>;
}

/// Default provider used when no runner layer is registered. The verification
/// path is only reached after a runner signaled a snapshot, so without a
/// provider there is nothing to verify and both methods report a clear error.
struct NoopLabWorkspaceProvenanceProvider;

const NO_PROVIDER: &str =
    "no runner is registered to verify the lab-materialized workspace provenance";

impl LabWorkspaceProvenanceProvider for NoopLabWorkspaceProvenanceProvider {
    fn verify_lab_workspace(
        &self,
        _expected_remote_component_path: &str,
        _materialized_workspace_path: &Path,
        _snapshot: SourceSnapshot,
        _lab: serde_json::Value,
        _require_git_root: bool,
    ) -> std::result::Result<LabWorkspaceProvenanceInfo, String> {
        Err(NO_PROVIDER.to_string())
    }

    fn materialize_verified_lab_snapshot_git_baseline(
        &self,
        _expected_remote_component_path: &str,
        _materialized_workspace_path: &Path,
        _snapshot: SourceSnapshot,
        _lab: serde_json::Value,
    ) -> std::result::Result<String, String> {
        Err(NO_PROVIDER.to_string())
    }

    fn verify_lab_workspace_from_env(
        &self,
        _expected_remote_component_path: &str,
        _materialized_workspace_path: &Path,
    ) -> std::result::Result<LabWorkspaceProvenanceInfo, String> {
        Err(NO_PROVIDER.to_string())
    }
}

static PROVIDER: Mutex<Option<Box<dyn LabWorkspaceProvenanceProvider>>> = Mutex::new(None);

/// Register the lab-workspace provenance provider. Called once at startup by the
/// runner layer (via the CLI).
pub fn register_lab_workspace_provenance_provider(
    provider: Box<dyn LabWorkspaceProvenanceProvider>,
) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Run `f` against the registered provider, or the no-op provider if none is
/// registered.
pub fn with_lab_workspace_provenance<T>(
    f: impl FnOnce(&dyn LabWorkspaceProvenanceProvider) -> T,
) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopLabWorkspaceProvenanceProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_verify_errors_clearly() {
        let noop = NoopLabWorkspaceProvenanceProvider;
        let err = noop
            .verify_lab_workspace(
                "path",
                Path::new("/tmp/x"),
                SourceSnapshot::default(),
                serde_json::Value::Null,
                false,
            )
            .expect_err("must error without a provider");
        assert!(err.contains("no runner is registered"));
    }

    #[test]
    fn noop_materialize_errors_clearly() {
        let noop = NoopLabWorkspaceProvenanceProvider;
        let err = noop
            .materialize_verified_lab_snapshot_git_baseline(
                "path",
                Path::new("/tmp/x"),
                SourceSnapshot::default(),
                serde_json::Value::Null,
            )
            .expect_err("must error without a provider");
        assert!(err.contains("no runner is registered"));
    }
}
