//! Recorded-run artifact access for the audit engine, inverted behind a
//! provider.
//!
//! The `artifact_portability` detector checks that artifacts recorded by past
//! runs use portable (relative / root-anchored) paths. It reads the recorded
//! runs + their artifacts from the observation store — which coupled
//! `code_audit` to the `observation` subsystem and blocked extracting audit
//! into its own crate.
//!
//! Instead, audit defines the slim view it needs (`AuditRecordedRun` +
//! `AuditRecordedArtifact`) plus a provider trait; the observation layer
//! registers an implementation at startup (same pattern as the manifest / runner
//! evidence provider hooks). When no provider is registered — e.g. audit running
//! standalone — the no-op provider yields no runs, which the detector already
//! treats as "nothing recorded to check".

use std::sync::Mutex;

/// A recorded artifact, projected to what the portability detector needs.
#[derive(Debug, Clone, Default)]
pub struct AuditRecordedArtifact {
    pub id: String,
    pub kind: String,
    pub artifact_type: String,
    pub path: String,
}

/// A recorded run plus its artifacts, projected for the portability detector.
#[derive(Debug, Clone, Default)]
pub struct AuditRecordedRun {
    pub id: String,
    pub command: Option<String>,
    pub metadata_json: serde_json::Value,
    pub artifacts: Vec<AuditRecordedArtifact>,
}

/// The recorded-artifact contract the audit engine depends on. Implemented by
/// the observation layer and registered at startup; audit calls it without
/// depending on observation behavior.
pub trait AuditRecordedArtifactProvider: Send + Sync {
    /// Return the most recent recorded runs (with their artifacts) for a
    /// component, newest first, up to `limit`.
    fn recent_runs(&self, component_id: &str, limit: usize) -> Vec<AuditRecordedRun>;
}

/// Default provider used when no observation layer is registered: no recorded
/// runs, so the portability detector reports nothing — exactly as it does today
/// when the observation store can't be opened.
struct NoopProvider;

impl AuditRecordedArtifactProvider for NoopProvider {
    fn recent_runs(&self, _component_id: &str, _limit: usize) -> Vec<AuditRecordedRun> {
        Vec::new()
    }
}

static PROVIDER: Mutex<Option<Box<dyn AuditRecordedArtifactProvider>>> = Mutex::new(None);

/// Register the recorded-artifact provider. Called once at binary startup by the
/// observation layer (via the CLI). Replaces any previously registered provider.
pub fn register_audit_recorded_artifact_provider(provider: Box<dyn AuditRecordedArtifactProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Recent recorded runs (with artifacts) for a component via the registered
/// provider.
pub(crate) fn recent_recorded_runs(component_id: &str, limit: usize) -> Vec<AuditRecordedRun> {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => provider.recent_runs(component_id, limit),
        None => NoopProvider.recent_runs(component_id, limit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_yields_no_runs() {
        let noop = NoopProvider;
        assert!(noop.recent_runs("any", 10).is_empty());
    }
}
