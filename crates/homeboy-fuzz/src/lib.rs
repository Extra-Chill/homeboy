//! Product-neutral fuzzing contracts shared by runners, labs, and reports.
//!
//! These types define Homeboy-owned envelope shapes only. Product-specific
//! runners can attach their own details through `metadata` or flattened extras.

mod artifact_envelope;
mod cohorts;
mod contract;
mod coverage;
mod coverage_reconciliation_persistence;
mod defaults;
mod envelope;
mod evidence_contract;
mod hotspots;
mod normalize;
mod observations;
mod parse;
mod payloads;
mod proof;
mod result_envelope_persistence;
mod run_dir_writers;
mod run_evidence_persistence;
mod schema_defaults;
mod schemas;
mod types;

#[cfg(test)]
mod tests;

pub use artifact_envelope::{
    inspect_fuzz_result_envelope_artifact, FuzzResultEnvelopeArtifactInspection,
    FuzzResultEnvelopeArtifactSummary,
};
pub use cohorts::{
    compare_fuzz_hotspot_cohorts, FuzzHotspotCohortComparison, FuzzHotspotCohortItem,
};

pub use contract::{
    fuzz_core_contract, FuzzCoreContract, FuzzFindingStatus, FuzzOperationFamily, FuzzSafetyClass,
};
pub(crate) use coverage::FuzzCoverageReconciliation;
pub use coverage::{
    FuzzArtifact, FuzzCoverageGroupSummary, FuzzCoverageSkip, FuzzCoverageSummary, FuzzFinding,
    FuzzProvenance, FuzzReplayMetadata, FuzzThresholdOperator,
};
#[allow(unused_imports)] // consumed by `src/tests/*` via `use crate::*`
pub(crate) use coverage::{FuzzCoverage, FuzzCoverageGap, FuzzThreshold};

pub use defaults::{
    default_fuzz_gates, default_fuzz_required_artifacts, fuzz_gate_profile_contract,
    FuzzGateProfile,
};
pub use envelope::{
    FuzzExecutionRequest, FuzzGate, FuzzRequiredArtifact, FuzzResultEnvelope,
    FuzzSamplingCorpusRef, FuzzSamplingReplayDeterminism, FuzzSamplingRequest, FuzzSamplingStratum,
    FuzzTargetInventory, IsolationProof,
};
pub use evidence_contract::{
    classify_fuzz_failure, FuzzEvidenceContract, FuzzEvidenceViolation, FuzzEvidenceViolationCode,
    FuzzFailureDomain, FuzzFailureSignals, FUZZ_ARTIFACT_ROOT_PRODUCER_CONTRACT,
    FUZZ_RESULTS_FILE_PRODUCER_CONTRACT,
};

pub use hotspots::{
    parse_fuzz_hotspot_set_value, rank_fuzz_observation_set_hotspots, FuzzHotspot, FuzzHotspotSet,
};
#[allow(unused_imports)] // consumed by `src/tests/*` via `use crate::*`
pub(crate) use hotspots::{FuzzHotspotDimension, FuzzHotspotMetric};

pub use observations::parse_fuzz_observation_set_value;
pub(crate) use observations::{FuzzObservation, FuzzObservationFamily, FuzzObservationSet};
pub use parse::{
    merge_fuzz_target_inventory, parse_fuzz_action_model_file, parse_fuzz_case_log_file,
    parse_fuzz_exploration_policy_file, parse_fuzz_result_envelope_file, parse_fuzz_results_file,
    parse_fuzz_sequence_plan_file, parse_fuzz_target_inventory_file,
};
pub(crate) use payloads::FUZZ_PAYLOAD_ARTIFACT_KIND;
pub use payloads::{
    externalize_fuzz_campaign_payloads, FuzzPayload, FuzzPayloadBudget,
    INLINE_FUZZ_PAYLOAD_LIMIT_BYTES,
};
pub use proof::{
    derive_fuzz_proof, fuzz_campaign_case_totals, fuzz_campaign_finding_totals, FuzzProof,
};

pub use result_envelope_persistence::{
    fuzz_result_envelope_evidence_ref, report_fuzz_result_envelope,
    FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND,
};

// Crate-internal surface. These stages have no consumer outside homeboy-fuzz —
// they are reached only through the public entry points above (and through
// `persist_fuzz_run_evidence`). Re-exporting them at `pub(crate)` keeps the
// in-crate `use crate::…` call sites working while leaving them subject to
// rustc's dead-code analysis; a `pub` re-export at the crate root would exempt
// them from it.
pub(crate) use coverage::reconcile_fuzz_coverage;
pub(crate) use coverage_reconciliation_persistence::persist_fuzz_coverage_reconciliation;
pub(crate) use result_envelope_persistence::persist_fuzz_run_result_envelope;
pub use run_dir_writers::{persist_fuzz_execution_request, persist_fuzz_sequence_plan};
pub use run_evidence_persistence::{persist_fuzz_run_evidence, FuzzRunEvidence};
pub(crate) use schemas::FUZZ_COVERAGE_RECONCILIATION_SCHEMA;
pub use schemas::{
    FUZZ_ACTION_MODEL_SCHEMA, FUZZ_ARTIFACT_SCHEMA, FUZZ_CAMPAIGN_SCHEMA, FUZZ_CASE_SCHEMA,
    FUZZ_CONTRACT_VERSION, FUZZ_COVERAGE_SUMMARY_SCHEMA, FUZZ_EVIDENCE_CONTRACT_SCHEMA,
    FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_EXPLORATION_POLICY_SCHEMA, FUZZ_FINDING_SCHEMA,
    FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA, FUZZ_PROOF_SCHEMA,
    FUZZ_PROVENANCE_SCHEMA, FUZZ_REPLAY_SCHEMA, FUZZ_REQUIRED_ARTIFACT_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA, FUZZ_SAMPLING_REQUEST_SCHEMA, FUZZ_SEED_SCHEMA,
    FUZZ_SEQUENCE_PLAN_SCHEMA, FUZZ_TARGET_INVENTORY_SCHEMA, FUZZ_WORKLOAD_SCHEMA,
    ISOLATION_PROOF_SCHEMA,
};
#[allow(unused_imports)] // consumed by `src/tests/*` via `use crate::*`
pub(crate) use schemas::{
    FUZZ_CASE_LOG_SCHEMA, FUZZ_CORE_CONTRACT_SCHEMA, FUZZ_COVERAGE_SCHEMA, FUZZ_GATE_SCHEMA,
    FUZZ_SEQUENCE_RESULT_SCHEMA, FUZZ_SKIP_REASON_AUTH_REQUIRED, FUZZ_SURFACE_SCHEMA,
    FUZZ_TARGET_SCHEMA, FUZZ_THRESHOLD_SCHEMA,
};
pub use types::{FuzzCampaign, FuzzCase, FuzzOperation, FuzzSeed, FuzzSequencePlan, FuzzWorkload};
#[allow(unused_imports)] // consumed by `src/tests/*` via `use crate::*`
pub(crate) use types::{FuzzCaseLogStatus, FuzzSurface, FuzzTarget};
pub(crate) use types::{FuzzSequenceCase, FuzzSequenceResult};
