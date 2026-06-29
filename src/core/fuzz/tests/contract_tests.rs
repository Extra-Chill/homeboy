use serde_json::json;

use crate::core::fuzz::*;

#[test]
fn core_contract_lists_product_neutral_schema_ids() {
    let contract = fuzz_core_contract();

    assert_eq!(contract.schema, FUZZ_CORE_CONTRACT_SCHEMA);
    assert_eq!(contract.version, FUZZ_CONTRACT_VERSION);
    assert_eq!(contract.schemas.surface, FUZZ_SURFACE_SCHEMA);
    assert_eq!(contract.schemas.target, FUZZ_TARGET_SCHEMA);
    assert_eq!(contract.schemas.campaign, FUZZ_CAMPAIGN_SCHEMA);
    assert_eq!(contract.schemas.case, FUZZ_CASE_SCHEMA);
    assert_eq!(contract.schemas.case_log, FUZZ_CASE_LOG_SCHEMA);
    assert_eq!(contract.schemas.sequence_plan, FUZZ_SEQUENCE_PLAN_SCHEMA);
    assert_eq!(
        contract.schemas.sequence_result,
        FUZZ_SEQUENCE_RESULT_SCHEMA
    );
    assert_eq!(contract.schemas.replay, FUZZ_REPLAY_SCHEMA);
    assert_eq!(
        contract.schemas.coverage_summary,
        FUZZ_COVERAGE_SUMMARY_SCHEMA
    );
    assert_eq!(
        contract.schemas.coverage_reconciliation,
        FUZZ_COVERAGE_RECONCILIATION_SCHEMA
    );
    assert_eq!(
        contract.schemas.target_inventory,
        FUZZ_TARGET_INVENTORY_SCHEMA
    );
    assert_eq!(
        contract.schemas.execution_request,
        FUZZ_EXECUTION_REQUEST_SCHEMA
    );
    assert_eq!(
        contract.schemas.sampling_request,
        FUZZ_SAMPLING_REQUEST_SCHEMA
    );
    assert_eq!(
        contract.schemas.result_envelope,
        FUZZ_RESULT_ENVELOPE_SCHEMA
    );
    assert_eq!(
        contract.schemas.required_artifact,
        FUZZ_REQUIRED_ARTIFACT_SCHEMA
    );
    assert_eq!(contract.schemas.gate, FUZZ_GATE_SCHEMA);
    assert_eq!(
        contract.schemas.observation_set,
        FUZZ_OBSERVATION_SET_SCHEMA
    );
    assert_eq!(
        contract.schemas.lifecycle_contract,
        crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA
    );
    assert_eq!(contract.schemas.isolation_proof, ISOLATION_PROOF_SCHEMA);
    assert!(contract
        .safety_classes
        .contains(&FuzzSafetyClass::IsolatedMutation));
    assert!(contract
        .operation_families
        .contains(&FuzzOperationFamily::Read));
    assert!(contract
        .operation_families
        .contains(&FuzzOperationFamily::PerformanceProbe));
    assert!(contract.finding_statuses.contains(&FuzzFindingStatus::Open));
    assert!(contract
        .skip_reason_codes
        .contains(&FUZZ_SKIP_REASON_AUTH_REQUIRED.to_string()));
}

#[test]
fn fuzz_gate_profiles_are_measurement_first_and_named() {
    let (measurement_artifacts, measurement_gates) =
        fuzz_gate_profile_contract(FuzzGateProfile::Measurement);
    assert!(measurement_artifacts.is_empty());
    assert!(measurement_gates.is_empty());

    let (evidence_artifacts, evidence_gates) =
        fuzz_gate_profile_contract(FuzzGateProfile::Evidence);
    assert!(evidence_artifacts
        .iter()
        .any(|artifact| artifact.id == "case-log"));
    assert!(!evidence_artifacts
        .iter()
        .any(|artifact| artifact.id == "coverage-summary"));
    assert!(evidence_gates
        .iter()
        .any(|gate| gate.id == "has-case-evidence"));
    assert!(!evidence_gates
        .iter()
        .any(|gate| gate.id == "target-coverage-complete"));

    let (coverage_artifacts, coverage_gates) =
        fuzz_gate_profile_contract(FuzzGateProfile::CoverageComplete);
    assert_eq!(coverage_artifacts.len(), 1);
    assert_eq!(coverage_artifacts[0].id, "coverage-summary");
    assert!(coverage_gates
        .iter()
        .all(|gate| gate.kind == "coverage_completeness"));

    let (strict_artifacts, strict_gates) = fuzz_gate_profile_contract(FuzzGateProfile::Strict);
    assert_eq!(
        strict_artifacts.len(),
        default_fuzz_required_artifacts().len()
    );
    assert_eq!(strict_gates.len(), default_fuzz_gates().len());
}

