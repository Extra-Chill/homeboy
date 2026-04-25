//! Bench command output — unified envelope for the `homeboy bench` command.

use serde::Serialize;

use super::baseline::BenchBaselineComparison;
use super::parsing::BenchResults;
use super::run::BenchRunWorkflowResult;
use crate::rig::RigStateSnapshot;

#[derive(Serialize)]
pub struct BenchCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<BenchResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<BenchBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    /// Rig state captured at the start of the run when bench was invoked
    /// with `--rig <id>`. Skipped when bench ran without a rig so the
    /// existing output shape is unchanged for the bare `homeboy bench`
    /// path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_state: Option<RigStateSnapshot>,
}

pub fn from_main_workflow(result: BenchRunWorkflowResult) -> (BenchCommandOutput, i32) {
    from_main_workflow_with_rig(result, None)
}

/// Same as `from_main_workflow` but also embeds an optional rig-state
/// snapshot — populated by `homeboy bench --rig <id>` so consumers can
/// see exactly which component commits the numbers were measured
/// against.
pub fn from_main_workflow_with_rig(
    result: BenchRunWorkflowResult,
    rig_state: Option<RigStateSnapshot>,
) -> (BenchCommandOutput, i32) {
    let exit_code = result.exit_code;
    (
        BenchCommandOutput {
            passed: exit_code == 0,
            status: result.status,
            component: result.component,
            exit_code,
            iterations: result.iterations,
            results: result.results,
            baseline_comparison: result.baseline_comparison,
            hints: result.hints,
            rig_state,
        },
        exit_code,
    )
}
