//! Bench command output — unified envelope for the `homeboy bench` command.

use serde::Serialize;

use super::baseline::BenchBaselineComparison;
use super::parsing::BenchResults;
use super::run::BenchRunWorkflowResult;

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
}

pub fn from_main_workflow(result: BenchRunWorkflowResult) -> (BenchCommandOutput, i32) {
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
        },
        exit_code,
    )
}
