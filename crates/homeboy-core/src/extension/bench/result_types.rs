use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::finding::HomeboyFinding;
use crate::lifecycle::LifecycleResultMetadata;
use crate::observation::timeline::{
    ObservationEvent, ObservationSpanDefinition, ObservationSpanResult,
};

use super::artifact::BenchArtifact;
use super::diagnostic::BenchDiagnostic;
use super::distribution::BenchRunDistribution;
use super::gate::{BenchGate, BenchGateResult};
use super::metric_policy_preset::BenchMetricPolicyPreset;
use super::phase_events::{BenchPhaseEvent, BenchPhaseFailureClassification, BenchPhaseSummary};
use super::responsiveness::BenchResponsivenessSummary;

fn provenance_is_empty(value: &BenchProvenance) -> bool {
    value.is_empty()
}
pub use homeboy_extension_contract::bench_result::{
    BenchChildCommandFailure, BenchMemory, BenchMetricDirection, BenchMetricPhase,
    BenchMetricPolicy, BenchMetrics, BenchProvenance, BenchProvenanceLink, BenchRunExecution,
    BenchRunnerMetadata, BenchWorkloadMetadata, RegressionTest, RigPackageEvidence,
    RigPackageFreshness,
};

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
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
    pub child_command_failures: Vec<BenchChildCommandFailure>,
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
