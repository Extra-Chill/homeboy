//! Bench run/list workflow argument and result types.

use std::path::PathBuf;

use serde::Serialize;

use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::invocation::InvocationRequirements;
use crate::core::extension::bench::baseline::BenchBaselineComparison;
use crate::core::extension::bench::diagnostic::BenchDiagnostic;
use crate::core::extension::bench::parsing::RigPackageEvidence;
use crate::core::extension::bench::parsing::{BenchResults, BenchRunExecution, BenchScenario};
use crate::core::extension::bench::phase_events::BenchPhaseFailureClassification;
use crate::core::extension::bench::responsiveness::{
    BenchFailureMemorySample, BenchResponsivenessSummary,
};
use crate::core::gate::HomeboyGateResult;

#[derive(Debug, Clone)]
pub struct BenchRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    /// Typed-JSON setting overrides from `--setting-json key=<json>`.
    /// Applied after `settings` (string overrides) so JSON wins on
    /// conflict. Required for object-shaped settings like
    /// `wp_config_defines` / `bench_env` whose dispatchers expect a JSON
    /// object, not a JSON-string-of-an-object.
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub iterations: u64,
    pub warmup_iterations: Option<u64>,
    /// Caller-supplied stable proof label from `--run-id`. Forwarded to
    /// component bench scripts via `$HOMEBOY_BENCH_RUN_ID` so a run can be
    /// correlated across CI logs, dashboards, and proof archives. `None`
    /// leaves the env var unset, preserving prior behaviour exactly.
    pub run_id: Option<String>,
    pub execution: BenchRunExecution,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub json_summary: bool,
    pub ci_env: Vec<(String, String)>,
    pub passthrough_args: Vec<String>,
    /// Exact scenario ids selected by the CLI. Empty means run every
    /// discovered scenario.
    pub scenario_ids: Vec<String>,
    /// Optional rig identifier when bench was invoked via `--rig <id>`.
    /// Threads through to the baseline storage key so rig-pinned and
    /// unpinned baselines stay in separate slots inside `homeboy.json`.
    /// `None` preserves the original baseline shape exactly.
    pub rig_id: Option<String>,
    /// Optional shared-state directory mounted across iterations and
    /// instances. When set, the dispatcher exposes the path to workloads
    /// via `$HOMEBOY_BENCH_SHARED_STATE` so they can persist on-disk
    /// state (SQLite files, content directories, counter files) that
    /// outlives a single iteration. Required when `concurrency > 1`.
    pub shared_state: Option<PathBuf>,
    /// Number of parallel runner instances to spawn. `1` (default)
    /// preserves single-instance behaviour. `> 1` requires `shared_state`
    /// to be set — N independent cold-boots without shared state would
    /// be N independent runs, not a multi-instance contention test.
    /// Rig-declared out-of-tree workloads to run alongside in-tree discovery.
    /// Exported to dispatchers as `HOMEBOY_BENCH_EXTRA_WORKLOADS`.
    pub extra_workloads: Vec<PathBuf>,
    /// Additional installed extensions whose generic env providers should be
    /// merged into the bench runner env before caller overrides.
    pub env_provider_extensions: Vec<String>,
    pub rig_package: Option<RigPackageEvidence>,
    /// Generic Homeboy isolation requirements for each child workload
    /// invocation. Rigs can use this for browser/server/wasm benchmarks without
    /// runner-specific namespace logic.
    pub invocation_requirements: InvocationRequirements,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    pub results: Option<BenchResults>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<HomeboyGateResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gate_failures: Vec<String>,
    pub baseline_comparison: Option<BenchBaselineComparison>,
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<BenchRunFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchRunFailure {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub exit_code: i32,
    pub stderr_tail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<BenchPhaseFailureClassification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responsiveness: Option<BenchResponsivenessSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_sample: Option<BenchFailureMemorySample>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

pub(crate) type BenchRunExecutionOutcome = (
    Option<BenchResults>,
    bool,
    i32,
    Option<String>,
    Option<BenchFailureMemorySample>,
    Option<u128>,
);

#[derive(Debug, Clone)]
pub struct BenchListWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub passthrough_args: Vec<String>,
    pub scenario_ids: Vec<String>,
    pub extra_workloads: Vec<PathBuf>,
    pub env_provider_extensions: Vec<String>,
    pub rig_package: Option<RigPackageEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchListWorkflowResult {
    pub component: String,
    pub component_id: String,
    pub scenarios: Vec<BenchScenario>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_package: Option<RigPackageEvidence>,
}
