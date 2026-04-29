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

/// A single structured test failure surfaced for renderer consumption.
#[derive(Debug, Clone, Serialize)]
pub struct FailedTest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

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
    pub failed_tests: Option<Vec<FailedTest>>,
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
            failed_tests: result.failed_tests,
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
            failed_tests: None,
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
            failed_tests: None,
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
        } else if counts.map(|counts| counts.failed == 0).unwrap_or(false) {
            format!(
                "test runner failed after reporting zero test failures (exit {})",
                exit_code
            )
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
    let category = if exit_code != 0 && counts.map(|counts| counts.failed == 0).unwrap_or(false) {
        PhaseFailureCategory::Infrastructure
    } else {
        phase_failure_category_from_exit_code(exit_code)
    };
    PhaseFailure {
        phase: VerificationPhase::Test,
        summary: match category {
            PhaseFailureCategory::Infrastructure => {
                if counts.map(|counts| counts.failed == 0).unwrap_or(false) {
                    format!(
                        "test runner failed after reporting zero test failures (exit {})",
                        exit_code
                    )
                } else {
                    format!("test harness infrastructure failure (exit {})", exit_code)
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow_result(failed_tests: Option<Vec<FailedTest>>) -> TestRunWorkflowResult {
        TestRunWorkflowResult {
            status: "failed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 1,
            test_counts: Some(TestCounts::new(3, 1, 2, 0)),
            failed_tests,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
        }
    }

    fn workflow_result_with_counts(exit_code: i32, counts: TestCounts) -> TestRunWorkflowResult {
        TestRunWorkflowResult {
            status: if exit_code == 0 { "passed" } else { "failed" }.to_string(),
            component: "homeboy".to_string(),
            exit_code,
            test_counts: Some(counts),
            failed_tests: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
        }
    }

    #[test]
    fn serializes_failed_tests_when_present() {
        let (output, exit_code) = from_main_workflow(workflow_result(Some(vec![FailedTest {
            name: "tests::fails".to_string(),
            detail: Some("assertion failed".to_string()),
            location: Some("tests/fails.rs:42".to_string()),
        }])));

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 1);
        assert_eq!(json["failed_tests"][0]["name"], "tests::fails");
        assert_eq!(json["failed_tests"][0]["detail"], "assertion failed");
        assert_eq!(json["failed_tests"][0]["location"], "tests/fails.rs:42");
    }

    #[test]
    fn omits_failed_tests_when_absent() {
        let (output, _) = from_main_workflow(workflow_result(None));
        let json = serde_json::to_value(output).expect("serialize test command output");
        assert!(
            json.get("failed_tests").is_none(),
            "failed_tests should be omitted when unavailable: {}",
            json
        );
    }

    #[test]
    fn omits_empty_failed_test_optional_fields() {
        let (output, _) = from_main_workflow(workflow_result(Some(vec![FailedTest {
            name: "tests::fails".to_string(),
            detail: None,
            location: None,
        }])));

        let json = serde_json::to_value(output).expect("serialize test command output");
        let failed = &json["failed_tests"][0];
        assert_eq!(failed["name"], "tests::fails");
        assert!(failed.get("detail").is_none());
        assert!(failed.get("location").is_none());
    }

    #[test]
    fn runner_failure_with_zero_parsed_failures_stays_failed() {
        let (output, exit_code) =
            from_main_workflow(workflow_result_with_counts(1, TestCounts::new(3, 3, 0, 0)));

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 1);
        assert_eq!(json["passed"], false);
        assert_eq!(json["status"], "failed");
        assert_eq!(json["exit_code"], 1);
        assert_eq!(
            json["phase"]["summary"],
            "test runner failed after reporting zero test failures (exit 1)"
        );
        assert_eq!(json["failure"]["category"], "infrastructure");
        assert_eq!(
            json["failure"]["summary"],
            "test runner failed after reporting zero test failures (exit 1)"
        );
    }

    #[test]
    fn successful_runner_with_zero_failures_still_passes() {
        let (output, exit_code) =
            from_main_workflow(workflow_result_with_counts(0, TestCounts::new(3, 3, 0, 0)));

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 0);
        assert_eq!(json["passed"], true);
        assert_eq!(json["status"], "passed");
        assert_eq!(json["exit_code"], 0);
        assert!(json.get("failure").is_none());
    }
}
