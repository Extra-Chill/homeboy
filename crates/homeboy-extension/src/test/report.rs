//! Test command output builders — owns the unified test output envelope.
//!
//! All test sub-workflows (main run, drift detection, auto-fix drift)
//! produce domain-specific result types. This module provides the unified output
//! envelope and builder functions that assemble results into command-ready output.

use crate::test::{
    CoverageOutput, DriftReport, TestAnalysis, TestBaselineComparison, TestCounts, TestScopeOutput,
    TestSummaryOutput,
};
use crate::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, PhaseFailure,
    PhaseFailureCategory, PhaseReport, PhaseStatus, VerificationPhase,
};
use homeboy_core::ci_profile::CiContext;
use homeboy_core::finding::HomeboyFinding;
pub use homeboy_extension_contract::test_results::TestCommandOutput;
use homeboy_refactor_contract::AppliedRefactor;
use serde::Serialize;
use serde_json::Value;

use super::run::{RawTestOutput, TestRunWorkflowResult};
use super::workflow::{AutoFixDriftOutput, AutoFixDriftWorkflowResult, DriftWorkflowResult};

/// Build output from a main test workflow result.
pub fn from_main_workflow(result: TestRunWorkflowResult) -> (TestCommandOutput, i32) {
    from_main_workflow_with_ci_context(result, None)
}

pub fn from_main_workflow_with_ci_context(
    result: TestRunWorkflowResult,
    ci_context: Option<CiContext>,
) -> (TestCommandOutput, i32) {
    let exit_code = result.exit_code;
    let phase = Some(test_phase_report(
        &result.status,
        exit_code,
        result.test_counts.as_ref(),
        result
            .findings
            .as_ref()
            .is_some_and(|findings| !findings.is_empty()),
    ));
    let failure = if exit_code == 0 {
        None
    } else {
        Some(test_phase_failure(
            exit_code,
            result.test_counts.as_ref(),
            result
                .findings
                .as_ref()
                .is_some_and(|findings| !findings.is_empty()),
        ))
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
            findings: result.findings,
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
            ci_context,
            extension_phase_timings: result.extension_phase_timings,
            actionable: None,
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
            findings: None,
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
            ci_context: None,
            extension_phase_timings: Vec::new(),
            actionable: None,
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
            findings: None,
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
            ci_context: None,
            extension_phase_timings: Vec::new(),
            actionable: None,
        },
        0,
    )
}

