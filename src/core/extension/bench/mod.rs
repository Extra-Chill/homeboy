//! Extension bench capability — run performance workloads inside an
//! extension's runtime and surface regression deltas vs a stored baseline.
//!
//! Bench is a sibling of `lint` / `test` / `build` under `ExtensionCapability`.
//! It shares the runner contract (env-var-driven, JSON-output-file), the
//! manifest shape (`bench: { extension_script: "..." }`), and the baseline
//! ratchet primitive (`engine::baseline`). What makes bench distinct is the
//! **threshold-based regression check on p95 latency** — see
//! [`baseline::compare`] for the logic and [`baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT`]
//! for the default.
//!
//! Contract with extension scripts:
//! - `$HOMEBOY_BENCH_RESULTS_FILE` — path to write the JSON envelope to.
//! - `$HOMEBOY_BENCH_ITERATIONS` — iterations per scenario.
//! - `$HOMEBOY_RUN_DIR` — the per-run directory (same as test/lint/build).
//! - Passthrough args after `--` forwarded verbatim to the script.
//!
//! See `docs/commands/bench.md` for the end-user view.

pub mod baseline;
pub mod parsing;
pub mod report;
pub mod run;

use crate::component::Component;
use crate::extension::{ExtensionCapability, ExtensionExecutionContext};

pub use baseline::{
    compare as compare_baseline, load_baseline, save_baseline, BenchBaseline,
    BenchBaselineComparison, BenchBaselineMetadata, BenchScenarioSnapshot, ScenarioDelta,
    DEFAULT_REGRESSION_THRESHOLD_PERCENT,
};
pub use parsing::{
    parse_bench_results_file, parse_bench_results_str, BenchMemory, BenchMetrics, BenchResults,
    BenchScenario,
};
pub use report::{from_main_workflow, from_main_workflow_with_rig, BenchCommandOutput};
pub use run::{run_main_bench_workflow, BenchRunWorkflowArgs, BenchRunWorkflowResult};

pub fn resolve_bench_command(
    component: &Component,
) -> crate::error::Result<ExtensionExecutionContext> {
    crate::extension::resolve_execution_context(component, ExtensionCapability::Bench)
}
