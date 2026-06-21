use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use super::{RunDetail, RunSummary};

#[derive(Serialize)]
pub struct BenchHistoryOutput {
    pub command: &'static str,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    pub runs: Vec<RunDetail>,
}

#[derive(Serialize)]
pub struct BenchCompareOutput {
    pub command: &'static str,
    pub baseline: BenchComparisonSide,
    pub candidate: BenchComparisonSide,
    pub shared: BenchComparisonSharedContext,
    pub comparisons: Vec<BenchMetricComparison>,
    pub missing: Vec<BenchMissingMetric>,
    pub reports: BenchCompareReports,
}

#[derive(Serialize)]
pub struct BenchComparisonSide {
    pub label: &'static str,
    pub run: RunSummary,
    pub component_state: BenchComponentState,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchComponentState {
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub git_sha: Option<String>,
    pub cwd: Option<String>,
    pub component_path: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BenchComparisonSharedContext {
    pub settings: BTreeMap<String, Value>,
    pub selected_scenarios: Vec<String>,
    pub workloads: Vec<BenchWorkloadFingerprint>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<BenchProvenanceEntry>,
    pub iterations: Option<u64>,
    pub runs: Option<u64>,
    pub concurrency: Option<u64>,
    pub shared_state: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchWorkloadFingerprint {
    pub id: String,
    pub sha256: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchProvenanceEntry {
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<BenchProvenanceLinkEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchProvenanceLinkEntry {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchCompareReports {
    pub markdown: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BenchMetricComparison {
    pub scenario_id: String,
    pub metric: String,
    pub from_value: f64,
    pub to_value: f64,
    pub delta: f64,
    pub percent_change: Option<f64>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchMissingMetric {
    pub scenario_id: String,
    pub metric: String,
    pub missing_from: String,
}
