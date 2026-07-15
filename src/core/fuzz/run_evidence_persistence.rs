//! Fuzz run-evidence persistence.
//!
//! Boundary: the caller (a command adapter) assembles the presentation-facing
//! inputs — the `RunRecord` with its metadata, the optional result envelope, and
//! the artifact paths — as plain data. This module owns the store orchestration:
//! upserting the run, recording every fuzz artifact, persisting the coverage
//! reconciliation and result envelope, and recording post-process outputs. This
//! keeps the store-write sequence out of the command layer.

use std::path::Path;

use crate::core::artifact_ref::EvidenceRef;
use crate::core::artifacts::{record_artifact_postprocess_outputs, ArtifactPostprocessOutput};
use crate::core::fuzz::{
    fuzz_result_envelope_evidence_ref, persist_fuzz_coverage_reconciliation,
    persist_fuzz_run_result_envelope, FuzzCampaign, FuzzResultEnvelope,
    FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_SEQUENCE_PLAN_SCHEMA,
};
use crate::core::observation::{ObservationStore, RunRecord};

/// Plain-data inputs for persisting fuzz run evidence.
///
/// The command adapter builds the `RunRecord`, the gated result envelope, and
/// resolves the artifact paths; this struct carries them across the boundary
/// without any command-layer types.
pub struct FuzzRunEvidence<'a> {
    /// The fully-assembled run record to upsert.
    pub run: RunRecord,
    /// Path to the fuzz results file (recorded when present).
    pub results_path: &'a Path,
    /// Path to the persisted execution request (recorded + reconciled).
    pub execution_request_path: Option<&'a Path>,
    /// Path to the persisted sequence plan (recorded when present).
    pub sequence_plan_path: Option<&'a Path>,
    /// Directory of runner-produced artifacts (recorded when present).
    pub artifacts_dir: &'a Path,
    /// Parsed campaign, used for coverage reconciliation and envelope status.
    pub campaign: Option<&'a FuzzCampaign>,
    /// Gated result envelope to persist (built by the caller from the campaign).
    pub envelope: Option<FuzzResultEnvelope>,
    /// Artifact refs missing from the campaign, recorded on the artifacts dir.
    pub missing_artifact_refs: &'a [String],
    /// Generic artifact post-process outputs to record.
    pub postprocess: Vec<ArtifactPostprocessOutput>,
}

/// Persist fuzz run evidence to the observation store, returning the run id and
/// any evidence references produced while recording the result envelope.
pub fn persist_fuzz_run_evidence(
    evidence: FuzzRunEvidence<'_>,
) -> crate::core::Result<(String, Vec<EvidenceRef>)> {
    let store = ObservationStore::open_initialized()?;
    let run_id = evidence.run.id.clone();
    store.upsert_imported_run(&evidence.run)?;

    let mut evidence_refs = Vec::new();
    if evidence.results_path.is_file() {
        store.record_artifact(&run_id, "fuzz_results", evidence.results_path)?;
    }
    if let Some(execution_request_path) = evidence.execution_request_path {
        if execution_request_path.is_file() {
            store.record_artifact_with_metadata(
                &run_id,
                "fuzz_execution_request",
                execution_request_path,
                serde_json::json!({
                    "schema": FUZZ_EXECUTION_REQUEST_SCHEMA,
                    "source": "HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE",
                }),
            )?;
        }
    }
    if let Some(sequence_plan_path) = evidence.sequence_plan_path {
        if sequence_plan_path.is_file() {
            store.record_artifact_with_metadata(
                &run_id,
                "fuzz_sequence_plan",
                sequence_plan_path,
                serde_json::json!({
                    "schema": FUZZ_SEQUENCE_PLAN_SCHEMA,
                    "source": "--sequence-plan",
                }),
            )?;
        }
    }
    if let Some(campaign) = evidence.campaign {
        if let Some(execution_request_path) = evidence.execution_request_path {
            persist_fuzz_coverage_reconciliation(
                &store,
                &run_id,
                execution_request_path,
                campaign,
            )?;
        }
    }
    if let Some(envelope) = evidence.envelope.as_ref() {
        if let Some(artifact) = persist_fuzz_run_result_envelope(Some(&run_id), envelope)? {
            evidence_refs.push(fuzz_result_envelope_evidence_ref(&artifact));
        }
    }
    if evidence.artifacts_dir.is_dir() {
        store.record_directory_artifact_with_metadata(
            &run_id,
            "fuzz_artifacts",
            evidence.artifacts_dir,
            serde_json::json!({
                "source": "HOMEBOY_FUZZ_ARTIFACTS_DIR",
                "missing_artifact_refs": evidence.missing_artifact_refs,
            }),
        )?;
    }
    record_artifact_postprocess_outputs(&store, &run_id, &evidence.postprocess)?;

    Ok((run_id, evidence_refs))
}
