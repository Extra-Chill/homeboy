//! Generic run-level proof envelope for Homeboy workflows.
//!
//! Plans declare intended work and required gates. Proof records the observed
//! evidence from a run: provenance, gate outcomes, artifacts, environment, and
//! explicit coverage gaps.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task_loop_definition::compile_loop_spec_value;
use crate::core::artifact_address::validated_public_url;
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

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

pub fn validate_proof_value(value: Value) -> HomeboyProofValidationReport {
    let mut diagnostics = Vec::new();
    match value.get("schema").and_then(Value::as_str) {
        Some(HOMEBOY_PROOF_SCHEMA) => match serde_json::from_value::<HomeboyProof>(value) {
            Ok(proof) => validate_homeboy_proof(&proof, &mut diagnostics),
            Err(error) => diagnostics.push(diagnostic(
                "invalid_proof_json",
                format!("proof JSON does not match {HOMEBOY_PROOF_SCHEMA}: {error}"),
                None,
            )),
        },
        Some("homeboy/agent-task-loop-spec-materialization/v1") => {
            validate_materialized_loop_spec(&value, &mut diagnostics);
        }
        Some("homeboy/agent-task-loop-controller/v1") => {
            validate_controller_record(&value, &mut diagnostics);
        }
        Some(schema) => diagnostics.push(diagnostic(
            "unsupported_schema",
            format!("validate-proof supports {HOMEBOY_PROOF_SCHEMA}, homeboy/agent-task-loop-spec-materialization/v1, and homeboy/agent-task-loop-controller/v1; got {schema}"),
            path("/schema"),
        )),
        None => diagnostics.push(diagnostic(
            "missing_schema",
            "proof validation input requires a schema field",
            path("/schema"),
        )),
    }

    HomeboyProofValidationReport {
        schema: HOMEBOY_PROOF_VALIDATION_SCHEMA.to_string(),
        valid: diagnostics.is_empty(),
        diagnostics,
    }
}

fn validate_homeboy_proof(
    proof: &HomeboyProof,
    diagnostics: &mut Vec<HomeboyProofValidationDiagnostic>,
) {
    if proof.artifacts.is_empty() {
        diagnostics.push(diagnostic(
            "declared_artifacts_missing",
            "proof must declare at least one artifact reference",
            path("/artifacts"),
        ));
    }
    for (index, artifact) in proof.artifacts.iter().enumerate() {
        validate_evidence_ref(
            &artifact.uri,
            format!("/artifacts/{index}/uri"),
            diagnostics,
        );
    }
    for (index, source_ref) in proof.provenance.source_refs.iter().enumerate() {
        validate_evidence_ref(
            source_ref,
            format!("/provenance/source_refs/{index}"),
            diagnostics,
        );
    }
    for (index, gate) in proof.gates.iter().enumerate() {
        if matches!(
            gate.status,
            HomeboyGateStatus::Failed | HomeboyGateStatus::Blocked
        ) {
            diagnostics.push(diagnostic(
                "gate_not_complete",
                format!(
                    "gate '{}' has status {}; deterministic completion requires passed or explicitly skipped gates",
                    gate.id,
                    gate_status_label(gate.status)
                ),
                Some(format!("/gates/{index}/status")),
            ));
        }
    }
    for (index, gap) in proof.gaps.iter().enumerate() {
        diagnostics.push(diagnostic(
            "proof_gap_declared",
            format!(
                "proof declares unresolved gap {:?}: {}",
                gap.kind, gap.summary
            ),
            Some(format!("/gaps/{index}")),
        ));
    }
}

fn validate_materialized_loop_spec(
    value: &Value,
    diagnostics: &mut Vec<HomeboyProofValidationDiagnostic>,
) {
    let Some(spec) = value.get("spec") else {
        diagnostics.push(diagnostic(
            "materialized_spec_missing",
            "materialized controller output must include spec",
            path("/spec"),
        ));
        return;
    };
    if let Err(error) = compile_loop_spec_value(spec.clone()) {
        let tried = error
            .details
            .get("tried")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if tried.is_empty() {
            diagnostics.push(diagnostic(
                "unsupported_controller_semantics",
                error.message,
                path("/spec"),
            ));
        } else {
            for message in tried
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_string))
            {
                diagnostics.push(diagnostic(
                    diagnostic_code_for_compile_message(&message),
                    message,
                    path("/spec"),
                ));
            }
        }
    }
}

