//! Fuzz run-evidence persistence onto the shared observation lifecycle.
//!
//! Boundary: the command adapter owns the run lifecycle through the shared
//! [`ActiveObservation`] (started before the workload executes, like bench,
//! trace, test and lint). This module owns what fuzz adds on top of it: the
//! fuzz artifact kinds, the coverage reconciliation, the gated result envelope,
//! the campaign findings recorded into the shared findings store, and the
//! terminal write guarded by the structured evidence contract.

use std::path::Path;

use crate::{
    fuzz_result_envelope_evidence_ref, persist_fuzz_coverage_reconciliation,
    persist_fuzz_run_result_envelope, FuzzCampaign, FuzzFinding, FuzzResultEnvelope,
    FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_SEQUENCE_PLAN_SCHEMA,
};
use homeboy_core::artifact_ref::EvidenceRef;
use homeboy_core::artifacts::{record_artifact_postprocess_outputs, ArtifactPostprocessOutput};
use homeboy_core::finding::{FindingSource, HomeboyFinding};
use homeboy_core::observation::{
    finding_records_from_homeboy_findings, ActiveObservation, NewFindingRecord, RunRecord,
    RunStatus,
};

/// Findings-store tool name for fuzz campaign findings.
pub const FUZZ_FINDING_TOOL: &str = "fuzz";

/// Plain-data inputs for terminalizing a fuzz run observation.
///
/// The command adapter computes the terminal status and metadata, the gated
/// result envelope and the artifact paths; this struct carries them across the
/// boundary without any command-layer types.
pub struct FuzzRunEvidence<'a> {
    /// Terminal status of the run.
    pub status: RunStatus,
    /// Terminal metadata, merged over the metadata the run was started with.
    pub metadata: serde_json::Value,
    /// Path to the fuzz results file (recorded when present).
    pub results_path: &'a Path,
    /// Path to the persisted execution request (recorded + reconciled).
    pub execution_request_path: Option<&'a Path>,
    /// Path to the persisted sequence plan (recorded when present).
    pub sequence_plan_path: Option<&'a Path>,
    /// Directory of runner-produced artifacts (recorded when present).
    pub artifacts_dir: &'a Path,
    /// Parsed campaign, used for coverage reconciliation and findings.
    pub campaign: Option<&'a FuzzCampaign>,
    /// Gated result envelope to persist (built by the caller from the campaign).
    pub envelope: Option<FuzzResultEnvelope>,
    /// Generic artifact post-process outputs to record.
    pub postprocess: Vec<ArtifactPostprocessOutput>,
    /// Content-addressed bodies externalized from the campaign before projection.
    pub payloads: &'a [crate::FuzzPayload],
}

/// Persisted outcome of a fuzz run: the terminal run row and the evidence
/// references produced while recording the result envelope.
#[derive(Debug, Clone)]
pub struct FuzzRunPersisted {
    pub run: RunRecord,
    pub evidence_refs: Vec<EvidenceRef>,
}

/// Record fuzz evidence on a running observation and terminalize it.
///
/// The structured evidence contract is checked before anything is written. A
/// contradictory contract never reaches the store: the run is terminalized as
/// an error carrying only the rejection, and the error is returned.
pub fn persist_fuzz_run_evidence(
    observation: &ActiveObservation,
    evidence: FuzzRunEvidence<'_>,
) -> homeboy_core::Result<FuzzRunPersisted> {
    let terminal_metadata = homeboy_core::observation::merge_metadata(
        observation.initial_metadata().clone(),
        evidence.metadata.clone(),
    );
    if let Err(error) = crate::FuzzEvidenceContract::from_run_metadata(&terminal_metadata) {
        observation.finish_error_with_merged_metadata(serde_json::json!({
            "status": "error",
            "error": error.message,
        }));
        return Err(error);
    }

    let store = observation.store();
    let run_id = observation.run_id().to_string();
    let mut evidence_refs = Vec::new();
    if evidence.results_path.is_file() {
        store.record_artifact(&run_id, "fuzz_results", evidence.results_path)?;
    }
    for payload in evidence.payloads {
        store.record_artifact_with_id(
            &run_id,
            crate::FUZZ_PAYLOAD_ARTIFACT_KIND,
            &payload.path,
            &payload.id,
            serde_json::json!({
                "schema": "homeboy/fuzz-payload/v1",
                "sha256": payload.sha256,
                "size_bytes": payload.size_bytes,
                "redaction": "inherited",
            }),
        )?;
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
            persist_fuzz_coverage_reconciliation(store, &run_id, execution_request_path, campaign)?;
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
            }),
        )?;
    }
    record_artifact_postprocess_outputs(store, &run_id, &evidence.postprocess)?;
    if let Some(campaign) = evidence.campaign {
        store.record_findings(&fuzz_finding_records(&run_id, campaign))?;
    }

    let run = observation.try_finish_with_merged_metadata(evidence.status, evidence.metadata)?;
    Ok(FuzzRunPersisted { run, evidence_refs })
}

