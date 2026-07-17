//! Runner-side implementation of core's `LabWorkspaceProvenanceProvider` hook.
//!
//! Core's agent-task scheduler calls this contract to verify a lab-materialized
//! workspace's provenance without depending on runner behavior. This adapter
//! delegates to the runner's workspace-provenance functions and maps the full
//! runner provenance onto the slim core-facing type.

use std::path::Path;

use homeboy_core::agent_task_scheduler::lab_workspace_provenance::{
    LabWorkspaceProvenanceInfo, LabWorkspaceProvenanceProvider,
};
use homeboy_core::source_snapshot::SourceSnapshot;

/// The runner layer's `LabWorkspaceProvenanceProvider`. Registered with core at
/// startup.
pub struct LabWorkspaceProvenance;

impl LabWorkspaceProvenanceProvider for LabWorkspaceProvenance {
    fn verify_lab_workspace(
        &self,
        expected_remote_component_path: &str,
        materialized_workspace_path: &Path,
        snapshot: SourceSnapshot,
        lab: serde_json::Value,
        require_git_root: bool,
    ) -> std::result::Result<LabWorkspaceProvenanceInfo, String> {
        let provenance = super::workspace::verify_lab_workspace(
            expected_remote_component_path,
            materialized_workspace_path,
            snapshot,
            lab,
        )?;
        if require_git_root {
            super::workspace::verify_lab_workspace_git_root(
                materialized_workspace_path,
                &provenance,
            )?;
        }
        Ok(LabWorkspaceProvenanceInfo {
            source_revision: provenance.source_revision,
            materialization_mode: provenance.materialization_mode,
            runner_id: provenance.runner_id,
            workspace_identity: provenance.workspace_identity,
            snapshot_hash: provenance.snapshot_hash,
        })
    }

    fn materialize_verified_lab_snapshot_git_baseline(
        &self,
        expected_remote_component_path: &str,
        materialized_workspace_path: &Path,
        snapshot: SourceSnapshot,
        lab: serde_json::Value,
    ) -> std::result::Result<String, String> {
        super::workspace::materialize_verified_lab_snapshot_git_baseline(
            expected_remote_component_path,
            materialized_workspace_path,
            snapshot,
            lab,
        )
    }

    fn verify_lab_workspace_from_env(
        &self,
        expected_remote_component_path: &str,
        materialized_workspace_path: &Path,
    ) -> std::result::Result<LabWorkspaceProvenanceInfo, String> {
        let provenance = super::workspace::verify_lab_workspace_from_env(
            expected_remote_component_path,
            materialized_workspace_path,
        )?;
        Ok(LabWorkspaceProvenanceInfo {
            source_revision: provenance.source_revision,
            materialization_mode: provenance.materialization_mode,
            runner_id: provenance.runner_id,
            workspace_identity: provenance.workspace_identity,
            snapshot_hash: provenance.snapshot_hash,
        })
    }
}

/// Register the lab-workspace provenance provider with core. Called once at
/// startup.
pub fn register() {
    homeboy_core::agent_task_scheduler::lab_workspace_provenance::register_lab_workspace_provenance_provider(
        Box::new(LabWorkspaceProvenance),
    );
}
