//! Product-neutral fuzzing contracts shared by runners, labs, and reports.
//!
//! These types define Homeboy-owned envelope shapes only. Product-specific
//! runners can attach their own details through `metadata` or flattened extras.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_contract::ArtifactContract;
use crate::core::lifecycle::{LifecycleContract, LifecycleResultMetadata};
use crate::core::{Error, Result};

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
pub const FUZZ_RESULT_ENVELOPE_SCHEMA: &str = "homeboy/fuzz-result-envelope/v1";
pub const FUZZ_REQUIRED_ARTIFACT_SCHEMA: &str = "homeboy/fuzz-required-artifact/v1";
pub const FUZZ_GATE_SCHEMA: &str = "homeboy/fuzz-gate/v1";
pub const FUZZ_SKIP_REASON_UNSAFE: &str = "unsafe";
pub const FUZZ_SKIP_REASON_DESTRUCTIVE: &str = "destructive";
pub const FUZZ_SKIP_REASON_AUTH_REQUIRED: &str = "auth_required";
pub const FUZZ_SKIP_REASON_UNAVAILABLE: &str = "unavailable";
pub const FUZZ_SKIP_REASON_LEGACY: &str = "legacy";
pub const FUZZ_SKIP_REASON_UNSUPPORTED: &str = "unsupported";
pub const FUZZ_SKIP_REASON_CONFIG_REQUIRED: &str = "config_required";

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
    #[serde(default = "fuzz_case_log_schema")]
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
    pub result_envelope: String,
    pub required_artifact: String,
    pub gate: String,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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
    BlockRender,
    PerformanceProbe,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzSurface {
    #[serde(default = "fuzz_surface_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub safety_class: FuzzSafetyClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<FuzzOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<FuzzInput>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzSurface {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut surface: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        surface.normalize()?;
        Ok(surface)
    }

    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_SURFACE_SCHEMA);
        require_schema(&self.schema, FUZZ_SURFACE_SCHEMA, "fuzz surface")?;
        self.id = required_trimmed("id", &self.id)?;
        self.kind = required_trimmed("kind", &self.kind)?;
        self.label = normalize_optional_string(self.label.take());
        self.target = normalize_optional_string(self.target.take());
        for operation in &mut self.operations {
            operation.normalize()?;
        }
        for input in &mut self.inputs {
            input.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzOperation {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<FuzzOperationFamily>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl FuzzOperation {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.id = required_trimmed("operation.id", &self.id)?;
        self.kind = required_trimmed("operation.kind", &self.kind)?;
        if self.family.is_none() {
            self.family = canonical_operation_family(&self.kind);
        }
        self.target_id = normalize_optional_string(self.target_id.take());
        self.label = normalize_optional_string(self.label.take());
        self.tags = normalize_string_vec(std::mem::take(&mut self.tags));
        Ok(())
    }
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
        "block_render" => Some(FuzzOperationFamily::BlockRender),
        "performance_probe" => Some(FuzzOperationFamily::PerformanceProbe),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzTarget {
    #[serde(default = "fuzz_target_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<FuzzOperation>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzTarget {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_TARGET_SCHEMA);
        require_schema(&self.schema, FUZZ_TARGET_SCHEMA, "fuzz target")?;
        self.id = required_trimmed("target.id", &self.id)?;
        self.kind = required_trimmed("target.kind", &self.kind)?;
        self.label = normalize_optional_string(self.label.take());
        self.locator = normalize_optional_string(self.locator.take());
        for operation in &mut self.operations {
            operation.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzInput {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
}

impl FuzzInput {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.name = required_trimmed("input.name", &self.name)?;
        self.kind = required_trimmed("input.kind", &self.kind)?;
        self.generator = normalize_optional_string(self.generator.take());
        self.constraints = normalize_string_vec(std::mem::take(&mut self.constraints));
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzWorkload {
    #[serde(default = "fuzz_workload_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub safety_class: FuzzSafetyClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surface_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seed_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_budget_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<FuzzThreshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleContract>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzWorkload {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_WORKLOAD_SCHEMA);
        require_schema(&self.schema, FUZZ_WORKLOAD_SCHEMA, "fuzz workload")?;
        self.id = required_trimmed("workload.id", &self.id)?;
        self.label = normalize_optional_string(self.label.take());
        self.surface_ids = normalize_string_vec(std::mem::take(&mut self.surface_ids));
        self.operations = normalize_string_vec(std::mem::take(&mut self.operations));
        self.seed_ids = normalize_string_vec(std::mem::take(&mut self.seed_ids));
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCampaign {
    #[serde(default = "fuzz_campaign_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub safety_class: FuzzSafetyClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<FuzzSurface>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<FuzzTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<FuzzWorkload>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<FuzzCase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeds: Vec<FuzzSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<FuzzCoverage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_summary: Option<FuzzCoverageSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<FuzzFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<FuzzArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<FuzzThreshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<FuzzReplayMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleResultMetadata>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCase {
    #[serde(default = "fuzz_case_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub expected: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub observed: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCaseLogEntry {
    #[serde(default = "fuzz_case_log_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub case_id: String,
    pub target_id: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_family: Option<FuzzOperationFamily>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    pub input_hash: String,
    pub status: FuzzCaseLogStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<FuzzCaseLogArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzCaseLogEntry {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_CASE_LOG_SCHEMA);
        require_schema(&self.schema, FUZZ_CASE_LOG_SCHEMA, "fuzz case log")?;
        if self.version != FUZZ_CONTRACT_VERSION {
            return Err(format!(
                "fuzz case log version must be {FUZZ_CONTRACT_VERSION}"
            ));
        }
        self.case_id = required_trimmed("case_id", &self.case_id)?;
        self.target_id = required_trimmed("target_id", &self.target_id)?;
        self.operation_id = required_trimmed("operation_id", &self.operation_id)?;
        self.seed = normalize_optional_string(self.seed.take());
        self.input_hash = required_trimmed("input_hash", &self.input_hash)?;
        self.started_at = normalize_optional_string(self.started_at.take());
        self.finished_at = normalize_optional_string(self.finished_at.take());
        self.failure_fingerprint = normalize_optional_string(self.failure_fingerprint.take());
        self.skip_reason = normalize_optional_string(self.skip_reason.take());
        self.failure_reason = normalize_optional_string(self.failure_reason.take());
        for artifact in &mut self.artifact_refs {
            artifact.normalize()?;
        }
        if self.duration_ms.is_none() && self.started_at.is_none() && self.finished_at.is_none() {
            return Err("case log timing requires duration_ms or timestamps".to_string());
        }
        match self.status {
            FuzzCaseLogStatus::Skipped if self.skip_reason.is_none() => {
                Err("skipped case log entries require skip_reason".to_string())
            }
            FuzzCaseLogStatus::Failed | FuzzCaseLogStatus::Error
                if self.failure_fingerprint.is_none() && self.failure_reason.is_none() =>
            {
                Err(
                    "failed case log entries require failure_fingerprint or failure_reason"
                        .to_string(),
                )
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzCaseLogStatus {
    Passed,
    Failed,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzCaseLogArtifactRef {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl FuzzCaseLogArtifactRef {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.id = required_trimmed("artifact_refs[].id", &self.id)?;
        self.kind = required_trimmed("artifact_refs[].kind", &self.kind)?;
        self.path = normalize_optional_string(self.path.take());
        self.uri = normalize_optional_string(self.uri.take());
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzSeed {
    #[serde(default = "fuzz_seed_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzSeed {
    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_SEED_SCHEMA);
        require_schema(&self.schema, FUZZ_SEED_SCHEMA, "fuzz seed")?;
        self.id = required_trimmed("seed.id", &self.id)?;
        self.kind = required_trimmed("seed.kind", &self.kind)?;
        self.label = normalize_optional_string(self.label.take());
        self.value = normalize_optional_string(self.value.take());
        self.tags = normalize_string_vec(std::mem::take(&mut self.tags));
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCoverage {
    #[serde(default = "fuzz_coverage_schema")]
    pub schema: String,
    pub id: String,
    pub metric: String,
    pub covered: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<FuzzCoverageGap>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzCoverageGap {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCoverageSummary {
    #[serde(default = "fuzz_coverage_summary_schema")]
    pub schema: String,
    pub declared_targets: u64,
    pub executable_targets: u64,
    pub proven_targets: u64,
    pub declared_operations: u64,
    pub executable_operations: u64,
    pub proven_operations: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_targets: Vec<FuzzCoverageSkip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_operations: Vec<FuzzCoverageSkip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surface_summaries: Vec<FuzzCoverageGroupSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kind_summaries: Vec<FuzzCoverageGroupSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCoverageGroupSummary {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub declared_targets: u64,
    pub executable_targets: u64,
    pub proven_targets: u64,
    pub declared_operations: u64,
    pub executable_operations: u64,
    pub proven_operations: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_targets: Vec<FuzzCoverageSkip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_operations: Vec<FuzzCoverageSkip>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzCoverageSkip {
    pub id: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzFinding {
    #[serde(default = "fuzz_finding_schema")]
    pub schema: String,
    pub id: String,
    pub title: String,
    pub severity: String,
    pub status: FuzzFindingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzFindingStatus {
    Open,
    Confirmed,
    Mitigated,
    Suppressed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzArtifact {
    #[serde(default = "fuzz_artifact_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactContract>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzThreshold {
    #[serde(default = "fuzz_threshold_schema")]
    pub schema: String,
    pub id: String,
    pub metric: String,
    pub operator: FuzzThresholdOperator,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_class: Option<FuzzSafetyClass>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FuzzThresholdOperator {
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
    Equal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzProvenance {
    #[serde(default = "fuzz_provenance_schema")]
    pub schema: String,
    pub producer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzReplayMetadata {
    #[serde(default = "fuzz_replay_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzTargetInventory {
    #[serde(default = "fuzz_target_inventory_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<FuzzSurface>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<FuzzTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<FuzzWorkload>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeds: Vec<FuzzSeed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl FuzzTargetInventory {
    pub fn from_value(value: Value) -> std::result::Result<Self, String> {
        let mut inventory: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        inventory.normalize()?;
        Ok(inventory)
    }

    fn normalize(&mut self) -> std::result::Result<(), String> {
        self.schema = trim_or_default(&self.schema, FUZZ_TARGET_INVENTORY_SCHEMA);
        require_schema(
            &self.schema,
            FUZZ_TARGET_INVENTORY_SCHEMA,
            "fuzz target inventory",
        )?;
        if self.version != FUZZ_CONTRACT_VERSION {
            return Err(format!(
                "fuzz target inventory version must be {FUZZ_CONTRACT_VERSION}"
            ));
        }
        self.id = required_trimmed("inventory.id", &self.id)?;
        for surface in &mut self.surfaces {
            surface.normalize()?;
        }
        for target in &mut self.targets {
            target.normalize()?;
        }
        for workload in &mut self.workloads {
            workload.normalize()?;
        }
        for seed in &mut self.seeds {
            seed.normalize()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzExecutionRequest {
    #[serde(default = "fuzz_execution_request_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<FuzzRequiredArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<FuzzGate>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzResultEnvelope {
    #[serde(default = "fuzz_result_envelope_schema")]
    pub schema: String,
    #[serde(default = "fuzz_contract_version")]
    pub version: u32,
    pub id: String,
    pub status: String,
    pub request: FuzzExecutionRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<FuzzCampaign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<FuzzArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<FuzzRequiredArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<FuzzGate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FuzzProvenance>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzRequiredArtifact {
    #[serde(default = "fuzz_required_artifact_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptable_artifact_kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzGate {
    #[serde(default = "fuzz_gate_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub metric: String,
    pub operator: FuzzThresholdOperator,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
            result_envelope: FUZZ_RESULT_ENVELOPE_SCHEMA.to_string(),
            required_artifact: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            gate: FUZZ_GATE_SCHEMA.to_string(),
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

fn default_fuzz_operation_families() -> Vec<FuzzOperationFamily> {
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
        FuzzOperationFamily::BlockRender,
        FuzzOperationFamily::PerformanceProbe,
    ]
}

pub fn parse_fuzz_results_file(path: &Path) -> Result<FuzzCampaign> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let campaign: FuzzCampaign = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse fuzz results file {}", path.display())),
            Some(contents),
        )
    })?;
    if campaign.schema != FUZZ_CAMPAIGN_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "fuzz results schema must be {FUZZ_CAMPAIGN_SCHEMA}, got {}",
                campaign.schema
            ),
            Some(campaign.schema),
            None,
        ));
    }
    Ok(campaign)
}

pub fn parse_fuzz_case_log_file(path: &Path) -> Result<Vec<FuzzCaseLogEntry>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let entries = parse_fuzz_case_log_contents(&contents).map_err(|message| {
        Error::validation_invalid_argument(
            "case_log",
            message,
            Some(path.display().to_string()),
            None,
        )
    })?;
    Ok(entries)
}

pub fn parse_fuzz_case_log_contents(
    contents: &str,
) -> std::result::Result<Vec<FuzzCaseLogEntry>, String> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Err("case log must contain at least one entry".to_string());
    }

    let values = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<Value>>(trimmed).map_err(|err| err.to_string())?
    } else if trimmed.starts_with('{') {
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => match value.get("entries").and_then(Value::as_array) {
                Some(entries) => entries.clone(),
                None => vec![value],
            },
            Err(_) if trimmed.lines().count() > 1 => parse_fuzz_case_log_jsonl(trimmed)?,
            Err(error) => return Err(error.to_string()),
        }
    } else {
        parse_fuzz_case_log_jsonl(trimmed)?
    };

    if values.is_empty() {
        return Err("case log must contain at least one entry".to_string());
    }

    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let mut entry: FuzzCaseLogEntry = serde_json::from_value(value)
                .map_err(|err| format!("case log entry {}: {err}", index + 1))?;
            entry
                .normalize()
                .map_err(|err| format!("case log entry {}: {err}", index + 1))?;
            Ok(entry)
        })
        .collect()
}

fn parse_fuzz_case_log_jsonl(contents: &str) -> std::result::Result<Vec<Value>, String> {
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line.trim())
                .map_err(|err| format!("case log line {}: {err}", index + 1))
        })
        .collect()
}

pub fn parse_fuzz_result_envelope_file(path: &Path) -> Result<FuzzResultEnvelope> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let envelope: FuzzResultEnvelope = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!(
                "parse fuzz result envelope file {}",
                path.display()
            )),
            Some(contents),
        )
    })?;
    if envelope.schema != FUZZ_RESULT_ENVELOPE_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "fuzz result envelope schema must be {FUZZ_RESULT_ENVELOPE_SCHEMA}, got {}",
                envelope.schema
            ),
            Some(envelope.schema),
            None,
        ));
    }
    Ok(envelope)
}

