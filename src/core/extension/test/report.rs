//! Test command output builders — owns the unified test output envelope.
//!
//! All test sub-workflows (main run, drift detection, auto-fix drift)
//! produce domain-specific result types. This module provides the unified output
//! envelope and builder functions that assemble results into command-ready output.

use crate::extension::test::{
    CoverageOutput, DriftReport, TestAnalysis, TestBaselineComparison, TestCounts, TestScopeOutput,
    TestSummaryOutput,
};
use crate::extension::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, PhaseFailure,
    PhaseFailureCategory, PhaseReport, VerificationPhase,
};
use crate::refactor::AppliedRefactor;
use serde::Serialize;

use super::run::{RawTestOutput, TestRunWorkflowResult};
use super::workflow::{AutoFixDriftOutput, AutoFixDriftWorkflowResult, DriftWorkflowResult};

/// Unified output envelope for all test command modes.
///
/// This is the single serialization target for the test command. Each sub-workflow
/// populates its relevant fields; unused fields are `None` and skipped in serialization.
#[derive(Serialize)]
pub struct TestCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<PhaseReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<PhaseFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_counts: Option<TestCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TestBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<TestAnalysis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<AppliedRefactor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<DriftReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_fix_drift: Option<AutoFixDriftOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_scope: Option<TestScopeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<TestSummaryOutput>,
    /// Tail of runner stdout/stderr when tests fail — lets CI wrappers and
    /// users see the actual PHPUnit/cargo output. (#1143)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<RawTestOutput>,
}

/// Build output from a main test workflow result.
pub fn from_main_workflow(result: TestRunWorkflowResult) -> (TestCommandOutput, i32) {
    let exit_code = result.exit_code;
    let phase = Some(test_phase_report(exit_code, result.test_counts.as_ref()));
    let failure = if exit_code == 0 {
        None
    } else {
        Some(test_phase_failure(exit_code, result.test_counts.as_ref()))
    };

    (
        TestCommandOutput {
            passed: exit_code == 0,
            status: result.status,
            component: result.component,
            exit_code: result.exit_code,
            phase,
            failure,
            test_counts: result.test_counts,
            coverage: result.coverage,
            baseline_comparison: result.baseline_comparison,
            analysis: result.analysis,
            autofix: result.autofix,
            hints: result.hints,
            drift: None,
            auto_fix_drift: None,
            test_scope: result.test_scope,
            summary: result.summary,
            raw_output: result.raw_output,
        },
        exit_code,
    )
}

/// Build output from a drift detection workflow result.
pub fn from_drift_workflow(result: DriftWorkflowResult) -> (TestCommandOutput, i32) {
    let exit_code = result.exit_code;
    (
        TestCommandOutput {
            passed: exit_code == 0,
            status: "drift".to_string(),
            component: result.component,
            exit_code: result.exit_code,
            phase: None,
            failure: None,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            drift: Some(result.report),
            auto_fix_drift: None,
            test_scope: None,
            summary: None,
            raw_output: None,
        },
        exit_code,
    )
}

/// Build output from an auto-fix drift workflow result.
pub fn from_auto_fix_drift_workflow(
    result: AutoFixDriftWorkflowResult,
) -> (TestCommandOutput, i32) {
    let status = if result.output.replacements > 0 || !result.hints.is_empty() {
        if result.output.written {
            "fixed"
        } else {
            "planned"
        }
        .to_string()
    } else {
        "passed".to_string()
    };

    (
        TestCommandOutput {
            passed: true,
            status,
            component: result.component,
            exit_code: 0,
            phase: None,
            failure: None,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: Some(result.hints),
            drift: result.report,
            auto_fix_drift: Some(result.output),
            test_scope: None,
            summary: None,
            raw_output: None,
        },
        0,
    )
}

fn test_phase_report(exit_code: i32, counts: Option<&TestCounts>) -> PhaseReport {
    PhaseReport {
        phase: VerificationPhase::Test,
        status: phase_status_from_exit_code(exit_code),
        exit_code: Some(exit_code),
        summary: if exit_code == 0 {
            if let Some(counts) = counts {
                format!(
                    "test phase passed: {} passed, {} skipped",
                    counts.passed, counts.skipped
                )
            } else {
                "test phase passed".to_string()
            }
        } else if exit_code >= 2 {
            format!("test harness infrastructure failure (exit {})", exit_code)
        } else if let Some(counts) = counts {
            format!(
                "test phase reported {} failure(s) out of {} test(s)",
                counts.failed, counts.total
            )
        } else {
            format!(
                "test phase failed without structured counts (exit {})",
                exit_code
            )
        },
    }
}

fn test_phase_failure(exit_code: i32, counts: Option<&TestCounts>) -> PhaseFailure {
    let category = phase_failure_category_from_exit_code(exit_code);
    PhaseFailure {
        phase: VerificationPhase::Test,
        summary: match category {
            PhaseFailureCategory::Infrastructure => {
                format!("test harness infrastructure failure (exit {})", exit_code)
            }
            PhaseFailureCategory::Findings => {
                if let Some(counts) = counts {
                    format!("{} test failure(s) detected", counts.failed)
                } else {
                    format!("test phase reported failures (exit {})", exit_code)
                }
            }
        },
        category,
    }
}
