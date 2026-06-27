//! Core fuzz contract envelope: safety classes, operation families, and the
//! product-neutral schema registry surfaced by `fuzz_core_contract`.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use super::schema_defaults::{
    fuzz_contract_version, fuzz_core_contract_schema, lifecycle_contract_schema,
    lifecycle_result_schema, lifecycle_snapshot_ref_schema,
};
use super::schemas::{
    standardized_fuzz_skip_reason_codes, FUZZ_ARTIFACT_SCHEMA, FUZZ_CAMPAIGN_SCHEMA,
    FUZZ_CASE_LOG_SCHEMA, FUZZ_CASE_SCHEMA, FUZZ_CONTRACT_VERSION, FUZZ_CORE_CONTRACT_SCHEMA,
    FUZZ_COVERAGE_SCHEMA, FUZZ_COVERAGE_SUMMARY_SCHEMA, FUZZ_EXECUTION_REQUEST_SCHEMA,
    FUZZ_FINDING_SCHEMA, FUZZ_GATE_SCHEMA, FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA,
    FUZZ_PROVENANCE_SCHEMA, FUZZ_REPLAY_SCHEMA, FUZZ_REQUIRED_ARTIFACT_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA, FUZZ_SAMPLING_REQUEST_SCHEMA, FUZZ_SEED_SCHEMA,
    FUZZ_SURFACE_SCHEMA, FUZZ_TARGET_INVENTORY_SCHEMA, FUZZ_TARGET_SCHEMA, FUZZ_THRESHOLD_SCHEMA,
    FUZZ_WORKLOAD_SCHEMA,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzCoreContract {
    #[serde(default = "fuzz_core_contract_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub schemas: FuzzContractSchemas,
    pub safety_classes: Vec<FuzzSafetyClass>,
    #[serde(default = "default_fuzz_operation_families")]
    pub operation_families: Vec<FuzzOperationFamily>,
    pub finding_statuses: Vec<FuzzFindingStatus>,
    #[serde(default = "standardized_fuzz_skip_reason_codes")]
    pub skip_reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzContractSchemas {
    pub surface: String,
    pub target: String,
    pub workload: String,
    pub campaign: String,
    pub case: String,
    #[serde(default = "super::schema_defaults::fuzz_case_log_schema")]
    pub case_log: String,
    pub seed: String,
    pub coverage: String,
    pub finding: String,
    pub artifact: String,
    pub threshold: String,
    pub provenance: String,
    pub replay: String,
    pub coverage_summary: String,
    pub target_inventory: String,
    pub execution_request: String,
    #[serde(default = "super::schema_defaults::fuzz_sampling_request_schema")]
    pub sampling_request: String,
    pub result_envelope: String,
    pub required_artifact: String,
    pub gate: String,
    #[serde(default = "super::schema_defaults::fuzz_hotspot_set_schema")]
    pub hotspot_set: String,
    #[serde(default = "super::schema_defaults::fuzz_observation_set_schema")]
    pub observation_set: String,
    #[serde(default = "lifecycle_contract_schema")]
    pub lifecycle_contract: String,
    #[serde(default = "lifecycle_result_schema")]
    pub lifecycle_result: String,
    #[serde(default = "lifecycle_snapshot_ref_schema")]
    pub lifecycle_snapshot_ref: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzSafetyClass {
    ReadOnly,
    Idempotent,
    IsolatedMutation,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzOperationFamily {
    Read,
    Create,
    Update,
    Delete,
    List,
    Search,
    Navigate,
    Render,
    Query,
    Load,
    Submit,
    PerformanceProbe,
}

impl Serialize for FuzzOperationFamily {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(fuzz_operation_family_name(*self))
    }
}

impl<'de> Deserialize<'de> for FuzzOperationFamily {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        canonical_operation_family(&value).ok_or_else(|| {
            de::Error::unknown_variant(
                &value,
                &[
                    "read",
                    "create",
                    "update",
                    "delete",
                    "list",
                    "search",
                    "navigate",
                    "render",
                    "query",
                    "load",
                    "submit",
                    "performance_probe",
                ],
            )
        })
    }
}

fn fuzz_operation_family_name(family: FuzzOperationFamily) -> &'static str {
    match family {
        FuzzOperationFamily::Read => "read",
        FuzzOperationFamily::Create => "create",
        FuzzOperationFamily::Update => "update",
        FuzzOperationFamily::Delete => "delete",
        FuzzOperationFamily::List => "list",
        FuzzOperationFamily::Search => "search",
        FuzzOperationFamily::Navigate => "navigate",
        FuzzOperationFamily::Render => "render",
        FuzzOperationFamily::Query => "query",
        FuzzOperationFamily::Load => "load",
        FuzzOperationFamily::Submit => "submit",
        FuzzOperationFamily::PerformanceProbe => "performance_probe",
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzFindingStatus {
    Open,
    Confirmed,
    Mitigated,
    Suppressed,
}

pub fn canonical_operation_family(kind: &str) -> Option<FuzzOperationFamily> {
    let normalized = kind.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match normalized.as_str() {
        "get" | "read" => Some(FuzzOperationFamily::Read),
        "post" | "create" => Some(FuzzOperationFamily::Create),
        "put" | "patch" | "update" => Some(FuzzOperationFamily::Update),
        "delete" => Some(FuzzOperationFamily::Delete),
        "list" => Some(FuzzOperationFamily::List),
        "search" => Some(FuzzOperationFamily::Search),
        "navigate" => Some(FuzzOperationFamily::Navigate),
        "render" => Some(FuzzOperationFamily::Render),
        "query" => Some(FuzzOperationFamily::Query),
        "load" => Some(FuzzOperationFamily::Load),
        "submit" => Some(FuzzOperationFamily::Submit),
        "performance_probe" => Some(FuzzOperationFamily::PerformanceProbe),
        _ => None,
    }
}

pub fn fuzz_core_contract() -> FuzzCoreContract {
    FuzzCoreContract {
        schema: FUZZ_CORE_CONTRACT_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        schemas: FuzzContractSchemas {
            surface: FUZZ_SURFACE_SCHEMA.to_string(),
            target: FUZZ_TARGET_SCHEMA.to_string(),
            workload: FUZZ_WORKLOAD_SCHEMA.to_string(),
            campaign: FUZZ_CAMPAIGN_SCHEMA.to_string(),
            case: FUZZ_CASE_SCHEMA.to_string(),
            case_log: FUZZ_CASE_LOG_SCHEMA.to_string(),
            seed: FUZZ_SEED_SCHEMA.to_string(),
            coverage: FUZZ_COVERAGE_SCHEMA.to_string(),
            finding: FUZZ_FINDING_SCHEMA.to_string(),
            artifact: FUZZ_ARTIFACT_SCHEMA.to_string(),
            threshold: FUZZ_THRESHOLD_SCHEMA.to_string(),
            provenance: FUZZ_PROVENANCE_SCHEMA.to_string(),
            replay: FUZZ_REPLAY_SCHEMA.to_string(),
            coverage_summary: FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
            target_inventory: FUZZ_TARGET_INVENTORY_SCHEMA.to_string(),
            execution_request: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
            sampling_request: FUZZ_SAMPLING_REQUEST_SCHEMA.to_string(),
            result_envelope: FUZZ_RESULT_ENVELOPE_SCHEMA.to_string(),
            required_artifact: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            gate: FUZZ_GATE_SCHEMA.to_string(),
            hotspot_set: FUZZ_HOTSPOT_SET_SCHEMA.to_string(),
            observation_set: FUZZ_OBSERVATION_SET_SCHEMA.to_string(),
            lifecycle_contract: crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA.to_string(),
            lifecycle_result: crate::core::lifecycle::LIFECYCLE_RESULT_SCHEMA.to_string(),
            lifecycle_snapshot_ref: crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA
                .to_string(),
        },
        safety_classes: vec![
            FuzzSafetyClass::ReadOnly,
            FuzzSafetyClass::Idempotent,
            FuzzSafetyClass::IsolatedMutation,
            FuzzSafetyClass::Destructive,
        ],
        operation_families: default_fuzz_operation_families(),
        finding_statuses: vec![
            FuzzFindingStatus::Open,
            FuzzFindingStatus::Confirmed,
            FuzzFindingStatus::Mitigated,
            FuzzFindingStatus::Suppressed,
        ],
        skip_reason_codes: standardized_fuzz_skip_reason_codes(),
    }
}

pub(super) fn default_fuzz_operation_families() -> Vec<FuzzOperationFamily> {
    vec![
        FuzzOperationFamily::Read,
        FuzzOperationFamily::Create,
        FuzzOperationFamily::Update,
        FuzzOperationFamily::Delete,
        FuzzOperationFamily::List,
        FuzzOperationFamily::Search,
        FuzzOperationFamily::Navigate,
        FuzzOperationFamily::Render,
        FuzzOperationFamily::Query,
        FuzzOperationFamily::Load,
        FuzzOperationFamily::Submit,
        FuzzOperationFamily::PerformanceProbe,
    ]
}
