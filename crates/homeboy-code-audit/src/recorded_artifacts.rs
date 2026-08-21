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
//!
//! # Rooting (#7505)
//!
//! The runs a provider returns and the artifact root those runs' paths are
//! anchored to are ONE fact, not two. A detector that reads runs from here and
//! separately resolves `homeboy_paths::artifact_root()` compares an injected
//! root against an ambiently-read store the moment anything upstream is rooted
//! — and then every stored artifact reads as non-portable. So the provider
//! answers both, and `RecordedRunScan` carries them out of a single registry
//! acquisition so they cannot be torn apart by a concurrent re-register.

use std::path::PathBuf;

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

    /// The artifact root the runs returned by [`Self::recent_runs`] are indexed
    /// against, or `None` when it cannot be resolved.
    ///
    /// Deliberately has no default implementation: an implementor that reads
    /// runs from a rooted store but lets the detector fall back to the ambient
    /// root produces the split this method exists to prevent, and a defaulted
    /// `None` would let that mistake compile (#7505).
    ///
    /// `None` is a real answer — "I cannot resolve one" — and the detector
    /// degrades on it rather than failing.
    fn artifact_root(&self) -> Option<PathBuf>;
}

/// Runs plus the artifact root they are anchored to, read from one provider in
/// one registry acquisition. See the module docs for why these travel together.
pub(crate) struct RecordedRunScan {
    pub(crate) runs: Vec<AuditRecordedRun>,
    pub(crate) artifact_root: Option<PathBuf>,
}

/// Default provider used when no observation layer is registered: no recorded
/// runs, so the portability detector reports nothing — exactly as it does today
/// when the observation store can't be opened.
struct NoopProvider;

impl AuditRecordedArtifactProvider for NoopProvider {
    fn recent_runs(&self, _component_id: &str, _limit: usize) -> Vec<AuditRecordedRun> {
        Vec::new()
    }

    /// No runs, so no root to anchor them to. The detector never consults this
    /// because the run list it accompanies is empty.
    fn artifact_root(&self) -> Option<PathBuf> {
        None
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn AuditRecordedArtifactProvider,
    noop: NoopProvider,
    /// Register the recorded-artifact provider. Called once at binary startup by the
    /// observation layer (via the CLI). Replaces any previously registered provider.
    register: pub fn register_audit_recorded_artifact_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// Recent recorded runs (with artifacts) for a component, plus the artifact
/// root they are anchored to, via the registered provider.
///
/// Both come from ONE `with_provider` call: two calls could straddle a
/// `register_audit_recorded_artifact_provider` from another thread and pair one
/// provider's runs with another provider's root.
pub(crate) fn recent_recorded_run_scan(component_id: &str, limit: usize) -> RecordedRunScan {
    with_provider(|p| RecordedRunScan {
        runs: p.recent_runs(component_id, limit),
        artifact_root: p.artifact_root(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_yields_no_runs() {
        let noop = NoopProvider;
        assert!(noop.recent_runs("any", 10).is_empty());
    }

    #[test]
    fn noop_provider_resolves_no_artifact_root() {
        assert!(NoopProvider.artifact_root().is_none());
    }
}
