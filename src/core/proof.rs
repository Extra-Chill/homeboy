//! Generic run-level proof envelope for Homeboy workflows.
//!
//! Plans declare intended work and required gates. Proof records the observed
//! evidence from a run: provenance, gate outcomes, artifacts, environment, and
//! explicit coverage gaps.

use serde::{Deserialize, Serialize};

use crate::core::gate::HomeboyGateResult;

mod validation;
pub use validation::{
    gate_scope_label, gate_status_label, is_ci_equivalent_gate, proof_runner_label,
    validate_proof_value,
};
use validation::{proof_schema, proof_scope, proof_validation_schema};

pub const HOMEBOY_PROOF_SCHEMA: &str = "homeboy/proof/v1";
pub const HOMEBOY_PROOF_VALIDATION_SCHEMA: &str = "homeboy/proof-validation/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeboyProof {
    #[serde(default = "proof_schema")]
    pub schema: String,
    pub id: String,
    pub scope: HomeboyProofScope,
    pub provenance: HomeboyProofProvenance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<HomeboyGateResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<HomeboyProofArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<HomeboyProofEnvironmentVariable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<HomeboyProofGap>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofScope {
    Targeted,
    CiEquivalent,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofProvenance {
    pub runner: HomeboyProofRunner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofRunner {
    Homeboy,
    Manual,
    ExternalCi,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofArtifactRef {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofEnvironmentVariable {
    pub name: String,
    pub value: String,
    pub disposition: HomeboyProofEnvironmentDisposition,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofEnvironmentDisposition {
    Inherited,
    Sanitized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofGap {
    pub kind: HomeboyProofGapKind,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofValidationReport {
    #[serde(default = "proof_validation_schema")]
    pub schema: String,
    pub valid: bool,
    pub diagnostics: Vec<HomeboyProofValidationDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofValidationDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofGapKind {
    CiEquivalentNotRecorded,
    ArtifactUnavailable,
    RunnerUnavailable,
    ManualOnly,
    Other,
}

impl HomeboyProof {
    pub fn new(id: impl Into<String>, provenance: HomeboyProofProvenance) -> Self {
        Self {
            schema: HOMEBOY_PROOF_SCHEMA.to_string(),
            id: id.into(),
            scope: HomeboyProofScope::Unknown,
            provenance,
            gates: Vec::new(),
            artifacts: Vec::new(),
            environment: Vec::new(),
            gaps: Vec::new(),
        }
    }

    pub fn gates(mut self, gates: impl IntoIterator<Item = HomeboyGateResult>) -> Self {
        self.gates = gates.into_iter().collect();
        self.scope = proof_scope(&self.gates);
        self
    }

    pub fn gates_requiring_ci_equivalent(
        self,
        gates: impl IntoIterator<Item = HomeboyGateResult>,
    ) -> Self {
        self.gates(gates).with_ci_equivalent_gap_if_missing()
    }

    pub fn artifacts(
        mut self,
        artifacts: impl IntoIterator<Item = HomeboyProofArtifactRef>,
    ) -> Self {
        self.artifacts = artifacts.into_iter().collect();
        self
    }

    pub fn environment(
        mut self,
        environment: impl IntoIterator<Item = HomeboyProofEnvironmentVariable>,
    ) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }

    pub fn gaps(mut self, gaps: impl IntoIterator<Item = HomeboyProofGap>) -> Self {
        self.gaps = gaps.into_iter().collect();
        self
    }

    pub fn with_ci_equivalent_gap_if_missing(mut self) -> Self {
        if !self.has_ci_equivalent_gate() {
            self.gaps.push(HomeboyProofGap {
                kind: HomeboyProofGapKind::CiEquivalentNotRecorded,
                summary: "CI-equivalent required gate was not recorded; targeted proof must not be treated as full CI coverage".to_string(),
            });
        }
        self
    }

    pub fn has_ci_equivalent_gate(&self) -> bool {
        self.gates.iter().any(is_ci_equivalent_gate)
    }
}

impl HomeboyProofProvenance {
    pub fn homeboy_run(run_id: impl Into<String>) -> Self {
        Self {
            runner: HomeboyProofRunner::Homeboy,
            run_id: Some(run_id.into()),
            task_id: None,
            source_refs: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn source_refs(mut self, source_refs: impl IntoIterator<Item = String>) -> Self {
        self.source_refs = source_refs.into_iter().collect();
        self
    }

    pub fn notes(mut self, notes: impl IntoIterator<Item = String>) -> Self {
        self.notes = notes.into_iter().collect();
        self
    }
}

impl HomeboyProofArtifactRef {
    pub fn uri(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            kind: None,
            label: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gate::{HomeboyGateKind, HomeboyGateStatus};
    use serde_json::json;

    #[test]
    fn proof_scope_distinguishes_targeted_and_ci_equivalent_gates() {
        let targeted = HomeboyGateResult::new(
            "gate-1",
            "focused check",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        );
        let ci_equivalent = HomeboyGateResult::new(
            "gate-2",
            "required project gate",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        )
        .evidence(serde_json::json!({ "ci_equivalent": true }));

        let proof = HomeboyProof::new("proof-1", HomeboyProofProvenance::homeboy_run("run-1"))
            .gates([targeted, ci_equivalent]);

        assert_eq!(proof.scope, HomeboyProofScope::Mixed);
        assert!(proof.has_ci_equivalent_gate());
        assert_eq!(gate_scope_label(&proof.gates[0]), "targeted");
        assert_eq!(gate_scope_label(&proof.gates[1]), "CI-equivalent");
        assert_eq!(
            proof_runner_label(proof.provenance.runner),
            "Homeboy agent-task cook loop"
        );
    }

    #[test]
    fn proof_records_ci_equivalent_gap_when_missing() {
        let proof = HomeboyProof::new("proof-1", HomeboyProofProvenance::homeboy_run("run-1"))
            .gates_requiring_ci_equivalent([HomeboyGateResult::new(
                "gate-1",
                "focused check",
                HomeboyGateKind::Command,
                HomeboyGateStatus::Passed,
            )]);

        assert_eq!(proof.scope, HomeboyProofScope::Targeted);
        assert_eq!(proof.gaps.len(), 1);
        assert_eq!(
            proof.gaps[0].kind,
            HomeboyProofGapKind::CiEquivalentNotRecorded
        );
    }

    #[test]
    fn proof_validation_accepts_non_local_evidence() {
        let report = validate_proof_value(json!({
            "schema": HOMEBOY_PROOF_SCHEMA,
            "id": "proof-1",
            "scope": "targeted",
            "provenance": {
                "runner": "homeboy",
                "run_id": "run-1",
                "source_refs": ["https://github.com/Extra-Chill/homeboy/actions/runs/1"]
            },
            "gates": [{
                "schema": "homeboy/gate-result/v1",
                "id": "quality-check",
                "name": "quality check",
                "kind": "command",
                "status": "passed"
            }],
            "artifacts": [{
                "uri": "runner-artifact://lab/run-1/proof.json",
                "kind": "proof"
            }]
        }));

        assert!(report.valid, "{report:?}");
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn proof_validation_rejects_local_evidence_and_incomplete_gates() {
        let report = validate_proof_value(json!({
            "schema": HOMEBOY_PROOF_SCHEMA,
            "id": "proof-1",
            "scope": "targeted",
            "provenance": {
                "runner": "manual",
                "source_refs": ["http://localhost:8888/evidence"]
            },
            "gates": [{
                "schema": "homeboy/gate-result/v1",
                "id": "quality-check",
                "name": "quality check",
                "kind": "command",
                "status": "failed"
            }],
            "artifacts": [{ "uri": "file:///tmp/proof.json" }]
        }));

        assert!(!report.valid);
        let codes: Vec<&str> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"local_evidence_ref"));
        assert!(codes.contains(&"gate_not_complete"));
    }

    #[test]
    fn proof_validation_accepts_successful_command_envelope_data() {
        let report = validate_proof_value(json!({
            "success": true,
            "data": valid_materialized_spec()
        }));

        assert!(report.valid, "{report:?}");
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn proof_validation_accepts_successful_command_envelope_value() {
        let report = validate_proof_value(json!({
            "success": true,
            "value": valid_materialized_spec()
        }));

        assert!(report.valid, "{report:?}");
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn proof_validation_rejects_failed_command_envelope() {
        let report = validate_proof_value(json!({
            "success": false,
            "error": "materialization failed"
        }));

        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].code, "command_envelope_failed");
        assert!(report.diagnostics[0]
            .message
            .contains("materialization failed"));
    }

    #[test]
    fn proof_validation_rejects_malformed_command_envelope() {
        let report = validate_proof_value(json!({
            "success": "true",
            "data": valid_materialized_spec()
        }));

        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].code, "malformed_command_envelope");
    }

    #[test]
    fn proof_validation_accepts_materialized_controller_gates_and_metrics() {
        let report = validate_proof_value(json!({
            "schema": "homeboy/agent-task-loop-spec-materialization/v1",
            "spec": {
                "loop_id": "example/join",
                "config_version": "v1",
                "artifacts": [{ "artifact_id": "page_blocks", "kind": "json" }],
                "workflows": [
                    {
                        "workflow_id": "build_page",
                        "prompt": "build page",
                        "entity_ids": ["home", "about"],
                        "emits": ["page_blocks"]
                    },
                    {
                        "workflow_id": "publish_site",
                        "prompt": "publish site",
                        "consumes": ["page_blocks"],
                        "gates": ["review"],
                        "metrics": ["fallback_blocks"]
                    }
                ]
            }
        }));

        assert!(report.valid, "{report:?}");
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn proof_validation_requires_completed_controller_terminal_outcome() {
        let report = validate_proof_value(json!({
            "schema": "homeboy/agent-task-loop-controller/v1",
            "loop_id": "loop-1",
            "phase": "done",
            "state": "completed",
            "config_version": "v1",
            "created_at": "2026-06-18T00:00:00Z",
            "updated_at": "2026-06-18T00:00:00Z",
            "next_actions": [{
                "action_id": "action-1",
                "action": { "action": "complete" },
                "status": "pending",
                "reason": "queued",
                "created_at": "2026-06-18T00:00:00Z"
            }]
        }));

        assert!(!report.valid);
        let codes: Vec<&str> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"completion_outcome_missing"));
        assert!(codes.contains(&"completion_has_pending_actions"));
    }

    fn valid_materialized_spec() -> Value {
        json!({
            "schema": "homeboy/agent-task-loop-spec-materialization/v1",
            "spec": {
                "loop_id": "example/simple",
                "config_version": "v1",
                "workflows": [{
                    "workflow_id": "brief",
                    "prompt": "Draft a concise update."
                }]
            }
        })
    }
}