fn test_phase_report(
    status: &str,
    exit_code: i32,
    counts: Option<&TestCounts>,
    has_findings: bool,
) -> PhaseReport {
    if status == "skipped" {
        return PhaseReport {
            phase: VerificationPhase::Test,
            status: PhaseStatus::Skipped,
            exit_code: Some(exit_code),
            summary: "activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran".to_string(),
        };
    }

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
        } else if counts.map(|counts| counts.total == 0).unwrap_or(false) {
            "test runner reported zero executed tests".to_string()
        } else if has_findings {
            format!("test phase reported structured failures (exit {exit_code})")
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

fn test_phase_failure(
    exit_code: i32,
    counts: Option<&TestCounts>,
    has_findings: bool,
) -> PhaseFailure {
    let category = if has_findings {
        PhaseFailureCategory::Findings
    } else if exit_code != 0 && counts.map(|counts| counts.total == 0).unwrap_or(false) {
        PhaseFailureCategory::Findings
    } else if exit_code != 0 && counts.map(|counts| counts.failed == 0).unwrap_or(false) {
        PhaseFailureCategory::Infrastructure
    } else {
        phase_failure_category_from_exit_code(exit_code)
    };
    PhaseFailure {
        phase: VerificationPhase::Test,
        summary: match category {
            PhaseFailureCategory::Infrastructure => {
                if counts.map(|counts| counts.total == 0).unwrap_or(false) {
                    "test runner reported zero executed tests".to_string()
                } else if counts.map(|counts| counts.failed == 0).unwrap_or(false) {
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
                    if counts.total == 0 {
                        "test runner reported zero executed tests".to_string()
                    } else {
                        format!("{} test failure(s) detected", counts.failed)
                    }
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

    fn workflow_result(findings: Option<Vec<HomeboyFinding>>) -> TestRunWorkflowResult {
        TestRunWorkflowResult {
            status: "failed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 1,
            test_counts: Some(TestCounts::new(3, 1, 2, 0)),
            findings,
            failure_analysis_input: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
            extension_phase_timings: Vec::new(),
        }
    }

    fn workflow_result_with_counts(exit_code: i32, counts: TestCounts) -> TestRunWorkflowResult {
        TestRunWorkflowResult {
            status: if exit_code == 0 { "passed" } else { "failed" }.to_string(),
            component: "homeboy".to_string(),
            exit_code,
            test_counts: Some(counts),
            findings: None,
            failure_analysis_input: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
            extension_phase_timings: Vec::new(),
        }
    }

    fn skipped_workflow_result() -> TestRunWorkflowResult {
        TestRunWorkflowResult {
            status: "skipped".to_string(),
            component: "wordpress-plugin".to_string(),
            exit_code: 0,
            test_counts: Some(TestCounts::new(0, 0, 0, 0)),
            findings: None,
            failure_analysis_input: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
            extension_phase_timings: Vec::new(),
        }
    }

    #[test]
    fn serializes_findings_when_present() {
        let (output, exit_code) =
            from_main_workflow(workflow_result(Some(vec![HomeboyFinding::builder(
                "test",
                "assertion failed",
            )
            .rule("AssertionFailed")
            .severity("error")
            .file("tests/fails.rs")
            .line(42)
            .metadata("test_name", "tests::fails")
            .build()])));

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 1);
        assert_eq!(json["findings"][0]["tool"], "test");
        assert_eq!(json["findings"][0]["metadata"]["test_name"], "tests::fails");
        assert_eq!(json["findings"][0]["message"], "assertion failed");
        assert_eq!(json["findings"][0]["file"], "tests/fails.rs");
        assert_eq!(json["findings"][0]["line"], 42);
        assert_eq!(json["failure"]["category"], "findings");
    }

    #[test]
    fn omits_findings_when_absent() {
        let (output, _) = from_main_workflow(workflow_result(None));
        let json = serde_json::to_value(output).expect("serialize test command output");
        assert!(
            json.get("findings").is_none(),
            "findings should be omitted when unavailable: {}",
            json
        );
    }

    #[test]
    fn serializes_extension_phase_timings_as_opaque_metadata() {
        let mut result = workflow_result(None);
        result.extension_phase_timings = vec![crate::ExtensionPhaseTiming {
            name: "opaque-provider-phase".to_string(),
            duration_ms: 4321,
            status: Some("waiting".to_string()),
            message: Some("provider is waiting for a shared resource".to_string()),
            artifacts: vec![serde_json::json!({ "url": "runner-artifact://phase.json" })],
            metadata: std::collections::BTreeMap::new(),
        }];

        let (output, _) = from_main_workflow(result);
        let json = serde_json::to_value(output).expect("serialize test command output");

        assert_eq!(
            json["extension_phase_timings"][0]["name"],
            "opaque-provider-phase"
        );
        assert_eq!(json["extension_phase_timings"][0]["duration_ms"], 4321);
        assert_eq!(json["extension_phase_timings"][0]["status"], "waiting");
        assert_eq!(
            json["extension_phase_timings"][0]["message"],
            "provider is waiting for a shared resource"
        );
        assert_eq!(
            json["extension_phase_timings"][0]["artifacts"][0]["url"],
            "runner-artifact://phase.json"
        );
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

    #[test]
    fn zero_executed_tests_use_runner_neutral_failure_summary() {
        let (output, exit_code) =
            from_main_workflow(workflow_result_with_counts(1, TestCounts::new(0, 0, 0, 0)));

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 1);
        assert_eq!(
            json["phase"]["summary"],
            "test runner reported zero executed tests"
        );
        assert_eq!(
            json["failure"]["summary"],
            "test runner reported zero executed tests"
        );
    }

    #[test]
    fn phpunit_no_discovery_is_structured_as_skipped() {
        let (output, exit_code) = from_main_workflow(skipped_workflow_result());

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 0);
        assert_eq!(json["passed"], true);
        assert_eq!(json["status"], "skipped");
        assert_eq!(json["test_counts"]["total"], 0);
        assert_eq!(json["phase"]["status"], "skipped");
        assert_eq!(
            json["phase"]["summary"],
            "activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran"
        );
        assert!(json.get("failure").is_none());
    }
}
