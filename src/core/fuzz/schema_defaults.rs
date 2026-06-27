//! Serde default constructors that resolve fuzz schema identifiers and version.

use super::schemas::{
    FUZZ_ARTIFACT_SCHEMA, FUZZ_CAMPAIGN_SCHEMA, FUZZ_CASE_LOG_SCHEMA, FUZZ_CASE_SCHEMA,
    FUZZ_CONTRACT_VERSION, FUZZ_CORE_CONTRACT_SCHEMA, FUZZ_COVERAGE_SCHEMA,
    FUZZ_COVERAGE_SUMMARY_SCHEMA, FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_FINDING_SCHEMA,
    FUZZ_GATE_SCHEMA, FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA, FUZZ_PROVENANCE_SCHEMA,
    FUZZ_REPLAY_SCHEMA, FUZZ_REQUIRED_ARTIFACT_SCHEMA, FUZZ_RESULT_ENVELOPE_SCHEMA,
    FUZZ_SAMPLING_REQUEST_SCHEMA, FUZZ_SEED_SCHEMA, FUZZ_SURFACE_SCHEMA,
    FUZZ_TARGET_INVENTORY_SCHEMA, FUZZ_TARGET_SCHEMA, FUZZ_THRESHOLD_SCHEMA, FUZZ_WORKLOAD_SCHEMA,
};

pub(super) fn fuzz_core_contract_schema() -> String {
    FUZZ_CORE_CONTRACT_SCHEMA.to_string()
}

pub(super) fn fuzz_contract_version() -> u32 {
    FUZZ_CONTRACT_VERSION
}

pub(super) fn fuzz_surface_schema() -> String {
    FUZZ_SURFACE_SCHEMA.to_string()
}

pub(super) fn fuzz_target_schema() -> String {
    FUZZ_TARGET_SCHEMA.to_string()
}

pub(super) fn fuzz_workload_schema() -> String {
    FUZZ_WORKLOAD_SCHEMA.to_string()
}

pub(super) fn fuzz_campaign_schema() -> String {
    FUZZ_CAMPAIGN_SCHEMA.to_string()
}

pub(super) fn fuzz_case_schema() -> String {
    FUZZ_CASE_SCHEMA.to_string()
}

pub(super) fn fuzz_case_log_schema() -> String {
    FUZZ_CASE_LOG_SCHEMA.to_string()
}

pub(super) fn fuzz_seed_schema() -> String {
    FUZZ_SEED_SCHEMA.to_string()
}

pub(super) fn fuzz_coverage_schema() -> String {
    FUZZ_COVERAGE_SCHEMA.to_string()
}

pub(super) fn fuzz_finding_schema() -> String {
    FUZZ_FINDING_SCHEMA.to_string()
}

pub(super) fn fuzz_artifact_schema() -> String {
    FUZZ_ARTIFACT_SCHEMA.to_string()
}

pub(super) fn fuzz_threshold_schema() -> String {
    FUZZ_THRESHOLD_SCHEMA.to_string()
}

pub(super) fn fuzz_provenance_schema() -> String {
    FUZZ_PROVENANCE_SCHEMA.to_string()
}

pub(super) fn fuzz_replay_schema() -> String {
    FUZZ_REPLAY_SCHEMA.to_string()
}

pub(super) fn fuzz_coverage_summary_schema() -> String {
    FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string()
}

pub(super) fn fuzz_target_inventory_schema() -> String {
    FUZZ_TARGET_INVENTORY_SCHEMA.to_string()
}

pub(super) fn fuzz_execution_request_schema() -> String {
    FUZZ_EXECUTION_REQUEST_SCHEMA.to_string()
}

pub(super) fn fuzz_sampling_request_schema() -> String {
    FUZZ_SAMPLING_REQUEST_SCHEMA.to_string()
}

pub(super) fn fuzz_result_envelope_schema() -> String {
    FUZZ_RESULT_ENVELOPE_SCHEMA.to_string()
}

pub(super) fn fuzz_required_artifact_schema() -> String {
    FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string()
}

pub(super) fn fuzz_gate_schema() -> String {
    FUZZ_GATE_SCHEMA.to_string()
}

pub(super) fn fuzz_hotspot_set_schema() -> String {
    FUZZ_HOTSPOT_SET_SCHEMA.to_string()
}

pub(super) fn fuzz_observation_set_schema() -> String {
    FUZZ_OBSERVATION_SET_SCHEMA.to_string()
}

pub(super) fn lifecycle_contract_schema() -> String {
    crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA.to_string()
}

pub(super) fn lifecycle_result_schema() -> String {
    crate::core::lifecycle::LIFECYCLE_RESULT_SCHEMA.to_string()
}

pub(super) fn lifecycle_snapshot_ref_schema() -> String {
    crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string()
}