pub fn parse_fuzz_target_inventory_file(path: &Path) -> Result<FuzzTargetInventory> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let value: Value = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!(
                "parse fuzz target inventory file {}",
                path.display()
            )),
            Some(contents.clone()),
        )
    })?;
    FuzzTargetInventory::from_value(value).map_err(|message| {
        Error::validation_invalid_argument(
            "inventory",
            message,
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn merge_fuzz_target_inventory(
    base: &mut FuzzTargetInventory,
    mut discovered: FuzzTargetInventory,
) {
    base.surfaces.append(&mut discovered.surfaces);
    base.targets.append(&mut discovered.targets);
    base.workloads.append(&mut discovered.workloads);
    base.seeds.append(&mut discovered.seeds);
    if base.provenance.is_none() {
        base.provenance = discovered.provenance;
    }
    merge_metadata(&mut base.metadata, discovered.metadata);
    base.extra.append(&mut discovered.extra);
}

fn merge_metadata(base: &mut Value, discovered: Value) {
    if discovered.is_null() {
        return;
    }
    if base.is_null() {
        *base = discovered;
        return;
    }
    match (base, discovered) {
        (Value::Object(base_map), Value::Object(incoming_map)) => {
            for (key, value) in incoming_map {
                base_map.entry(key).or_insert(value);
            }
        }
        (base, incoming) => {
            let previous = std::mem::take(base);
            *base = serde_json::json!({
                "homeboy_metadata": previous,
                "merged_inventory_metadata": incoming,
            });
        }
    }
}

pub fn default_fuzz_required_artifacts() -> Vec<FuzzRequiredArtifact> {
    vec![
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "result-envelope".to_string(),
            kind: "result_envelope".to_string(),
            required: true,
            description: Some(
                "Homeboy fuzz result envelope for the campaign or planned execution".to_string(),
            ),
            acceptable_artifact_kinds: vec!["json".to_string()],
        },
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "case-log".to_string(),
            kind: "case_log".to_string(),
            required: true,
            description: Some(
                "Case-level execution log or equivalent replayable trace".to_string(),
            ),
            acceptable_artifact_kinds: vec!["jsonl".to_string(), "text".to_string()],
        },
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "replay-data".to_string(),
            kind: "replay_data".to_string(),
            required: true,
            description: Some(
                "Inputs, seed, or fixture data required to replay a failing case".to_string(),
            ),
            acceptable_artifact_kinds: vec!["json".to_string(), "artifact".to_string()],
        },
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "coverage-summary".to_string(),
            kind: "coverage_summary".to_string(),
            required: true,
            description: Some(
                "Target and operation coverage summary with declared, executable, and proven counts"
                    .to_string(),
            ),
            acceptable_artifact_kinds: vec!["json".to_string()],
        },
    ]
}