fn validate_controller_record(
    value: &Value,
    diagnostics: &mut Vec<HomeboyProofValidationDiagnostic>,
) {
    if value.get("state").and_then(Value::as_str) == Some("completed") {
        if value
            .get("terminal_outcomes")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            diagnostics.push(diagnostic(
                "completion_outcome_missing",
                "completed controller records must include a terminal outcome explaining deterministic completion",
                path("/terminal_outcomes"),
            ));
        }
        if value
            .get("next_actions")
            .and_then(Value::as_array)
            .is_some_and(|actions| !actions.is_empty())
        {
            diagnostics.push(diagnostic(
                "completion_has_pending_actions",
                "completed controller records must not retain executable next_actions",
                path("/next_actions"),
            ));
        }
    }
}

fn validate_evidence_ref(
    reference: &str,
    path: String,
    diagnostics: &mut Vec<HomeboyProofValidationDiagnostic>,
) {
    if is_non_local_evidence_ref(reference) {
        return;
    }
    diagnostics.push(diagnostic(
        "local_evidence_ref",
        format!("evidence reference is not reviewer-visible/non-local: {reference}"),
        Some(path),
    ));
}

fn is_non_local_evidence_ref(reference: &str) -> bool {
    let reference = reference.trim();
    if reference.is_empty() {
        return false;
    }
    if reference.starts_with("runner-artifact://") || reference.starts_with("gh://") {
        return true;
    }
    validated_public_url(reference).is_some()
}

fn diagnostic_code_for_compile_message(message: &str) -> &'static str {
    if message.contains("join over fan-out") {
        "unsupported_join"
    } else if message.contains("gates") || message.contains("metrics") {
        "unsupported_gate"
    } else {
        "unsupported_controller_semantics"
    }
}

fn diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    path: Option<String>,
) -> HomeboyProofValidationDiagnostic {
    HomeboyProofValidationDiagnostic {
        code: code.into(),
        message: message.into(),
        path,
    }
}

fn path(value: &str) -> Option<String> {
    Some(value.to_string())
}

fn proof_validation_schema() -> String {
    HOMEBOY_PROOF_VALIDATION_SCHEMA.to_string()
}

fn proof_scope(gates: &[HomeboyGateResult]) -> HomeboyProofScope {
    if gates.is_empty() {
        return HomeboyProofScope::Unknown;
    }

    let ci_equivalent = gates
        .iter()
        .filter(|gate| is_ci_equivalent_gate(gate))
        .count();
    if ci_equivalent == 0 {
        HomeboyProofScope::Targeted
    } else if ci_equivalent == gates.len() {
        HomeboyProofScope::CiEquivalent
    } else {
        HomeboyProofScope::Mixed
    }
}

pub fn is_ci_equivalent_gate(gate: &HomeboyGateResult) -> bool {
    gate.provenance
        .get("ci_equivalent")
        .and_then(|value| value.as_bool())
        .or_else(|| {
            gate.evidence
                .get("ci_equivalent")
                .and_then(|value| value.as_bool())
        })
        .unwrap_or(false)
}

pub fn gate_status_label(status: HomeboyGateStatus) -> &'static str {
    match status {
        HomeboyGateStatus::Passed => "passed",
        HomeboyGateStatus::Failed => "failed",
        HomeboyGateStatus::Skipped => "skipped",
        HomeboyGateStatus::Blocked => "blocked",
    }
}

pub fn gate_scope_label(gate: &HomeboyGateResult) -> &'static str {
    if is_ci_equivalent_gate(gate) {
        "CI-equivalent"
    } else {
        "targeted"
    }
}

pub fn proof_runner_label(runner: HomeboyProofRunner) -> &'static str {
    match runner {
        HomeboyProofRunner::Homeboy => "Homeboy agent-task cook loop",
        HomeboyProofRunner::Manual => "manual",
        HomeboyProofRunner::ExternalCi => "external CI",
        HomeboyProofRunner::Unknown => "unknown",
    }
}

fn proof_schema() -> String {
    HOMEBOY_PROOF_SCHEMA.to_string()
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
                "id": "cargo-test",
                "name": "cargo test",
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
                "id": "cargo-test",
                "name": "cargo test",
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
    fn proof_validation_surfaces_materialized_join_and_gate_diagnostics() {
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
                        "gates": ["review"]
                    }
                ]
            }
        }));

        assert!(!report.valid);
        let codes: Vec<&str> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"unsupported_join"));
        assert!(codes.contains(&"unsupported_gate"));
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
}
