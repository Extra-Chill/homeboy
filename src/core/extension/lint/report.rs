//! Lint command output builders — owns the unified lint output envelope.
//!
//! Mirrors `core/extension/test/report.rs` — the command layer calls a single
//! builder function to convert a workflow result into the command output tuple.

use crate::extension::lint::baseline::{BaselineComparison, LintFinding};
use crate::refactor::AppliedRefactor;
use serde::Serialize;

use super::run::LintRunWorkflowResult;

/// Unified output envelope for the lint command.
///
/// This is the single serialization target. The workflow populates relevant
/// fields; unused fields are `None` and skipped in serialization.
#[derive(Serialize)]
pub struct LintCommandOutput {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<AppliedRefactor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<BaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lint_findings: Option<Vec<LintFinding>>,
}

/// Build output from a main lint workflow result.
pub fn from_main_workflow(result: LintRunWorkflowResult) -> (LintCommandOutput, i32) {
    // Exit code should reflect the computed status, not just the extension's
    // shell exit code. When findings exist but the extension exited 0, the
    // process must still exit non-zero so CI treats it as a failure (#696).
    let exit_code = if result.status == "failed" && result.exit_code == 0 {
        1
    } else {
        result.exit_code
    };
    (
        LintCommandOutput {
            status: result.status,
            component: result.component,
            exit_code,
            autofix: result.autofix,
            hints: result.hints,
            baseline_comparison: result.baseline_comparison,
            lint_findings: result.lint_findings,
        },
        exit_code,
    )
}
