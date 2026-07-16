//! Observation-side implementation of the audit recorded-artifact provider.
//!
//! The audit engine (`code_audit`) defines `AuditRecordedArtifactProvider` and
//! calls it without depending on the observation store. This module implements
//! that trait by opening the store, listing a component's recent runs and their
//! artifacts, and projecting each into the slim view audit's portability
//! detector needs. It is registered at binary startup by the CLI, mirroring the
//! manifest / runner-evidence provider hooks.

use crate::code_audit::recorded_artifacts::{
    register_audit_recorded_artifact_provider, AuditRecordedArtifact,
    AuditRecordedArtifactProvider, AuditRecordedRun,
};
use crate::observation::{ObservationStore, RunListFilter};

struct StoreArtifactProvider;

impl AuditRecordedArtifactProvider for StoreArtifactProvider {
    fn recent_runs(&self, component_id: &str, limit: usize) -> Vec<AuditRecordedRun> {
        let Ok(store) = ObservationStore::open_initialized() else {
            return Vec::new();
        };
        let Ok(runs) = store.list_runs(RunListFilter {
            kind: None,
            component_id: Some(component_id.to_string()),
            status: None,
            rig_id: None,
            limit: Some(limit as i64),
        }) else {
            return Vec::new();
        };

        runs.into_iter()
            .map(|run| {
                let artifacts = store
                    .list_artifacts(&run.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|artifact| AuditRecordedArtifact {
                        id: artifact.id,
                        kind: artifact.kind,
                        artifact_type: artifact.artifact_type,
                        path: artifact.path,
                    })
                    .collect();
                AuditRecordedRun {
                    id: run.id,
                    command: run.command,
                    metadata_json: run.metadata_json,
                    artifacts,
                }
            })
            .collect()
    }
}

/// Register the observation-backed recorded-artifact provider. Called once at
/// binary startup by the CLI.
pub fn register() {
    register_audit_recorded_artifact_provider(Box::new(StoreArtifactProvider));
}
