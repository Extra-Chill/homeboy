use serde_json::Value;

use crate::artifact_address::validated_public_url;
use crate::gate::HomeboyGateStatus;

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
            super::loop_spec_validation::validate_loop_spec_materialization_record(
                &value,
                &mut diagnostics,
            );
        }
        Some("homeboy/agent-task-loop-controller/v1") => {
            super::loop_spec_validation::validate_controller_record(&value, &mut diagnostics);
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

pub(super) fn diagnostic(
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

pub(super) fn path(value: &str) -> Option<String> {
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