pub fn default_fuzz_gates() -> Vec<FuzzGate> {
    vec![
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "no-open-findings".to_string(),
            kind: "finding_count".to_string(),
            metric: "open_findings".to_string(),
            operator: FuzzThresholdOperator::Equal,
            value: 0.0,
            unit: Some("count".to_string()),
            description: Some("Result envelope has no open findings".to_string()),
        },
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "has-case-evidence".to_string(),
            kind: "artifact_presence".to_string(),
            metric: "case_log_artifacts".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 1.0,
            unit: Some("count".to_string()),
            description: Some(
                "Result envelope links at least one case-level proof artifact".to_string(),
            ),
        },
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "target-coverage-complete".to_string(),
            kind: "coverage_completeness".to_string(),
            metric: "target_coverage_ratio".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 1.0,
            unit: Some("ratio".to_string()),
            description: Some(
                "Coverage summary proves every declared target, or explicitly declares zero targets"
                    .to_string(),
            ),
        },
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "operation-coverage-complete".to_string(),
            kind: "coverage_completeness".to_string(),
            metric: "operation_coverage_ratio".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 1.0,
            unit: Some("ratio".to_string()),
            description: Some(
                "Coverage summary proves every declared operation, or explicitly declares zero operations"
                    .to_string(),
            ),
        },
    ]
}

