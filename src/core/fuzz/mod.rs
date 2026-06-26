//! Product-neutral fuzzing contracts shared by runners, labs, and reports.
//!
//! These types define Homeboy-owned envelope shapes only. Product-specific
//! runners can attach their own details through `metadata` or flattened extras.

mod artifact_envelope;
mod contract;
mod coverage;
mod defaults;
mod envelope;
mod hotspots;
mod normalize;
mod observations;
mod parse;
mod schema_defaults;
mod schemas;
mod types;

#[cfg(test)]
mod tests;

pub use artifact_envelope::{
    inspect_fuzz_result_envelope_artifact, FuzzResultEnvelopeArtifactInspection,
    FuzzResultEnvelopeArtifactSummary,
};
pub use contract::{
    canonical_operation_family, fuzz_core_contract, FuzzContractSchemas, FuzzCoreContract,
    FuzzFindingStatus, FuzzOperationFamily, FuzzSafetyClass,
};
pub use coverage::{
    FuzzArtifact, FuzzCoverage, FuzzCoverageGap, FuzzCoverageGroupSummary, FuzzCoverageSkip,
    FuzzCoverageSummary, FuzzFinding, FuzzProvenance, FuzzReplayMetadata, FuzzThreshold,
    FuzzThresholdOperator,
};
pub use defaults::{
    default_fuzz_gates, default_fuzz_required_artifacts, fuzz_gate_profile_contract,
    FuzzGateProfile,
};
pub use envelope::{
    FuzzExecutionRequest, FuzzGate, FuzzRequiredArtifact, FuzzResultEnvelope, FuzzTargetInventory,
};
pub use hotspots::{parse_fuzz_hotspot_set_value, FuzzHotspot, FuzzHotspotSet};
pub use observations::{
    parse_fuzz_observation_set_value, FuzzObservation, FuzzObservationFamily, FuzzObservationSet,
};
pub use parse::{
    merge_fuzz_target_inventory, parse_fuzz_case_log_contents, parse_fuzz_case_log_file,
    parse_fuzz_result_envelope_file, parse_fuzz_results_file, parse_fuzz_target_inventory_file,
};
pub use schemas::{
    standardized_fuzz_skip_reason_codes, FUZZ_ARTIFACT_SCHEMA, FUZZ_CAMPAIGN_SCHEMA,
    FUZZ_CASE_LOG_SCHEMA, FUZZ_CASE_SCHEMA, FUZZ_CONTRACT_VERSION, FUZZ_CORE_CONTRACT_SCHEMA,
    FUZZ_COVERAGE_SCHEMA, FUZZ_COVERAGE_SUMMARY_SCHEMA, FUZZ_EXECUTION_REQUEST_SCHEMA,
    FUZZ_FINDING_SCHEMA, FUZZ_GATE_SCHEMA, FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA,
    FUZZ_PROVENANCE_SCHEMA, FUZZ_REPLAY_SCHEMA, FUZZ_REQUIRED_ARTIFACT_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA, FUZZ_SEED_SCHEMA, FUZZ_SKIP_REASON_AUTH_REQUIRED,
    FUZZ_SKIP_REASON_CONFIG_REQUIRED, FUZZ_SKIP_REASON_DESTRUCTIVE, FUZZ_SKIP_REASON_LEGACY,
    FUZZ_SKIP_REASON_UNAVAILABLE, FUZZ_SKIP_REASON_UNSAFE, FUZZ_SKIP_REASON_UNSUPPORTED,
    FUZZ_SURFACE_SCHEMA, FUZZ_TARGET_INVENTORY_SCHEMA, FUZZ_TARGET_SCHEMA, FUZZ_THRESHOLD_SCHEMA,
    FUZZ_WORKLOAD_SCHEMA,
};
pub use types::{
    FuzzCampaign, FuzzCase, FuzzCaseLogArtifactRef, FuzzCaseLogEntry, FuzzCaseLogStatus, FuzzInput,
    FuzzOperation, FuzzSeed, FuzzSurface, FuzzTarget, FuzzWorkload,
};
