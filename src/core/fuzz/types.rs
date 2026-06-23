//! Core fuzz envelope types: surfaces, targets, workloads, campaigns, cases,
//! case logs, and seeds, with their normalization rules.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_contract::ArtifactContract;
use crate::core::lifecycle::{LifecycleContract, LifecycleResultMetadata};

use super::contract::{canonical_operation_family, FuzzOperationFamily, FuzzSafetyClass};
use super::coverage::{
    FuzzArtifact, FuzzCoverage, FuzzCoverageSummary, FuzzFinding, FuzzProvenance,
    FuzzReplayMetadata, FuzzThreshold,
};
use super::normalize::{
    normalize_optional_string, normalize_string_vec, require_schema, required_trimmed,
    trim_or_default,
};
use super::schema_defaults::{
    fuzz_campaign_schema, fuzz_case_log_schema, fuzz_case_schema, fuzz_contract_version,
    fuzz_seed_schema, fuzz_surface_schema, fuzz_target_schema, fuzz_workload_schema,
};
use super::schemas::{
    FUZZ_CASE_LOG_SCHEMA, FUZZ_CONTRACT_VERSION, FUZZ_SEED_SCHEMA, FUZZ_SURFACE_SCHEMA,
    FUZZ_TARGET_SCHEMA, FUZZ_WORKLOAD_SCHEMA,
};

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

    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
    pub(super) fn normalize(&mut self) -> std::result::Result<(), String> {
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