#[test]
fn core_contract_deserializes_without_operation_families() {
    let contract: FuzzCoreContract = serde_json::from_value(json!({
        "schema": FUZZ_CORE_CONTRACT_SCHEMA,
        "version": FUZZ_CONTRACT_VERSION,
        "schemas": {
            "surface": FUZZ_SURFACE_SCHEMA,
            "target": FUZZ_TARGET_SCHEMA,
            "workload": FUZZ_WORKLOAD_SCHEMA,
            "campaign": FUZZ_CAMPAIGN_SCHEMA,
            "case": FUZZ_CASE_SCHEMA,
            "case_log": FUZZ_CASE_LOG_SCHEMA,
            "seed": FUZZ_SEED_SCHEMA,
            "coverage": FUZZ_COVERAGE_SCHEMA,
            "finding": FUZZ_FINDING_SCHEMA,
            "artifact": FUZZ_ARTIFACT_SCHEMA,
            "threshold": FUZZ_THRESHOLD_SCHEMA,
            "provenance": FUZZ_PROVENANCE_SCHEMA,
            "replay": FUZZ_REPLAY_SCHEMA,
            "coverage_summary": FUZZ_COVERAGE_SUMMARY_SCHEMA,
            "target_inventory": FUZZ_TARGET_INVENTORY_SCHEMA,
            "execution_request": FUZZ_EXECUTION_REQUEST_SCHEMA,
            "result_envelope": FUZZ_RESULT_ENVELOPE_SCHEMA,
            "required_artifact": FUZZ_REQUIRED_ARTIFACT_SCHEMA,
            "gate": FUZZ_GATE_SCHEMA
        },
        "safety_classes": ["read_only"],
        "finding_statuses": ["open"]
    }))
    .expect("old contract payload");

    assert!(contract
        .operation_families
        .contains(&FuzzOperationFamily::Read));
    assert!(contract
        .operation_families
        .contains(&FuzzOperationFamily::Render));
    assert_eq!(contract.schemas.sequence_plan, FUZZ_SEQUENCE_PLAN_SCHEMA);
    assert_eq!(
        contract.schemas.sequence_result,
        FUZZ_SEQUENCE_RESULT_SCHEMA
    );
    assert_eq!(
        contract.schemas.sampling_request,
        FUZZ_SAMPLING_REQUEST_SCHEMA
    );
    assert_eq!(
        contract.schemas.coverage_reconciliation,
        FUZZ_COVERAGE_RECONCILIATION_SCHEMA
    );
    assert_eq!(contract.schemas.isolation_proof, ISOLATION_PROOF_SCHEMA);
}

#[test]
fn isolation_proof_requires_explicit_destructive_safety_fields() {
    let proof = IsolationProof::from_value(json!({
        "schema": ISOLATION_PROOF_SCHEMA,
        "version": FUZZ_CONTRACT_VERSION,
        "runtime_kind": "ephemeral-runner",
        "provider_ref": { "opaque": "provider-owned" },
        "disposable": true,
        "snapshot_ref": "snapshot://baseline-1",
        "reset_supported": true,
        "teardown_required": true,
        "mutation_boundary": "runner-workspace",
        "proof_artifacts": [{ "kind": "log", "ref": "artifact://proof" }],
        "verified_by": "test-suite"
    }))
    .expect("valid proof");

    assert_eq!(proof.schema, ISOLATION_PROOF_SCHEMA);
    assert_eq!(proof.runtime_kind, "ephemeral-runner");
    assert!(proof.disposable);
    assert!(proof.reset_supported);
    assert!(proof.teardown_required);
}

#[test]
fn isolation_proof_rejects_missing_required_proof_artifacts() {
    let error = IsolationProof::from_value(json!({
        "schema": ISOLATION_PROOF_SCHEMA,
        "version": FUZZ_CONTRACT_VERSION,
        "runtime_kind": "ephemeral-runner",
        "disposable": true,
        "snapshot_ref": "snapshot://baseline-1",
        "reset_supported": true,
        "teardown_required": true,
        "mutation_boundary": "runner-workspace",
        "verified_by": "test-suite"
    }))
    .expect_err("proof artifacts are required");

    assert!(error.contains("proof_artifacts"));
}

#[test]
fn execution_request_deserializes_without_sampling_contract() {
    let request: FuzzExecutionRequest = serde_json::from_value(json!({
        "schema": FUZZ_EXECUTION_REQUEST_SCHEMA,
        "version": FUZZ_CONTRACT_VERSION,
        "id": "request-1",
        "component": "component-a"
    }))
    .expect("legacy execution request payload");

    assert_eq!(request.sampling.schema, FUZZ_SAMPLING_REQUEST_SCHEMA);
    assert_eq!(request.sampling.strategy, "all");
    assert!(request.sampling.replay.deterministic);
}

#[test]
fn default_required_artifacts_and_gates_are_product_neutral() {
    let artifacts = default_fuzz_required_artifacts();
    let gates = default_fuzz_gates();

    assert!(artifacts.iter().any(|artifact| {
        artifact.schema == FUZZ_REQUIRED_ARTIFACT_SCHEMA && artifact.id == "result-envelope"
    }));
    assert!(artifacts.iter().any(|artifact| artifact.id == "case-log"));
    assert!(artifacts
        .iter()
        .any(|artifact| artifact.id == "coverage-summary"));
    assert!(gates
        .iter()
        .any(|gate| gate.schema == FUZZ_GATE_SCHEMA && gate.id == "no-open-findings"));
    assert!(gates.iter().any(|gate| gate.id == "has-case-evidence"));
    assert!(gates
        .iter()
        .any(|gate| gate.id == "target-coverage-complete"));
    assert!(gates
        .iter()
        .any(|gate| gate.id == "operation-coverage-complete"));
}
