//! Schema identifiers and skip-reason codes for fuzz contracts.

pub const FUZZ_CORE_CONTRACT_SCHEMA: &str = "homeboy/fuzz-core-contract/v1";
pub const FUZZ_CONTRACT_VERSION: u32 = 1;
pub const FUZZ_SURFACE_SCHEMA: &str = "homeboy/fuzz-surface/v1";
pub const FUZZ_TARGET_SCHEMA: &str = "homeboy/fuzz-target/v1";
pub const FUZZ_WORKLOAD_SCHEMA: &str = "homeboy/fuzz-workload/v1";
pub const FUZZ_CAMPAIGN_SCHEMA: &str = "homeboy/fuzz-campaign/v1";
pub const FUZZ_CASE_SCHEMA: &str = "homeboy/fuzz-case/v1";
pub const FUZZ_CASE_LOG_SCHEMA: &str = "homeboy/fuzz-case-log/v1";
pub const FUZZ_SEED_SCHEMA: &str = "homeboy/fuzz-seed/v1";
pub const FUZZ_COVERAGE_SCHEMA: &str = "homeboy/fuzz-coverage/v1";
pub const FUZZ_FINDING_SCHEMA: &str = "homeboy/fuzz-finding/v1";
pub const FUZZ_ARTIFACT_SCHEMA: &str = "homeboy/fuzz-artifact/v1";
pub const FUZZ_THRESHOLD_SCHEMA: &str = "homeboy/fuzz-threshold/v1";
pub const FUZZ_PROVENANCE_SCHEMA: &str = "homeboy/fuzz-provenance/v1";
pub const FUZZ_REPLAY_SCHEMA: &str = "homeboy/fuzz-replay/v1";
pub const FUZZ_COVERAGE_SUMMARY_SCHEMA: &str = "homeboy/fuzz-coverage-summary/v1";
pub const FUZZ_TARGET_INVENTORY_SCHEMA: &str = "homeboy/fuzz-target-inventory/v1";
pub const FUZZ_EXECUTION_REQUEST_SCHEMA: &str = "homeboy/fuzz-execution-request/v1";
pub const FUZZ_SAMPLING_REQUEST_SCHEMA: &str = "homeboy/fuzz-sampling-request/v1";
pub const FUZZ_RESULT_ENVELOPE_SCHEMA: &str = "homeboy/fuzz-result-envelope/v1";
pub const FUZZ_REQUIRED_ARTIFACT_SCHEMA: &str = "homeboy/fuzz-required-artifact/v1";
pub const FUZZ_GATE_SCHEMA: &str = "homeboy/fuzz-gate/v1";
pub const FUZZ_HOTSPOT_SET_SCHEMA: &str = "homeboy/fuzz-hotspot-set/v1";
pub const FUZZ_OBSERVATION_SET_SCHEMA: &str = "homeboy/fuzz-observation-set/v1";
pub const FUZZ_SKIP_REASON_UNSAFE: &str = "unsafe";
pub const FUZZ_SKIP_REASON_DESTRUCTIVE: &str = "destructive";
pub const FUZZ_SKIP_REASON_AUTH_REQUIRED: &str = "auth_required";
pub const FUZZ_SKIP_REASON_UNAVAILABLE: &str = "unavailable";
pub const FUZZ_SKIP_REASON_LEGACY: &str = "legacy";
pub const FUZZ_SKIP_REASON_UNSUPPORTED: &str = "unsupported";
pub const FUZZ_SKIP_REASON_CONFIG_REQUIRED: &str = "config_required";

pub fn standardized_fuzz_skip_reason_codes() -> Vec<String> {
    [
        FUZZ_SKIP_REASON_UNSAFE,
        FUZZ_SKIP_REASON_DESTRUCTIVE,
        FUZZ_SKIP_REASON_AUTH_REQUIRED,
        FUZZ_SKIP_REASON_UNAVAILABLE,
        FUZZ_SKIP_REASON_LEGACY,
        FUZZ_SKIP_REASON_UNSUPPORTED,
        FUZZ_SKIP_REASON_CONFIG_REQUIRED,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