fn fuzz_core_contract_schema() -> String {
    FUZZ_CORE_CONTRACT_SCHEMA.to_string()
}

fn fuzz_contract_version() -> u32 {
    FUZZ_CONTRACT_VERSION
}

fn fuzz_surface_schema() -> String {
    FUZZ_SURFACE_SCHEMA.to_string()
}

fn fuzz_target_schema() -> String {
    FUZZ_TARGET_SCHEMA.to_string()
}

fn fuzz_workload_schema() -> String {
    FUZZ_WORKLOAD_SCHEMA.to_string()
}

fn fuzz_campaign_schema() -> String {
    FUZZ_CAMPAIGN_SCHEMA.to_string()
}

fn fuzz_case_schema() -> String {
    FUZZ_CASE_SCHEMA.to_string()
}

fn fuzz_case_log_schema() -> String {
    FUZZ_CASE_LOG_SCHEMA.to_string()
}

fn fuzz_seed_schema() -> String {
    FUZZ_SEED_SCHEMA.to_string()
}

fn fuzz_coverage_schema() -> String {
    FUZZ_COVERAGE_SCHEMA.to_string()
}

fn fuzz_finding_schema() -> String {
    FUZZ_FINDING_SCHEMA.to_string()
}

fn fuzz_artifact_schema() -> String {
    FUZZ_ARTIFACT_SCHEMA.to_string()
}

