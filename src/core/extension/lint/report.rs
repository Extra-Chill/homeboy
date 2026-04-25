//! Lint command output builders — owns the unified lint output envelope.
//!
//! Mirrors `core/extension/test/report.rs` — the command layer calls a single
//! builder function to convert a workflow result into the command output tuple.

use crate::extension::lint::baseline::{BaselineComparison, LintFinding};
use crate::extension::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, PhaseFailure,
    PhaseFailureCategory, PhaseReport, VerificationPhase,
};
use crate::refactor::plan::RefactorSourceRun;
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

/// Build a [`LintCommandOutput`] from a `homeboy lint --fix` dispatch.
///
/// `--fix` is a thin alias for `homeboy refactor <component> --from lint
/// --write`, so the fix path receives a `RefactorSourceRun` rather than the
/// usual lint workflow result. We surface the applied autofix via the existing
/// `autofix` field on `LintCommandOutput` so consumers see a consistent shape
/// regardless of which mode was requested.
///
/// Exit code semantics: autofixable findings should never fail the run, so
/// the fix dispatch returns exit 0 even when fixes were applied. Real fixer
/// errors propagate through `Result` and never reach this builder.
pub fn from_lint_fix(component_label: String, run: RefactorSourceRun) -> (LintCommandOutput, i32) {
    let exit_code = 0;
    let phase = PhaseReport {
        phase: VerificationPhase::Lint,
        status: phase_status_from_exit_code(exit_code),
        exit_code: Some(exit_code),
        summary: if run.applied {
            format!(
                "lint phase auto-fix applied to {} file(s)",
                run.files_modified
            )
        } else if run.files_modified > 0 {
            "lint phase auto-fix dry run".to_string()
        } else {
            "lint phase auto-fix found no autofixable findings".to_string()
        },
    };

    let mut hints = run.hints.clone();
    if run.applied {
        hints.push(format!(
            "Re-run lint to confirm clean: homeboy lint {}",
            component_label
        ));
    } else if run.files_modified == 0 && run.warnings.is_empty() {
        hints.push("No autofixable findings detected.".to_string());
    }
    let hints = if hints.is_empty() { None } else { Some(hints) };

    let autofix = AppliedRefactor {
        files_modified: run.files_modified,
        rerun_recommended: run.applied,
        changed_files: run.changed_files.clone(),
        fix_summary: run.fix_summary.clone(),
    };

    (
        LintCommandOutput {
            passed: true,
            status: "passed".to_string(),
            component: component_label,
            exit_code,
            phase,
            failure: None,
            autofix: Some(autofix),
            hints,
            baseline_comparison: None,
            lint_findings: None,
        },
        exit_code,
    )
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
