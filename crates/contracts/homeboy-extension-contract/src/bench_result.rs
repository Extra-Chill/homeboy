//! Pure bench result contract types (provenance, metrics, rig-package
//! evidence, workload/runner metadata).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn is_false(value: &bool) -> bool {
    !*value
}

fn provenance_is_empty(value: &BenchProvenance) -> bool {
    value.is_empty()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RigPackageEvidence {
    pub rig_id: String,
    pub package_root: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_dirty: bool,
    pub linked: bool,
    pub materialized: bool,
    pub freshness: RigPackageFreshness,
    pub freshness_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RigPackageFreshness {
    Verified,
    Stale,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchRunExecution {
    pub runs: u64,
    pub concurrency: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchWorkloadMetadata {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "provenance_is_empty")]
    pub provenance: BenchProvenance,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchProvenance {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<BenchProvenanceLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

impl BenchProvenance {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.labels.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchProvenanceLink {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchRunnerMetadata {
    pub extension: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchChildCommandFailure {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BenchMetrics {
    #[serde(flatten)]
    pub values: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub distributions: BTreeMap<String, Vec<f64>>,
}

impl BenchMetrics {
    pub fn get(&self, key: &str) -> Option<f64> {
        self.values.get(key).copied()
    }

    pub fn distribution(&self, key: &str) -> Option<&[f64]> {
        self.distributions.get(key).map(Vec::as_slice)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchMetricPolicy {
    pub direction: BenchMetricDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold_absolute: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub variance_aware: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_iterations_for_variance: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_test: Option<RegressionTest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<BenchMetricPhase>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BenchMetricDirection {
    #[serde(rename = "lower_is_better", alias = "lower")]
    LowerIsBetter,
    #[serde(rename = "higher_is_better", alias = "higher")]
    HigherIsBetter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegressionTest {
    PointDelta,
    MannWhitneyU,
    KolmogorovSmirnov,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum BenchMetricPhase {
    Cold,
    Warm,
    Amortized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchMemory {
    pub peak_bytes: u64,
}