/// Project a campaign's findings into shared findings-store records.
///
/// The typed `homeboy/fuzz-finding/v1` envelope rides along unchanged as the
/// `fuzz_finding` metadata payload, so readers of the shared store lose none of
/// the fuzz-specific fields (status, case/seed/workload ids, artifact ids).
pub fn fuzz_finding_records(run_id: &str, campaign: &FuzzCampaign) -> Vec<NewFindingRecord> {
    finding_records_from_homeboy_findings(
        run_id,
        campaign
            .findings
            .iter()
            .map(|finding| fuzz_homeboy_finding(&campaign.id, finding)),
    )
}

fn fuzz_homeboy_finding(campaign_id: &str, finding: &FuzzFinding) -> HomeboyFinding {
    let mut builder = HomeboyFinding::builder(FUZZ_FINDING_TOOL, finding.title.clone())
        .category("fuzz")
        .severity(finding.severity.clone())
        .fingerprint(
            finding
                .fingerprint
                .clone()
                .unwrap_or_else(|| finding.id.clone()),
        )
        .source(FindingSource::new("fuzz").label(campaign_id))
        .metadata("fuzz_finding", finding);
    if let Some(rule) = finding.operation_id.as_ref().or(finding.target_id.as_ref()) {
        builder = builder.rule(rule.clone());
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn campaign_with_findings() -> FuzzCampaign {
        serde_json::from_value(serde_json::json!({
            "schema": crate::FUZZ_CAMPAIGN_SCHEMA,
            "version": crate::FUZZ_CONTRACT_VERSION,
            "id": "campaign-1",
            "safety_class": "read_only",
            "findings": [
                {
                    "schema": crate::FUZZ_FINDING_SCHEMA,
                    "id": "finding-1",
                    "title": "parser panics on empty input",
                    "severity": "high",
                    "status": "open",
                    "operation_id": "parse",
                    "case_id": "case-7",
                    "fingerprint": "panic:parse"
                },
                {
                    "schema": crate::FUZZ_FINDING_SCHEMA,
                    "id": "finding-2",
                    "title": "slow path",
                    "severity": "low",
                    "status": "mitigated",
                    "target_id": "target-a"
                }
            ]
        }))
        .expect("campaign")
    }

    #[test]
    fn fuzz_finding_records_project_every_campaign_finding_with_its_typed_envelope() {
        let records = fuzz_finding_records("run-1", &campaign_with_findings());

        assert_eq!(records.len(), 2);
        let open = &records[0];
        assert_eq!(open.run_id, "run-1");
        assert_eq!(open.tool, FUZZ_FINDING_TOOL);
        assert_eq!(open.rule.as_deref(), Some("parse"));
        assert_eq!(open.severity.as_deref(), Some("high"));
        assert_eq!(open.fingerprint.as_deref(), Some("panic:parse"));
        assert_eq!(open.message, "parser panics on empty input");
        assert_eq!(open.metadata_json["category"], "fuzz");
        assert_eq!(open.metadata_json["source"]["label"], "campaign-1");
        assert_eq!(open.metadata_json["fuzz_finding"]["case_id"], "case-7");
        assert_eq!(open.metadata_json["fuzz_finding"]["status"], "open");

        let mitigated = &records[1];
        assert_eq!(mitigated.rule.as_deref(), Some("target-a"));
        assert_eq!(mitigated.fingerprint.as_deref(), Some("finding-2"));
        assert_eq!(
            mitigated.metadata_json["fuzz_finding"]["status"],
            "mitigated"
        );
    }

    #[test]
    fn persist_fuzz_run_evidence_records_findings_and_finishes_the_observation() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let observation = ActiveObservation::start_with_stable_id(
                homeboy_core::observation::NewRunRecord::builder("fuzz")
                    .component_id("component-a")
                    .metadata(serde_json::json!({ "source": "homeboy fuzz run" }))
                    .build(),
                "fuzz-evidence",
            )
            .expect("start");
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(&results_path, "{}").expect("results");
            let campaign = campaign_with_findings();

            let persisted = persist_fuzz_run_evidence(
                &observation,
                FuzzRunEvidence {
                    status: RunStatus::Fail,
                    metadata: serde_json::json!({
                        "status": "failed",
                        "evidence_contract": crate::FuzzEvidenceContract::satisfied(),
                    }),
                    results_path: &results_path,
                    execution_request_path: None,
                    sequence_plan_path: None,
                    artifacts_dir: &home.path().join("absent-artifacts"),
                    campaign: Some(&campaign),
                    envelope: None,
                    postprocess: Vec::new(),
                    payloads: &[],
                },
            )
            .expect("persist");

            assert_eq!(persisted.run.id, "fuzz-evidence");
            assert_eq!(persisted.run.kind, "fuzz");
            assert_eq!(persisted.run.status, "fail");
            assert!(persisted.run.finished_at.is_some());
            assert_eq!(persisted.run.metadata_json["source"], "homeboy fuzz run");
            assert_eq!(persisted.run.metadata_json["status"], "failed");
            let store = observation.store();
            let artifacts = store.list_artifacts("fuzz-evidence").expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].kind, "fuzz_results");
            let findings = store
                .list_findings(homeboy_core::observation::FindingListFilter {
                    run_id: Some("fuzz-evidence".to_string()),
                    tool: Some(FUZZ_FINDING_TOOL.to_string()),
                    ..Default::default()
                })
                .expect("findings");
            assert_eq!(findings.len(), 2);
        });
    }
}
