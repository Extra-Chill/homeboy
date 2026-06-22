use serde_json::Value;

use crate::core::agent_task_controller_service::{validate_loop_spec, AgentTaskRepoLoopSpec};
use crate::core::agent_task_repo_loop_compile::validate_repo_loop_artifact_references;
use crate::core::artifact_address::validated_public_url;
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

use super::*;

const REQUIRED_PROOF_ARTIFACT_MISSING: &str = "required_proof_artifact_missing";

pub fn validate_proof_value(value: Value) -> HomeboyProofValidationReport {
    let mut diagnostics = Vec::new();
    let value = match unwrap_command_envelope(value, &mut diagnostics) {
        Some(value) => value,
        None => {
            return HomeboyProofValidationReport {
                schema: HOMEBOY_PROOF_VALIDATION_SCHEMA.to_string(),
                status: validation_status(&diagnostics),
                valid: false,
                diagnostics,
            };
        }
    };

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

    validation_report(diagnostics)
}

fn unwrap_command_envelope(
    value: Value,
    diagnostics: &mut Vec<HomeboyProofValidationDiagnostic>,
) -> Option<Value> {
    let Some(success) = value.get("success") else {
        return Some(value);
    };
    let Some(success) = success.as_bool() else {
        diagnostics.push(diagnostic(
            "malformed_command_envelope",
            "command envelope success field must be a boolean",
            path("/success"),
        ));
        return None;
    };
    if !success {
        diagnostics.push(diagnostic(
            "command_envelope_failed",
            command_envelope_failure_message(&value),
            path("/success"),
        ));
        return None;
    }
    if let Some(data) = value.get("data") {
        return Some(data.clone());
    }
    if let Some(value) = value.get("value") {
        return Some(value.clone());
    }
    diagnostics.push(diagnostic(
        "command_envelope_payload_missing",
        "successful command envelope must include data or value for proof validation",
        None,
    ));
    None
}

fn command_envelope_failure_message(value: &Value) -> String {
    let detail = value
        .get("error")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("command reported success=false");
    format!("cannot validate failed command envelope: {detail}")
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
    validate_required_proof_artifacts(proof, diagnostics);
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

fn validate_required_proof_artifacts(
    proof: &HomeboyProof,
    diagnostics: &mut Vec<HomeboyProofValidationDiagnostic>,
) {
    for (index, requirement) in proof.artifact_requirements.iter().enumerate() {
        if !requirement.required {
            continue;
        }
        if proof
            .artifacts
            .iter()
            .any(|artifact| artifact_satisfies_requirement(artifact, requirement))
        {
            continue;
        }
        diagnostics.push(diagnostic(
            REQUIRED_PROOF_ARTIFACT_MISSING,
            format!(
                "required {:?} artifact '{}' was declared but not recorded in proof artifacts",
                requirement.purpose, requirement.id
            ),
            Some(format!("/artifact_requirements/{index}")),
        ));
    }
}

fn artifact_satisfies_requirement(
    artifact: &HomeboyProofArtifactRef,
    requirement: &HomeboyProofArtifactRequirement,
) -> bool {
    artifact.purpose == requirement.purpose
        && artifact.id.as_ref().is_some_and(|id| id == &requirement.id)
        && requirement
            .kind
            .as_ref()
            .map(|kind| artifact.kind.as_ref() == Some(kind))
            .unwrap_or(true)
        && requirement
            .label
            .as_ref()
            .map(|label| artifact.label.as_ref() == Some(label))
            .unwrap_or(true)
        && requirement
            .semantic_key
            .as_ref()
            .map(|semantic_key| artifact.semantic_key.as_ref() == Some(semantic_key))
            .unwrap_or(true)
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
    let Ok(spec) = serde_json::from_value::<AgentTaskRepoLoopSpec>(spec.clone()) else {
        diagnostics.push(diagnostic(
            "invalid_loop_spec_json",
            "materialized controller spec does not match the loop spec schema",
            path("/spec"),
        ));
        return;
    };
    if let Err(error) = validate_loop_spec(&spec) {
        diagnostics.push(diagnostic(
            "invalid_controller_loop_spec",
            error.message,
            path("/spec"),
        ));
    }
    if let Err(error) = validate_repo_loop_artifact_references(&spec) {
        diagnostics.push(diagnostic(
            "invalid_artifact_references",
            error.message,
            path("/spec"),
        ));
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

fn validation_report(
    diagnostics: Vec<HomeboyProofValidationDiagnostic>,
) -> HomeboyProofValidationReport {
    let status = validation_status(&diagnostics);
    HomeboyProofValidationReport {
        schema: HOMEBOY_PROOF_VALIDATION_SCHEMA.to_string(),
        status,
        valid: status == HomeboyProofValidationStatus::Passed,
        diagnostics,
    }
}

fn validation_status(
    diagnostics: &[HomeboyProofValidationDiagnostic],
) -> HomeboyProofValidationStatus {
    if diagnostics.is_empty() {
        return HomeboyProofValidationStatus::Passed;
    }
    if diagnostics
        .iter()
        .all(|diagnostic| diagnostic.code == REQUIRED_PROOF_ARTIFACT_MISSING)
    {
        return HomeboyProofValidationStatus::Incomplete;
    }
    HomeboyProofValidationStatus::Failed
}

pub(super) fn proof_validation_schema() -> String {
    HOMEBOY_PROOF_VALIDATION_SCHEMA.to_string()
}

pub(super) fn proof_scope(gates: &[HomeboyGateResult]) -> HomeboyProofScope {
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

pub(super) fn proof_schema() -> String {
    HOMEBOY_PROOF_SCHEMA.to_string()
}
