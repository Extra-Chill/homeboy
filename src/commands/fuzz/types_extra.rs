use serde::Serialize;
use std::collections::BTreeMap;

use homeboy::core::fuzz::{
    FuzzCampaign, FuzzExecutionRequest, FuzzGate, FuzzReplayMetadata, FuzzRequiredArtifact,
    FuzzResultEnvelope, FuzzTargetInventory,
};
use homeboy::core::performance_hotspots::PerformanceHotspotSummary;

#[derive(Serialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum FuzzOutput {
    Contract(FuzzContractOutput),
    Discover(FuzzDiscoverOutput),
    List(FuzzListOutput),
    Plan(FuzzPlanOutput),
    Run(FuzzRunOutput),
    Validate(FuzzValidateOutput),
    Report(FuzzReportOutput),
    Compare(FuzzCompareOutput),
    Replay(FuzzReplayOutput),
}

#[derive(Serialize)]
pub struct FuzzContractOutput {
    pub command: String,
    pub contract: homeboy::core::fuzz::FuzzCoreContract,
    pub required_artifacts: Vec<FuzzRequiredArtifact>,
    pub gates: Vec<FuzzGate>,
}

#[derive(Debug, Serialize)]
pub struct FuzzDiscoverOutput {
    pub command: String,
    pub status: String,
    pub source_label: String,
    pub inventory_files: Vec<String>,
    pub target_inventory: FuzzTargetInventory,
    pub summary: FuzzDiscoverSummary,
}

#[derive(Debug, Serialize)]
pub struct FuzzDiscoverSummary {
    pub surfaces: usize,
    pub targets: usize,
    pub workloads: usize,
    pub seeds: usize,
}

#[derive(Serialize)]
pub struct FuzzListOutput {
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub workloads: Vec<FuzzWorkloadOutput>,
    pub count: usize,
    pub run_hint: String,
}

#[derive(Serialize)]
pub struct FuzzRunOutput {
    pub kind: String,
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub status: String,
    pub workload_id: Option<String>,
    pub workload_path: Option<String>,
    pub run_id: Option<String>,
    pub seed: Option<String>,
    pub inventory_file: Option<String>,
    pub max_duration: Option<String>,
    pub passthrough_args: Vec<String>,
    pub target_inventory: Option<FuzzTargetInventory>,
    pub execution: Option<FuzzExecutionOutput>,
    pub results: Option<FuzzCampaign>,
    pub campaign_contract: FuzzCampaignContract,
    pub runner_contract: FuzzRunnerContract,
    pub evidence_followups: Vec<String>,
}

#[derive(Serialize)]
pub struct FuzzPlanOutput {
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub target_inventory: FuzzTargetInventory,
    pub request: FuzzExecutionRequest,
    pub runner_contract: FuzzRunnerContract,
}

#[derive(Serialize)]
pub struct FuzzValidateOutput {
    pub command: String,
    pub status: String,
    pub results_file: String,
    pub campaign_id: String,
    pub case_log_files: Vec<String>,
    pub case_log_entries: usize,
    pub open_findings: usize,
    pub artifacts: usize,
    pub coverage_completeness: FuzzCoverageCompletenessOutput,
    pub performance_hotspots: PerformanceHotspotSummary,
    pub gates: Vec<FuzzGateEvaluation>,
}

#[derive(Serialize)]
pub struct FuzzReportOutput {
    pub command: String,
    pub status: String,
    pub results_file: String,
    pub envelope_file: Option<String>,
    pub envelope: FuzzResultEnvelope,
    pub coverage_completeness: FuzzCoverageCompletenessOutput,
    pub performance_hotspots: PerformanceHotspotSummary,
    pub gates: Vec<FuzzGateEvaluation>,
}

