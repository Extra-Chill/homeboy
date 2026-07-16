//! Fuzz result-envelope persistence.
//!
//! Boundary: the caller computes the `FuzzResultEnvelope` and (optionally) the
//! run id and an on-disk envelope path. This module owns the persistence
//! orchestration: opening the observation store, recording the envelope as a
//! run artifact (materializing a temp file when no path is supplied), encoding
//! the envelope to JSON, and building the evidence reference. Command modules
//! stay thin adapters that delegate here.

use std::path::Path;

use crate::artifact_ref::EvidenceRef;
use crate::fuzz::FuzzResultEnvelope;
use crate::observation::{ArtifactRecord, ObservationStore};

/// Observation-store artifact kind for a persisted fuzz result envelope.
pub const FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND: &str = "fuzz_result_envelope";

/// Write (optionally) and persist a `homeboy fuzz report` result envelope,
/// returning the evidence references for any persisted artifact.
///
/// When `envelope_path` is supplied the envelope JSON is written there first;
/// the envelope is then recorded as a run artifact when `run_id` refers to a
/// known run. This owns the full report-time persistence concern so the command
/// module stays a thin adapter.
pub fn report_fuzz_result_envelope(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
    envelope_path: Option<&Path>,
) -> crate::Result<Vec<EvidenceRef>> {
    if let Some(path) = envelope_path {
        let json = fuzz_result_envelope_json(envelope)?;
        std::fs::write(path, json).map_err(|error| {
            crate::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
    }
    let persisted = persist_fuzz_result_envelope(run_id, envelope, envelope_path)?;
    Ok(persisted
        .as_ref()
        .map(fuzz_result_envelope_evidence_ref)
        .into_iter()
        .collect())
}

/// Persist a fuzz result envelope produced by `homeboy fuzz report`.
pub fn persist_fuzz_result_envelope(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
    envelope_path: Option<&Path>,
) -> crate::Result<Option<ArtifactRecord>> {
    persist_fuzz_result_envelope_with_source(run_id, envelope, envelope_path, "homeboy fuzz report")
}

/// Persist a fuzz result envelope produced by `homeboy fuzz run`.
pub fn persist_fuzz_run_result_envelope(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
) -> crate::Result<Option<ArtifactRecord>> {
    persist_fuzz_result_envelope_with_source(run_id, envelope, None, "homeboy fuzz run")
}

fn persist_fuzz_result_envelope_with_source(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
    envelope_path: Option<&Path>,
    source: &str,
) -> crate::Result<Option<ArtifactRecord>> {
    let Some(run_id) = run_id.filter(|run_id| !run_id.trim().is_empty()) else {
        return Ok(None);
    };
    let store = ObservationStore::open_initialized()?;
    if store.get_run(run_id)?.is_none() {
        return Ok(None);
    }

    if let Some(path) = envelope_path.filter(|path| path.is_file()) {
        return record_fuzz_result_envelope_artifact(&store, run_id, path, envelope, source);
    }

    let mut artifact_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .map_err(|error| {
            crate::Error::internal_io(
                error.to_string(),
                Some("create temporary fuzz result envelope artifact".to_string()),
            )
        })?;
    serde_json::to_writer_pretty(&mut artifact_file, envelope).map_err(|error| {
        crate::Error::internal_unexpected(format!("failed to encode fuzz result envelope: {error}"))
    })?;
    record_fuzz_result_envelope_artifact(&store, run_id, artifact_file.path(), envelope, source)
}

fn record_fuzz_result_envelope_artifact(
    store: &ObservationStore,
    run_id: &str,
    path: &Path,
    envelope: &FuzzResultEnvelope,
    source: &str,
) -> crate::Result<Option<ArtifactRecord>> {
    let metadata = serde_json::json!({
        "schema": envelope.schema.as_str(),
        "envelope_id": envelope.id.as_str(),
        "status": envelope.status.as_str(),
        "campaign_id": envelope.campaign.as_ref().map(|campaign| campaign.id.as_str()),
        "source": source,
        "evidence": {
            "role": "result",
            "semantic_key": "fuzz.result_envelope",
        },
    });
    store
        .record_artifact_with_metadata(run_id, FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND, path, metadata)
        .map(Some)
}

/// Build the evidence reference for a persisted fuzz result-envelope artifact.
pub fn fuzz_result_envelope_evidence_ref(artifact: &ArtifactRecord) -> EvidenceRef {
    EvidenceRef::for_artifact(
        artifact,
        "Fuzz result envelope",
        Some("result".to_string()),
        Some("fuzz.result_envelope".to_string()),
    )
}

/// Encode a fuzz result envelope to pretty JSON.
pub fn fuzz_result_envelope_json(envelope: &FuzzResultEnvelope) -> crate::Result<String> {
    serde_json::to_string_pretty(envelope).map_err(|error| {
        crate::Error::internal_unexpected(format!("failed to encode fuzz result envelope: {error}"))
    })
}
