//! Extension bench capability — run performance workloads inside an
//! extension's runtime and surface regression deltas vs a stored baseline.
//!
//! Bench is a sibling of `lint` / `test` / `build` under `ExtensionCapability`.
//! It shares the runner contract (env-var-driven, JSON-output-file), the
//! manifest shape (`bench: { extension_script: "..." }`), and the baseline
//! ratchet primitive (`engine::baseline`). What makes bench distinct is the
//! **threshold-based regression check on numeric metrics** — see
//! [`baseline::compare`] for the logic and [`baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT`]
//! for the legacy p95 default.
//!
//! Contract with extension scripts:
//! - `$HOMEBOY_BENCH_RESULTS_FILE` — path to write the JSON envelope to.
//! - `$HOMEBOY_BENCH_ITERATIONS` — iterations per scenario.
//! - `$HOMEBOY_BENCH_LIST_ONLY` — when `1`, emit scenario inventory only.
//! - `$HOMEBOY_BENCH_SCENARIOS` — comma-separated exact scenario ids selected by `--scenario`.
//! - `$HOMEBOY_RUN_DIR` — the per-run directory (same as test/lint/build).
//! - Passthrough args after `--` forwarded verbatim to the script.
//!
//! See `docs/commands/bench.md` for the end-user view.

pub mod aggregation;
pub mod artifact;
pub mod baseline;
pub mod distribution;
pub mod metrics;
pub mod parsing;
pub mod report;
pub mod run;
#[cfg(test)]
pub(crate) mod test_support;

use crate::component::Component;
use crate::extension::{ExtensionCapability, ExtensionExecutionContext};

pub use aggregation::aggregate_runs;
pub use artifact::BenchArtifact;
pub use baseline::{
    compare as compare_baseline, load_baseline, save_baseline, BenchBaseline,
    BenchBaselineComparison, BenchBaselineMetadata, BenchScenarioSnapshot, ScenarioDelta,
    DEFAULT_REGRESSION_THRESHOLD_PERCENT,
};
pub use distribution::BenchRunDistribution;
pub use metrics::MetricDelta;
pub use parsing::{
    evaluate_gates, parse_bench_results_file, parse_bench_results_str, BenchGate, BenchGateOp,
    BenchGateResult, BenchMemory, BenchMetrics, BenchResults, BenchScenario,
};
pub use report::{
    aggregate_comparison, from_main_workflow, from_main_workflow_with_rig, BenchCommandOutput,
    BenchComparisonDiff, BenchComparisonOutput, MetricDelta as ReportMetricDelta, RigBenchEntry,
};
pub use run::{
    run_bench_list_workflow, run_main_bench_workflow, BenchListWorkflowArgs,
    BenchListWorkflowResult, BenchRunWorkflowArgs, BenchRunWorkflowResult,
};

pub fn resolve_bench_command(
    component: &Component,
) -> crate::error::Result<ExtensionExecutionContext> {
    crate::extension::resolve_execution_context(component, ExtensionCapability::Bench)
}
