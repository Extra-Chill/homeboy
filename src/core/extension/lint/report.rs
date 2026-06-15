//! Lint command output builders — owns the unified lint output envelope.
//!
//! Mirrors `core/extension/test/report.rs` — the command layer calls a single
//! builder function to convert a workflow result into the command output tuple.

use crate::core::ci_profile::CiContext;
use crate::core::extension::lint::baseline::BaselineComparison;
use crate::core::extension::self_check::SelfCheckCaptureMetadata;
use crate::core::extension::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, PhaseFailure,
    PhaseFailureCategory, PhaseReport, VerificationPhase,
};
use crate::core::finding::{FindingProducerSummary, HomeboyFinding};
use crate::core::refactor::plan::RefactorSourceRun;
use crate::core::refactor::AppliedRefactor;
use serde::Serialize;

use super::run::{LintRunWorkflowResult, LintSummaryOutput};

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
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<LintSummaryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_check_capture: Option<SelfCheckCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_context: Option<CiContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<crate::core::extension::ExtensionPhaseTiming>,
}

/// Build output from a main lint workflow result.
pub fn from_main_workflow(result: LintRunWorkflowResult) -> (LintCommandOutput, i32) {
    from_main_workflow_with_ci_context(result, None)
}

pub fn from_main_workflow_with_ci_context(
    result: LintRunWorkflowResult,
    ci_context: Option<CiContext>,
) -> (LintCommandOutput, i32) {
    // Exit code should reflect the computed status, not just the extension's
    // shell exit code. When findings exist but the extension exited 0, the
    // process must still exit non-zero so CI treats it as a failure (#696).
    let exit_code = if result.status == "failed" && result.exit_code == 0 {
        1
    } else {
        result.exit_code
    };
    let finding_count = result
        .findings
        .as_ref()
        .map(|findings| findings.len())
        .unwrap_or(0);
    let json_summary = result.summary.is_some();
    let phase = lint_phase_report(
        exit_code,
        &result.status,
        finding_count,
        &result.producer_summaries,
    );
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
            findings: if json_summary { None } else { result.findings },
            producer_summaries: result.producer_summaries,
            summary: result.summary,
            self_check_capture: result.self_check_capture,
            ci_context,
            extension_phase_timings: result.extension_phase_timings,
        },
        exit_code,
    )
}

fn lint_phase_report(
    exit_code: i32,
    status: &str,
    finding_count: usize,
    producer_summaries: &[FindingProducerSummary],
) -> PhaseReport {
    PhaseReport {
        phase: VerificationPhase::Lint,
        status: phase_status_from_exit_code(exit_code),
        exit_code: Some(exit_code),
        summary: if exit_code == 0 {
            "lint phase passed with no findings".to_string()
        } else if exit_code >= 2 {
            format!("lint phase infrastructure failure (exit {})", exit_code)
        } else if !producer_summaries.is_empty() {
            format!(
                "lint phase failed with {} finding(s) across {}",
                finding_count,
                producer_summary_label(producer_summaries)
            )
        } else if finding_count > 0 {
            format!("lint phase reported {} finding(s)", finding_count)
        } else {
            format!("lint phase {} (exit {})", status, exit_code)
        },
    }
}

fn producer_summary_label(producer_summaries: &[FindingProducerSummary]) -> String {
    producer_summaries
        .iter()
        .map(|producer| {
            format!(
                "{} {}: {}",
                producer.tool, producer.status, producer.finding_count
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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
            findings: None,
            producer_summaries: Vec::new(),
            summary: None,
            self_check_capture: None,
            ci_context: None,
            extension_phase_timings: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_phase_summary_aggregates_multiple_producers() {
        let result = LintRunWorkflowResult {
            status: "failed".to_string(),
            component: "fixture".to_string(),
            exit_code: 1,
            autofix: None,
            hints: None,
            baseline_comparison: None,
            findings: Some(vec![
                HomeboyFinding::builder("eslint", "eslint error").build(),
                HomeboyFinding::builder("phpstan", "phpstan error").build(),
            ]),
            producer_summaries: vec![
                FindingProducerSummary::new("phpcs", "passed").finding_count(0),
                FindingProducerSummary::new("eslint", "failed").finding_count(1),
                FindingProducerSummary::new("phpstan", "failed").finding_count(1),
            ],
            summary: None,
            self_check_capture: None,
            extension_phase_timings: Vec::new(),
        };

        let (output, exit_code) = from_main_workflow(result);

        assert_eq!(exit_code, 1);
        assert_eq!(
            output.phase.summary,
            "lint phase failed with 2 finding(s) across phpcs passed: 0, eslint failed: 1, phpstan failed: 1"
        );
    }
}
