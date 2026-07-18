//! Run-level proof envelope: contract types re-exported from
//! `homeboy-gate-contract`, plus the validation behavior that stays in core.
//!
//! The pure proof data types, builders, and classification helpers live in the
//! leaf `homeboy-gate-contract` crate (alongside the mutually-dependent gate and
//! plan types). The proof *validation* (`validate_proof_value`) reaches into
//! core's `artifact_address` / observation layer, and the `loop_spec_validation`
//! provider is a behavior-inversion hook, so both stay here in core.

pub mod loop_spec_validation;
mod validation;

pub use homeboy_gate_contract::proof::{
    gate_scope_label, gate_status_label, is_ci_equivalent_gate, proof_runner_label, proof_scope,
    HomeboyProof, HomeboyProofArtifactPurpose, HomeboyProofArtifactRef,
    HomeboyProofArtifactRequirement, HomeboyProofEnvironmentDisposition,
    HomeboyProofEnvironmentVariable, HomeboyProofGap, HomeboyProofGapKind, HomeboyProofProvenance,
    HomeboyProofRunner, HomeboyProofScope, HomeboyProofValidationDiagnostic,
    HomeboyProofValidationReport, HomeboyProofValidationStatus, HOMEBOY_PROOF_SCHEMA,
    HOMEBOY_PROOF_VALIDATION_SCHEMA,
};

pub use validation::validate_proof_value;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

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
        assert_eq!(report.status, HomeboyProofValidationStatus::Passed);
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn proof_validation_marks_missing_required_proof_artifact_incomplete() {
        let report = validate_proof_value(json!({
            "schema": HOMEBOY_PROOF_SCHEMA,
            "id": "proof-1",
            "scope": "targeted",
            "provenance": { "runner": "homeboy", "run_id": "run-1" },
            "gates": [{
                "schema": "homeboy/gate-result/v1",
                "id": "quality-check",
                "name": "quality check",
                "kind": "command",
                "status": "passed"
            }],
            "artifact_requirements": [{
                "id": "coverage-summary",
                "kind": "coverage",
                "purpose": "proof",
                "required": true
            }],
            "artifacts": [{
                "id": "coverage-debug-log",
                "uri": "runner-artifact://lab/run-1/debug.log",
                "kind": "log",
                "purpose": "diagnostic"
            }]
        }));

        assert!(!report.valid);
        assert_eq!(report.status, HomeboyProofValidationStatus::Incomplete);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(
            report.diagnostics[0].code,
            "required_proof_artifact_missing"
        );
    }

    #[test]
    fn proof_validation_accepts_missing_optional_diagnostic_artifact() {
        let report = validate_proof_value(json!({
            "schema": HOMEBOY_PROOF_SCHEMA,
            "id": "proof-1",
            "scope": "targeted",
            "provenance": { "runner": "homeboy", "run_id": "run-1" },
            "gates": [{
                "schema": "homeboy/gate-result/v1",
                "id": "quality-check",
                "name": "quality check",
                "kind": "command",
                "status": "passed"
            }],
            "artifact_requirements": [{
                "id": "coverage-summary",
                "kind": "coverage",
                "purpose": "proof",
                "required": true
            }, {
                "id": "coverage-debug-log",
                "kind": "log",
                "purpose": "diagnostic",
                "required": false
            }],
            "artifacts": [{
                "id": "coverage-summary",
                "uri": "runner-artifact://lab/run-1/coverage.json",
                "kind": "coverage",
                "purpose": "proof"
            }]
        }));

        assert!(report.valid, "{report:?}");
        assert_eq!(report.status, HomeboyProofValidationStatus::Passed);
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
        assert_eq!(report.status, HomeboyProofValidationStatus::Failed);
        let codes: Vec<&str> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"local_evidence_ref"));
        assert!(codes.contains(&"gate_not_complete"));
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
