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
    assert_eq!(contract.schemas.replay, FUZZ_REPLAY_SCHEMA);
    assert_eq!(
        contract.schemas.coverage_summary,
        FUZZ_COVERAGE_SUMMARY_SCHEMA
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
fn core_contract_publishes_canonical_artifact_kinds() {
    let contract = fuzz_core_contract();

    assert_eq!(contract.artifact_kinds, canonical_fuzz_artifact_kinds());
    for kind in [
        FUZZ_ARTIFACT_KIND_RESULT_ENVELOPE,
        FUZZ_ARTIFACT_KIND_CASE_LOG,
        FUZZ_ARTIFACT_KIND_COVERAGE_SUMMARY,
        FUZZ_ARTIFACT_KIND_REPLAY_DATA,
    ] {
        assert!(
            contract.artifact_kinds.iter().any(|k| k == kind),
            "published contract should list canonical artifact kind {kind}",
        );
    }
}

#[test]
fn published_result_envelope_schema_matches_struct_serialization() {
    let contract = fuzz_core_contract();

    // The schema id and version the extension would consume from the published
    // contract must equal what `FuzzResultEnvelope` actually serializes with, so
    // the extension can validate against the contract instead of re-declaring
    // `homeboy/fuzz-result-envelope/v1` and the version by hand (#6766).
    let envelope: FuzzResultEnvelope = serde_json::from_value(json!({
        "id": "envelope-1",
        "status": "passed",
        "request": {"id": "request-1", "component": "component-a"}
    }))
    .expect("minimal result envelope payload backfills schema + version");

    assert_eq!(envelope.schema, contract.schemas.result_envelope);
    assert_eq!(envelope.schema, FUZZ_RESULT_ENVELOPE_SCHEMA);
    assert_eq!(envelope.version, contract.version);
    assert_eq!(envelope.version, FUZZ_CONTRACT_VERSION);
}

#[test]
fn core_contract_backfills_artifact_kinds_for_legacy_payloads() {
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
    .expect("legacy contract payload without artifact_kinds");

    assert_eq!(contract.artifact_kinds, canonical_fuzz_artifact_kinds());
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
    assert_eq!(
        contract.schemas.sampling_request,
        FUZZ_SAMPLING_REQUEST_SCHEMA
    );
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