fn fuzz_threshold_schema() -> String {
    FUZZ_THRESHOLD_SCHEMA.to_string()
}

fn fuzz_provenance_schema() -> String {
    FUZZ_PROVENANCE_SCHEMA.to_string()
}

fn fuzz_replay_schema() -> String {
    FUZZ_REPLAY_SCHEMA.to_string()
}

fn fuzz_coverage_summary_schema() -> String {
    FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string()
}

fn fuzz_target_inventory_schema() -> String {
    FUZZ_TARGET_INVENTORY_SCHEMA.to_string()
}

fn fuzz_execution_request_schema() -> String {
    FUZZ_EXECUTION_REQUEST_SCHEMA.to_string()
}

fn fuzz_result_envelope_schema() -> String {
    FUZZ_RESULT_ENVELOPE_SCHEMA.to_string()
}

fn fuzz_required_artifact_schema() -> String {
    FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string()
}

fn fuzz_gate_schema() -> String {
    FUZZ_GATE_SCHEMA.to_string()
}

fn lifecycle_contract_schema() -> String {
    crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA.to_string()
}

fn lifecycle_result_schema() -> String {
    crate::core::lifecycle::LIFECYCLE_RESULT_SCHEMA.to_string()
}

fn lifecycle_snapshot_ref_schema() -> String {
    crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string()
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn normalize_string_vec(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

fn trim_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn required_trimmed(field: &str, value: &str) -> std::result::Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} must be non-empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn require_schema(actual: &str, expected: &str, label: &str) -> std::result::Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} schema must be {expected}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
            contract.schemas.result_envelope,
            FUZZ_RESULT_ENVELOPE_SCHEMA
        );
        assert_eq!(
            contract.schemas.required_artifact,
            FUZZ_REQUIRED_ARTIFACT_SCHEMA
        );
        assert_eq!(contract.schemas.gate, FUZZ_GATE_SCHEMA);
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
            .contains(&FuzzOperationFamily::BlockRender));
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

    #[test]
    fn surface_normalizes_optional_fields_and_nested_inputs() {
        let surface = FuzzSurface::from_value(json!({
            "id": " orders ",
            "kind": " api ",
            "label": " ",
            "target": " https://example.test/resource ",
            "safety_class": "read_only",
            "operations": [
                { "id": " list ", "kind": " read ", "tags": [" stable ", " "] }
            ],
            "inputs": [
                { "name": " query ", "kind": " string ", "generator": " ascii ", "constraints": [" max:64 "] }
            ],
            "owner": "extension"
        }))
        .expect("surface contract");

        assert_eq!(surface.schema, FUZZ_SURFACE_SCHEMA);
        assert_eq!(surface.id, "orders");
        assert_eq!(surface.kind, "api");
        assert_eq!(surface.label, None);
        assert_eq!(
            surface.target.as_deref(),
            Some("https://example.test/resource")
        );
        assert_eq!(surface.operations[0].tags, vec!["stable"]);
        assert_eq!(
            surface.operations[0].family,
            Some(FuzzOperationFamily::Read)
        );
        assert_eq!(surface.inputs[0].constraints, vec!["max:64"]);
        assert_eq!(surface.extra["owner"], "extension");
    }

    #[test]
    fn operation_deserializes_old_payload_and_preserves_custom_kind() {
        let surface = FuzzSurface::from_value(json!({
            "id": "surface-1",
            "kind": "api",
            "safety_class": "read_only",
            "operations": [
                { "id": "custom-1", "kind": "domain_specific_probe" }
            ]
        }))
        .expect("surface contract");

        assert_eq!(surface.operations[0].kind, "domain_specific_probe");
        assert_eq!(surface.operations[0].family, None);
    }

    #[test]
    fn operation_normalizes_canonical_families_from_kind() {
        let surface = FuzzSurface::from_value(json!({
            "id": "surface-1",
            "kind": "api",
            "safety_class": "read_only",
            "operations": [
                { "id": "read-1", "kind": " GET " },
                { "id": "create-1", "kind": "post" },
                { "id": "update-1", "kind": "PATCH" },
                { "id": "delete-1", "kind": "delete" },
                { "id": "block-render-1", "kind": "block-render" },
                { "id": "performance-1", "kind": "performance probe" }
            ]
        }))
        .expect("surface contract");

        let families: Vec<Option<FuzzOperationFamily>> = surface
            .operations
            .iter()
            .map(|operation| operation.family)
            .collect();

        assert_eq!(
            families,
            vec![
                Some(FuzzOperationFamily::Read),
                Some(FuzzOperationFamily::Create),
                Some(FuzzOperationFamily::Update),
                Some(FuzzOperationFamily::Delete),
                Some(FuzzOperationFamily::BlockRender),
                Some(FuzzOperationFamily::PerformanceProbe),
            ]
        );
        assert_eq!(surface.operations[0].kind, "GET");
    }

    #[test]
    fn operation_preserves_declared_canonical_family() {
        let surface = FuzzSurface::from_value(json!({
            "id": "surface-1",
            "kind": "api",
            "safety_class": "read_only",
            "operations": [
                { "id": "custom-search", "kind": "bespoke_lookup", "family": "search" }
            ]
        }))
        .expect("surface contract");

        assert_eq!(surface.operations[0].kind, "bespoke_lookup");
        assert_eq!(
            surface.operations[0].family,
            Some(FuzzOperationFamily::Search)
        );
    }

    #[test]
    fn campaign_serializes_seeds_coverage_findings_artifacts_thresholds_and_provenance() {
        let campaign = FuzzCampaign {
            schema: FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: "campaign-1".to_string(),
            title: Some("generic campaign".to_string()),
            safety_class: FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: vec![FuzzTarget {
                schema: FUZZ_TARGET_SCHEMA.to_string(),
                id: "target-1".to_string(),
                kind: "api".to_string(),
                label: None,
                locator: Some("https://example.test/orders".to_string()),
                operations: vec![FuzzOperation {
                    id: "operation-1".to_string(),
                    kind: "read".to_string(),
                    family: Some(FuzzOperationFamily::Read),
                    target_id: Some("target-1".to_string()),
                    label: None,
                    tags: Vec::new(),
                }],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            workloads: vec![FuzzWorkload {
                schema: FUZZ_WORKLOAD_SCHEMA.to_string(),
                id: "workload-1".to_string(),
                label: None,
                safety_class: FuzzSafetyClass::ReadOnly,
                surface_ids: vec!["surface-1".to_string()],
                operations: vec!["read".to_string()],
                seed_ids: vec!["seed-1".to_string()],
                case_budget: Some(100),
                duration_budget_seconds: Some(60),
                thresholds: Vec::new(),
                lifecycle: Some(LifecycleContract {
                    schema: crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA.to_string(),
                    version: crate::core::lifecycle::LIFECYCLE_CONTRACT_VERSION,
                    phases: vec![crate::core::lifecycle::LifecyclePhaseContract {
                        id: "snapshot".to_string(),
                        phase: crate::core::lifecycle::LifecyclePhaseKind::Snapshot,
                        label: None,
                        extension_hook: Some("runtime.snapshot".to_string()),
                        command: None,
                        timeout_seconds: None,
                        required: Some(true),
                    }],
                    metadata: BTreeMap::new(),
                }),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            cases: vec![FuzzCase {
                schema: FUZZ_CASE_SCHEMA.to_string(),
                id: "case-1".to_string(),
                target_id: Some("target-1".to_string()),
                operation_id: Some("operation-1".to_string()),
                workload_id: Some("workload-1".to_string()),
                seed_id: Some("seed-1".to_string()),
                replay_id: Some("replay-1".to_string()),
                input: serde_json::json!({ "path": "/orders" }),
                expected: Value::Null,
                observed: Value::Null,
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            seeds: vec![FuzzSeed {
                schema: FUZZ_SEED_SCHEMA.to_string(),
                id: "seed-1".to_string(),
                kind: "literal".to_string(),
                label: None,
                value: Some("sample".to_string()),
                artifact: None,
                tags: Vec::new(),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            coverage: vec![FuzzCoverage {
                schema: FUZZ_COVERAGE_SCHEMA.to_string(),
                id: "coverage-1".to_string(),
                metric: "operation".to_string(),
                covered: 8,
                total: 10,
                ratio: Some(0.8),
                gaps: vec![FuzzCoverageGap {
                    id: "gap-1".to_string(),
                    label: None,
                    surface_id: Some("surface-1".to_string()),
                    target_id: Some("target-1".to_string()),
                    operation: Some("write".to_string()),
                    operation_id: Some("operation-1".to_string()),
                }],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            coverage_summary: Some(FuzzCoverageSummary {
                schema: FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
                declared_targets: 1,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 1,
                executable_operations: 1,
                proven_operations: 1,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                surface_summaries: vec![FuzzCoverageGroupSummary {
                    id: "surface-1".to_string(),
                    kind: "api".to_string(),
                    label: Some("API".to_string()),
                    declared_targets: 1,
                    executable_targets: 1,
                    proven_targets: 1,
                    declared_operations: 1,
                    executable_operations: 1,
                    proven_operations: 1,
                    skipped_targets: Vec::new(),
                    skipped_operations: Vec::new(),
                    metadata: Value::Null,
                    extra: BTreeMap::new(),
                }],
                kind_summaries: vec![FuzzCoverageGroupSummary {
                    id: "read".to_string(),
                    kind: "operation_kind".to_string(),
                    label: None,
                    declared_targets: 1,
                    executable_targets: 1,
                    proven_targets: 1,
                    declared_operations: 1,
                    executable_operations: 1,
                    proven_operations: 1,
                    skipped_targets: Vec::new(),
                    skipped_operations: Vec::new(),
                    metadata: Value::Null,
                    extra: BTreeMap::new(),
                }],
                artifact_ids: vec!["artifact-1".to_string()],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }),
            findings: vec![FuzzFinding {
                schema: FUZZ_FINDING_SCHEMA.to_string(),
                id: "finding-1".to_string(),
                title: "unexpected response".to_string(),
                severity: "high".to_string(),
                status: FuzzFindingStatus::Open,
                surface_id: Some("surface-1".to_string()),
                target_id: Some("target-1".to_string()),
                operation_id: Some("operation-1".to_string()),
                case_id: Some("case-1".to_string()),
                workload_id: Some("workload-1".to_string()),
                seed_id: Some("seed-1".to_string()),
                fingerprint: Some("abc123".to_string()),
                artifact_ids: vec!["artifact-1".to_string()],
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            artifacts: vec![FuzzArtifact {
                schema: FUZZ_ARTIFACT_SCHEMA.to_string(),
                id: "artifact-1".to_string(),
                kind: "log".to_string(),
                artifact: None,
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            thresholds: vec![FuzzThreshold {
                schema: FUZZ_THRESHOLD_SCHEMA.to_string(),
                id: "threshold-1".to_string(),
                metric: "coverage_ratio".to_string(),
                operator: FuzzThresholdOperator::GreaterThanOrEqual,
                value: 0.75,
                unit: None,
                safety_class: Some(FuzzSafetyClass::ReadOnly),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            provenance: Some(FuzzProvenance {
                schema: FUZZ_PROVENANCE_SCHEMA.to_string(),
                producer: "runner".to_string(),
                producer_version: Some("1.0.0".to_string()),
                invocation: Some("run campaign-1".to_string()),
                run_id: Some("run-1".to_string()),
                source_ref: Some("abc123".to_string()),
                created_at: Some("2026-06-21T00:00:00Z".to_string()),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }),
            replay: Some(FuzzReplayMetadata {
                schema: FUZZ_REPLAY_SCHEMA.to_string(),
                id: "replay-1".to_string(),
                command: Some("homeboy fuzz run component --workload workload-1".to_string()),
                args: Vec::new(),
                env: vec!["HOMEBOY_FUZZ_SEED=sample".to_string()],
                seed: Some("sample".to_string()),
                artifact_id: Some("artifact-1".to_string()),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }),
            lifecycle: Some(crate::core::lifecycle::LifecycleResultMetadata {
                schema: crate::core::lifecycle::LIFECYCLE_RESULT_SCHEMA.to_string(),
                version: crate::core::lifecycle::LIFECYCLE_CONTRACT_VERSION,
                phases: vec![crate::core::lifecycle::LifecyclePhaseResult {
                    id: "snapshot".to_string(),
                    phase: crate::core::lifecycle::LifecyclePhaseKind::Snapshot,
                    status: crate::core::lifecycle::LifecyclePhaseStatus::Passed,
                    snapshot_ref: Some("snapshot-1".to_string()),
                    started_at: None,
                    finished_at: None,
                    message: None,
                }],
                snapshot_refs: vec![crate::core::lifecycle::LifecycleSnapshotRef {
                    schema: crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string(),
                    id: "snapshot-1".to_string(),
                    kind: "database".to_string(),
                    phase_id: Some("snapshot".to_string()),
                    artifact_id: Some("artifact-1".to_string()),
                    artifact: None,
                    locator: None,
                    created_at: None,
                    metadata: BTreeMap::new(),
                }],
                metadata: BTreeMap::new(),
            }),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        };

        let value = serde_json::to_value(campaign).expect("campaign json");

        assert_eq!(value["schema"], FUZZ_CAMPAIGN_SCHEMA);
        assert_eq!(value["version"], FUZZ_CONTRACT_VERSION);
        assert_eq!(value["targets"][0]["schema"], FUZZ_TARGET_SCHEMA);
        assert_eq!(value["workloads"][0]["schema"], FUZZ_WORKLOAD_SCHEMA);
        assert_eq!(value["cases"][0]["schema"], FUZZ_CASE_SCHEMA);
        assert_eq!(value["seeds"][0]["schema"], FUZZ_SEED_SCHEMA);
        assert_eq!(value["coverage"][0]["schema"], FUZZ_COVERAGE_SCHEMA);
        assert_eq!(
            value["coverage_summary"]["schema"],
            FUZZ_COVERAGE_SUMMARY_SCHEMA
        );
        assert_eq!(
            value["coverage_summary"]["surface_summaries"][0]["id"],
            "surface-1"
        );
        assert_eq!(value["coverage_summary"]["kind_summaries"][0]["id"], "read");
        assert_eq!(value["findings"][0]["schema"], FUZZ_FINDING_SCHEMA);
        assert_eq!(value["artifacts"][0]["schema"], FUZZ_ARTIFACT_SCHEMA);
        assert_eq!(value["thresholds"][0]["schema"], FUZZ_THRESHOLD_SCHEMA);
        assert_eq!(value["provenance"]["schema"], FUZZ_PROVENANCE_SCHEMA);
        assert_eq!(value["replay"]["schema"], FUZZ_REPLAY_SCHEMA);
        assert_eq!(
            value["workloads"][0]["lifecycle"]["schema"],
            crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA
        );
        assert_eq!(
            value["lifecycle"]["snapshot_refs"][0]["schema"],
            crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA
        );
    }

    #[test]
    fn parse_fuzz_case_log_contents_accepts_jsonl_entries() {
        let entries = parse_fuzz_case_log_contents(
            r#"{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","operation_family":"read","seed":"seed-1","input_hash":"sha256:abc","status":"passed","duration_ms":12,"artifact_refs":[{"id":"artifact-1","kind":"stdout","path":"logs/case-1.txt"}]}
{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-2","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:def","status":"skipped","duration_ms":1,"skip_reason":"auth_required"}"#,
        )
        .expect("parse case log jsonl");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].schema, FUZZ_CASE_LOG_SCHEMA);
        assert_eq!(entries[0].operation_family, Some(FuzzOperationFamily::Read));
        assert_eq!(entries[0].artifact_refs[0].kind, "stdout");
        assert_eq!(entries[1].status, FuzzCaseLogStatus::Skipped);
        assert_eq!(entries[1].skip_reason.as_deref(), Some("auth_required"));
    }

    #[test]
    fn parse_fuzz_case_log_contents_rejects_invalid_entries() {
        let err = parse_fuzz_case_log_contents(
            r#"[{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"failed","duration_ms":4}]"#,
        )
        .expect_err("failed entry without reason should fail");

        assert!(err.contains("failure_fingerprint or failure_reason"));

        let err = parse_fuzz_case_log_contents(
            r#"{"entries":[{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"passed"}]}"#,
        )
        .expect_err("entry without timing should fail");

        assert!(err.contains("duration_ms or timestamps"));
    }

    #[test]
    fn parse_fuzz_results_file_reads_campaign_contract() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fuzz-results.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema": FUZZ_CAMPAIGN_SCHEMA,
                "id": "campaign-1",
                "safety_class": "read_only"
            })
            .to_string(),
        )
        .expect("write fuzz results");

        let parsed = parse_fuzz_results_file(&path).expect("parse fuzz results");

        assert_eq!(parsed.id, "campaign-1");
        assert_eq!(parsed.safety_class, FuzzSafetyClass::ReadOnly);
    }

    #[test]
    fn parse_fuzz_target_inventory_file_reads_and_normalizes_inventory_contract() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fuzz-inventory.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema": FUZZ_TARGET_INVENTORY_SCHEMA,
                "id": " discovered ",
                "surfaces": [{
                    "id": " api ",
                    "kind": " rest ",
                    "safety_class": "read_only"
                }],
                "targets": [{
                    "id": " target-1 ",
                    "kind": " endpoint "
                }],
                "workloads": [{
                    "id": " workload-1 ",
                    "safety_class": "read_only",
                    "surface_ids": [" api ", " "]
                }],
                "seeds": [{
                    "id": " seed-1 ",
                    "kind": " literal ",
                    "tags": [" stable ", " "]
                }]
            })
            .to_string(),
        )
        .expect("write fuzz inventory");

        let parsed = parse_fuzz_target_inventory_file(&path).expect("parse fuzz inventory");

        assert_eq!(parsed.id, "discovered");
        assert_eq!(parsed.surfaces[0].id, "api");
        assert_eq!(parsed.targets[0].kind, "endpoint");
        assert_eq!(parsed.workloads[0].surface_ids, vec!["api"]);
        assert_eq!(parsed.seeds[0].tags, vec!["stable"]);
    }

    #[test]
    fn merge_fuzz_target_inventory_appends_discovered_contract_sections() {
        let mut base = FuzzTargetInventory {
            schema: FUZZ_TARGET_INVENTORY_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: "base".to_string(),
            surfaces: Vec::new(),
            targets: Vec::new(),
            workloads: Vec::new(),
            seeds: Vec::new(),
            provenance: None,
            metadata: json!({ "declared_workloads": [] }),
            extra: BTreeMap::new(),
        };
        let discovered = FuzzTargetInventory::from_value(json!({
            "schema": FUZZ_TARGET_INVENTORY_SCHEMA,
            "id": "discovered",
            "surfaces": [{
                "id": "api",
                "kind": "rest",
                "safety_class": "read_only"
            }],
            "metadata": { "producer": "runner" }
        }))
        .expect("inventory contract");

        merge_fuzz_target_inventory(&mut base, discovered);

        assert_eq!(base.id, "base");
        assert_eq!(base.surfaces.len(), 1);
        assert_eq!(base.metadata["declared_workloads"], json!([]));
        assert_eq!(base.metadata["producer"], "runner");
    }
}
