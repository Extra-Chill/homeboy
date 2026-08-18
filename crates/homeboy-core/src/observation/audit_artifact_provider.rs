//! Observation-side implementation of the audit recorded-artifact provider.
//!
//! The audit engine (`code_audit`) defines `AuditRecordedArtifactProvider` and
//! calls it without depending on the observation store. This module implements
//! that trait by opening the store, listing a component's recent runs and their
//! artifacts, and projecting each into the slim view audit's portability
//! detector needs. It is registered at binary startup by the CLI, mirroring the
//! manifest / runner-evidence provider hooks.
//!
//! # Rooting (#7505)
//!
//! The registry itself stays process-global: it exists so the observation layer
//! can be swapped in at startup, and nothing about that is ambient state. What
//! WAS ambient is the value registered into it — a unit struct that reopened
//! `ObservationStore::open_initialized()` on every call. So the roots live on
//! the registered provider instead: [`register_in_roots`] pins one
//! [`PathRoots`], and both halves of the answer (the runs and the artifact root
//! they are anchored to) then come from that same value.

use std::path::PathBuf;

use crate::paths::PathRoots;
use crate::Result;

use crate::code_audit::recorded_artifacts::{
    register_audit_recorded_artifact_provider, AuditRecordedArtifact,
    AuditRecordedArtifactProvider, AuditRecordedRun,
};
use crate::observation::{ObservationStore, RunListFilter};

/// Store-backed provider, optionally pinned to injected roots.
///
/// `roots: None` is the ambient boundary and is what the CLI registers today.
/// `roots: Some(_)` reads runs from the database under `roots.data()` and
/// reports `roots.artifacts()` as the artifact root, so a caller auditing an
/// injected home never mixes the two.
struct StoreArtifactProvider {
    roots: Option<PathRoots>,
}

impl StoreArtifactProvider {
    /// Open the store this provider reads from, honoring its pinned roots.
    ///
    /// `open_initialized_in_roots` roots BOTH the database and artifact
    /// resolution, so a rooted provider cannot end up indexing an injected
    /// artifact tree with an ambient database (or vice versa).
    fn open_store(&self) -> Result<ObservationStore> {
        match &self.roots {
            Some(roots) => ObservationStore::open_initialized_in_roots(roots),
            None => ObservationStore::open_initialized(),
        }
    }
}

impl AuditRecordedArtifactProvider for StoreArtifactProvider {
    fn recent_runs(&self, component_id: &str, limit: usize) -> Vec<AuditRecordedRun> {
        let Ok(store) = self.open_store() else {
            return Vec::new();
        };
        let Ok(runs) = store.list_runs(RunListFilter {
            kind: None,
            component_id: Some(component_id.to_string()),
            status: None,
            rig_id: None,
            limit: Some(limit as i64),
            ..RunListFilter::default()
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

    /// The artifact root the runs from [`Self::recent_runs`] are anchored to.
    ///
    /// A rooted provider answers from its own `PathRoots` — the identical value
    /// `open_store` hands `ObservationStore::open_initialized_in_roots`, which
    /// is in turn exactly what `ObservationStore::artifact_root()` would report
    /// for that store. Resolving it here avoids a second store open (and its
    /// startup artifact maintenance) purely to read a path back out.
    ///
    /// An ambient provider answers from `paths::artifact_root()`, which is
    /// verbatim the fallback an ambiently-opened `ObservationStore` uses, so
    /// this stays the root the runs were actually written under.
    ///
    /// `None` (unresolvable ambient root) is a degrade signal, not an error:
    /// the detector drops the root-anchored exemption and keeps scanning.
    fn artifact_root(&self) -> Option<PathBuf> {
        match &self.roots {
            Some(roots) => Some(roots.artifacts().to_path_buf()),
            None => crate::paths::artifact_root().ok(),
        }
    }
}

/// Register the observation-backed recorded-artifact provider against the
/// ambient path boundary. Called once at binary startup by the CLI.
pub fn register() {
    register_audit_recorded_artifact_provider(Box::new(StoreArtifactProvider { roots: None }));
}

/// Register the observation-backed recorded-artifact provider against injected
/// roots.
///
/// This is the counterpart a caller must use before handing
/// `artifact_portability` an injected artifact root: the detector's root and
/// the store the runs come from then derive from the same `PathRoots`, instead
/// of comparing an injected root against an ambient database — which would flag
/// every recorded artifact as non-portable (#7505).
pub fn register_in_roots(roots: &PathRoots) {
    register_audit_recorded_artifact_provider(Box::new(StoreArtifactProvider {
        roots: Some(roots.clone()),
    }));
}
