//! Coverage, finding, artifact, threshold, provenance, and replay contracts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_contract::ArtifactContract;

use super::contract::{FuzzFindingStatus, FuzzSafetyClass};
use super::schema_defaults::{
    fuzz_artifact_schema, fuzz_coverage_schema, fuzz_coverage_summary_schema, fuzz_finding_schema,
    fuzz_provenance_schema, fuzz_replay_schema, fuzz_threshold_schema,
};
use super::schemas::{FUZZ_CONTRACT_VERSION, FUZZ_COVERAGE_RECONCILIATION_SCHEMA};
use super::{FuzzCampaign, FuzzCase, FuzzExecutionRequest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FuzzCoverageReconciliation {
    pub schema: String,
    pub version: u32,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    pub planned_operations: u64,
    pub executed_operations: u64,
    pub skipped_operations: u64,
    pub untested_operations: u64,
    pub planned_cases: u64,
    pub executed_cases: u64,
    pub skipped_cases: u64,
    pub failed_cases: u64,
    pub errored_cases: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub case_status_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skipped_reason_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untested_operation_ids: Vec<String>,
}

pub fn reconcile_fuzz_coverage(
    request: &FuzzExecutionRequest,
    campaign: &FuzzCampaign,
) -> FuzzCoverageReconciliation {
    let planned_operation_ids = planned_operation_ids(request);
    let executed_operation_ids = campaign
        .cases
        .iter()
        .filter(|case| case_status(case) != Some("skipped"))
        .filter_map(|case| case.operation_id.as_deref())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let skipped_operation_ids = skipped_operation_ids(request, campaign);
    let untested_operation_ids = planned_operation_ids
        .difference(&executed_operation_ids)
        .filter(|id| !skipped_operation_ids.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    let case_status_counts = campaign_case_status_counts(campaign);
    let skipped_cases = *case_status_counts.get("skipped").unwrap_or(&0);
    let failed_cases = *case_status_counts.get("failed").unwrap_or(&0);
    let errored_cases = *case_status_counts.get("error").unwrap_or(&0);
    let planned_cases = if request.case_ids.is_empty() {
        campaign.cases.len() as u64
    } else {
        request.case_ids.len() as u64
    };

    FuzzCoverageReconciliation {
        schema: FUZZ_COVERAGE_RECONCILIATION_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        request_id: request.id.clone(),
        campaign_id: Some(campaign.id.clone()),
        planned_operations: planned_operation_ids.len() as u64,
        executed_operations: planned_operation_ids
            .intersection(&executed_operation_ids)
            .count() as u64,
        skipped_operations: planned_operation_ids
            .intersection(&skipped_operation_ids)
            .count() as u64,
        untested_operations: untested_operation_ids.len() as u64,
        planned_cases,
        executed_cases: campaign.cases.len() as u64 - skipped_cases,
        skipped_cases,
        failed_cases,
        errored_cases,
        case_status_counts,
        skipped_reason_counts: skipped_reason_counts(request, campaign),
        untested_operation_ids,
    }
}

fn planned_operation_ids(request: &FuzzExecutionRequest) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    collect_operation_stratum_values(request, &mut ids);
    collect_selection_operations(&request.metadata, &mut ids);
    collect_skipped_operation_ids(&request.metadata, &mut ids);
    ids
}

fn skipped_operation_ids(
    request: &FuzzExecutionRequest,
    campaign: &FuzzCampaign,
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    collect_skipped_operation_ids(&request.metadata, &mut ids);
    if let Some(summary) = campaign.coverage_summary.as_ref() {
        ids.extend(
            summary
                .skipped_operations
                .iter()
                .map(|skip| skip.id.clone()),
        );
    }
    ids
}

fn collect_operation_stratum_values(request: &FuzzExecutionRequest, ids: &mut BTreeSet<String>) {
    for stratum in &request.sampling.operation_strata {
        if stratum.kind == "operation" || stratum.id == "selected-operations" {
            ids.extend(stratum.values.iter().filter_map(|value| trimmed(value)));
        }
    }
}

fn collect_selection_operations(value: &serde_json::Value, ids: &mut BTreeSet<String>) {
    if let Some(operations) = value
        .get("selection")
        .and_then(|selection| selection.get("operations"))
        .and_then(serde_json::Value::as_array)
    {
        for operation in operations {
            if let Some(id) = operation
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
            {
                if let Some(id) = trimmed(id) {
                    ids.insert(id);
                }
            }
        }
    }
}

fn collect_skipped_operation_ids(value: &serde_json::Value, ids: &mut BTreeSet<String>) {
    if let Some(operations) = value
        .get("skipped")
        .and_then(|skipped| skipped.get("operations"))
        .and_then(serde_json::Value::as_array)
    {
        for operation in operations {
            if let Some(id) = operation
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
            {
                if let Some(id) = trimmed(id) {
                    ids.insert(id);
                }
            }
        }
    }
}

fn campaign_case_status_counts(campaign: &FuzzCampaign) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for case in &campaign.cases {
        let status = case_status(case).unwrap_or("executed").to_string();
        *counts.entry(status).or_insert(0) += 1;
    }
    counts
}

fn case_status(case: &FuzzCase) -> Option<&'static str> {
    status_value(&case.metadata)
        .or_else(|| status_value(&case.observed))
        .or_else(|| case.extra.get("status").and_then(status_value))
}

fn status_value(value: &serde_json::Value) -> Option<&'static str> {
    let raw = value
        .get("status")
        .or_else(|| value.get("outcome"))
        .and_then(serde_json::Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "pass" | "passed" | "success" | "succeeded" => Some("passed"),
        "fail" | "failed" | "failure" => Some("failed"),
        "error" | "errored" => Some("error"),
        "skip" | "skipped" => Some("skipped"),
        _ => Some("executed"),
    }
}

fn skipped_reason_counts(
    request: &FuzzExecutionRequest,
    campaign: &FuzzCampaign,
) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    collect_request_skipped_reasons(&request.metadata, &mut counts);
    if let Some(summary) = campaign.coverage_summary.as_ref() {
        for skip in summary
            .skipped_targets
            .iter()
            .chain(summary.skipped_operations.iter())
        {
            *counts.entry(skip.reason.clone()).or_insert(0) += 1;
        }
    }
    for case in &campaign.cases {
        if case_status(case) == Some("skipped") {
            if let Some(reason) = case
                .metadata
                .get("skip_reason")
                .or_else(|| case.metadata.get("reason"))
                .and_then(serde_json::Value::as_str)
                .and_then(trimmed)
            {
                *counts.entry(reason).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn collect_request_skipped_reasons(value: &serde_json::Value, counts: &mut BTreeMap<String, u64>) {
    let Some(skipped) = value.get("skipped") else {
        return;
    };
    for key in ["targets", "operations"] {
        if let Some(items) = skipped.get(key).and_then(serde_json::Value::as_array) {
            for item in items {
                if let Some(reason) = item
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .and_then(trimmed)
                {
                    *counts.entry(reason).or_insert(0) += 1;
                }
            }
        }
    }
}

fn trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
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