#[derive(Serialize)]
pub struct FuzzCompareOutput {
    pub schema: String,
    pub command: String,
    pub status: String,
    pub baseline_file: String,
    pub candidate_file: String,
    pub baseline: FuzzCompareSnapshot,
    pub candidate: FuzzCompareSnapshot,
    pub deltas: FuzzCompareDeltas,
    pub regressions: Vec<String>,
    pub improvements: Vec<String>,
    pub summary: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct FuzzCompareSnapshot {
    pub envelope_id: String,
    pub status: String,
    pub campaign_id: Option<String>,
    pub target_coverage_ratio: f64,
    pub operation_coverage_ratio: f64,
    pub declared_targets: u64,
    pub proven_targets: u64,
    pub declared_operations: u64,
    pub proven_operations: u64,
    pub case_count: usize,
    pub case_status_counts: BTreeMap<String, usize>,
    pub failure_rate: f64,
    pub finding_severity_counts: BTreeMap<String, usize>,
    pub critical_finding_keys: Vec<String>,
    pub missing_required_artifacts: Vec<String>,
    pub gate_status_counts: BTreeMap<String, usize>,
    pub gate_statuses: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct FuzzCompareDeltas {
    pub target_coverage_ratio: f64,
    pub operation_coverage_ratio: f64,
    pub declared_targets: i64,
    pub proven_targets: i64,
    pub declared_operations: i64,
    pub proven_operations: i64,
    pub case_count: i64,
    pub case_status_counts: BTreeMap<String, i64>,
    pub failure_rate: f64,
    pub finding_severity_counts: BTreeMap<String, i64>,
    pub missing_required_artifacts: Vec<String>,
    pub resolved_required_artifacts: Vec<String>,
    pub new_critical_findings: Vec<String>,
    pub resolved_critical_findings: Vec<String>,
    pub gate_status_changes: Vec<FuzzGateStatusChange>,
}

#[derive(Serialize)]
pub struct FuzzGateStatusChange {
    pub gate_id: String,
    pub baseline: Option<String>,
    pub candidate: Option<String>,
}

#[derive(Serialize)]
pub struct FuzzReplayOutput {
    pub command: String,
    pub status: String,
    pub message: String,
    pub artifact_file: Option<String>,
    pub campaign_id: Option<String>,
    pub envelope_id: Option<String>,
    pub case_id: Option<String>,
    pub run_id: Option<String>,
    pub replay: Option<FuzzReplayMetadata>,
    pub env: Vec<FuzzReplayEnv>,
    pub replay_command: Option<String>,
    pub execution: Option<FuzzReplayExecution>,
    pub passthrough_args: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Serialize)]
pub struct FuzzReplayExecution {
    pub kind: String,
    pub extension_id: String,
    pub exit_code: i32,
    pub success: bool,
    pub run_dir: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Serialize)]
pub struct FuzzReplayEnv {
    pub name: String,
    pub value: String,
}

#[derive(Serialize)]
pub struct FuzzExecutionOutput {
    pub kind: String,
    pub extension_id: String,
    pub exit_code: i32,
    pub success: bool,
    pub run_dir: String,
    pub results_file: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FuzzWorkloadOutput {
    pub id: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub source: String,
    pub manifest_path: Option<String>,
}

#[derive(Serialize)]
pub struct FuzzRunnerContract {
    pub capability: String,
    pub extension_script_required: bool,
    pub env: Vec<&'static str>,
}

#[derive(Serialize)]
pub struct FuzzCampaignContract {
    pub case_artifact: Option<String>,
    pub corpus_artifacts: Vec<String>,
    pub seed: Option<String>,
    pub replay_command: Option<String>,
    pub minimize_command: Option<String>,
    pub result_schema: String,
    pub artifact_retention: Option<String>,
    pub unsupported: Vec<&'static str>,
}

#[derive(Serialize, Clone)]
pub struct FuzzGateEvaluation {
    pub gate_id: String,
    pub status: String,
    pub metric: String,
    pub observed: f64,
    pub expected: f64,
}

#[derive(Serialize, Clone)]
pub struct FuzzCoverageCompletenessOutput {
    pub has_summary: bool,
    pub declared_targets: u64,
    pub executable_targets: u64,
    pub proven_targets: u64,
    pub target_coverage_ratio: f64,
    pub declared_operations: u64,
    pub executable_operations: u64,
    pub proven_operations: u64,
    pub operation_coverage_ratio: f64,
    pub skipped_targets: usize,
    pub skipped_operations: usize,
    pub skipped_reason_counts: BTreeMap<String, usize>,
    pub surface_summaries: Vec<FuzzCoverageSelectorSummaryOutput>,
    pub kind_summaries: Vec<FuzzCoverageSelectorSummaryOutput>,
    pub artifact_ids: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct FuzzCoverageSelectorSummaryOutput {
    pub id: String,
    pub kind: String,
    pub label: Option<String>,
    pub declared_targets: u64,
    pub executable_targets: u64,
    pub proven_targets: u64,
    pub target_coverage_ratio: f64,
    pub declared_operations: u64,
    pub executable_operations: u64,
    pub proven_operations: u64,
    pub operation_coverage_ratio: f64,
    pub skipped_targets: usize,
    pub skipped_operations: usize,
    pub skipped_reason_counts: BTreeMap<String, usize>,
}
