//! Lint command output builders — owns the unified lint output envelope.
//!
//! Mirrors `core/extension/test/report.rs` — the command layer calls a single
//! builder function to convert a workflow result into the command output tuple.

use crate::extension::lint::baseline::{BaselineComparison, LintFinding};
use crate::extension::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, PhaseFailure,
    PhaseFailureCategory, PhaseReport, VerificationPhase,
};
use crate::refactor::AppliedRefactor;
use serde::Serialize;

use super::run::LintRunWorkflowResult;

/// Unified output envelope for the lint command.
///
/// This is the single serialization target. The workflow populates relevant
/// fields; unused fields are `None` and skipped in serialization.
#[derive(Serialize)]
pub struct LintCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub phase: PhaseReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<PhaseFailure>,
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
    let finding_count = result
        .lint_findings
        .as_ref()
        .map(|findings| findings.len())
        .unwrap_or(0);
    let phase = lint_phase_report(exit_code, &result.status, finding_count);
    let failure = if exit_code == 0 {
        None
    } else {
        Some(lint_phase_failure(exit_code, finding_count))
    };

    (
        LintCommandOutput {
            passed: exit_code == 0,
            status: result.status,
            component: result.component,
            exit_code,
            phase,
            failure,
            autofix: result.autofix,
            hints: result.hints,
            baseline_comparison: result.baseline_comparison,
            lint_findings: result.lint_findings,
        },
        exit_code,
    )
}

fn lint_phase_report(exit_code: i32, status: &str, finding_count: usize) -> PhaseReport {
    PhaseReport {
        phase: VerificationPhase::Lint,
        status: phase_status_from_exit_code(exit_code),
        exit_code: Some(exit_code),
        summary: if exit_code == 0 {
            "lint phase passed with no findings".to_string()
        } else if exit_code >= 2 {
            format!("lint phase infrastructure failure (exit {})", exit_code)
        } else if finding_count > 0 {
            format!("lint phase reported {} finding(s)", finding_count)
        } else {
            format!("lint phase {} (exit {})", status, exit_code)
        },
    }
}

fn lint_phase_failure(exit_code: i32, finding_count: usize) -> PhaseFailure {
    let category = phase_failure_category_from_exit_code(exit_code);
    PhaseFailure {
        phase: VerificationPhase::Lint,
        summary: match category {
            PhaseFailureCategory::Infrastructure => {
                format!("lint runner infrastructure failure (exit {})", exit_code)
            }
            PhaseFailureCategory::Findings => {
                if finding_count > 0 {
                    format!("{} lint finding(s) detected", finding_count)
                } else {
                    format!("lint phase reported findings (exit {})", exit_code)
                }
            }
        },
        category,
    }
}
