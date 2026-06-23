//! Coverage, finding, artifact, threshold, provenance, and replay contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_contract::ArtifactContract;

use super::contract::{FuzzFindingStatus, FuzzSafetyClass};
use super::schema_defaults::{
    fuzz_artifact_schema, fuzz_coverage_schema, fuzz_coverage_summary_schema, fuzz_finding_schema,
    fuzz_provenance_schema, fuzz_replay_schema, fuzz_threshold_schema,
};

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
