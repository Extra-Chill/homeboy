use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::finding::HomeboyFinding;
use crate::core::lifecycle::LifecycleResultMetadata;
use crate::core::observation::timeline::{
    ObservationEvent, ObservationSpanDefinition, ObservationSpanResult,
};

use super::artifact::BenchArtifact;
use super::diagnostic::BenchDiagnostic;
use super::distribution::BenchRunDistribution;
use super::gate::{BenchGate, BenchGateResult};
use super::metric_policy_preset::BenchMetricPolicyPreset;
use super::phase_events::{BenchPhaseEvent, BenchPhaseFailureClassification, BenchPhaseSummary};
use super::responsiveness::BenchResponsivenessSummary;

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn provenance_is_empty(value: &BenchProvenance) -> bool {
    value.is_empty()
}

/// Full bench run output from an extension script.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchResults {
    pub component_id: String,
    pub iterations: u64,
    #[serde(default, skip_serializing_if = "provenance_is_empty")]
    pub provenance: BenchProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_metadata: Option<BenchRunMetadata>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_groups: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub span_definitions: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_events: Vec<BenchPhaseEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_summaries: Vec<BenchPhaseSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<BenchPhaseFailureClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsiveness: Option<BenchResponsivenessSummary>,
    #[serde(
        default,
        deserialize_with = "super::budget_findings::deserialize_budget_findings",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub budget_findings: Vec<HomeboyFinding>,
    pub scenarios: Vec<BenchScenario>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_policies: BTreeMap<String, BenchMetricPolicy>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_policy_presets: BTreeMap<String, BenchMetricPolicyPreset>,
}

/// Homeboy-owned reproducibility metadata for a bench invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchRunMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_version: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_state: Option<String>,
    pub iterations: u64,
    #[serde(flatten)]
    pub execution: BenchRunExecution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warmup_iterations: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_scenarios: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_overrides: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<BenchWorkloadMetadata>,
    #[serde(default, skip_serializing_if = "provenance_is_empty")]
    pub provenance: BenchProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<BenchRunnerMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig_package: Option<RigPackageEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleResultMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
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

/// One scenario's measurements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchScenario {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_iterations: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub iterations: u64,
    pub metrics: BenchMetrics,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_groups: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ObservationEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_definitions: Vec<ObservationSpanDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_results: Vec<ObservationSpanResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<BenchGate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<BenchGateResult>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "provenance_is_empty")]
    pub provenance: BenchProvenance,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, BenchArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<BenchRunSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs_summary: Option<BTreeMap<String, BenchRunDistribution>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchRunSnapshot {
    pub metrics: BenchMetrics,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_groups: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ObservationEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_definitions: Vec<ObservationSpanDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_results: Vec<ObservationSpanResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, BenchArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
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
