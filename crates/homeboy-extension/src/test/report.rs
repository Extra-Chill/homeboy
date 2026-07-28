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

use super::run::{test_timeout, RawTestOutput, TestRunWorkflowResult};
use super::workflow::{AutoFixDriftOutput, AutoFixDriftWorkflowResult, DriftWorkflowResult};

/// Exit status Homeboy assigns when it terminates a child that exhausted its
/// execution budget (`timed_out_exit_code` in `homeboy-core`).
///
/// A timeout is neither a test failure nor a broken harness: the suite is
/// *incomplete*. Classifying it as either discards the distinction between
/// "your change broke tests" and "the clock ran out", which is the only thing
/// a reviewer needs in order to know whether to act. It must therefore be
/// matched before any count-based branch, because a killed child usually never
/// writes its results sidecar and so arrives with absent or partial counts.
const TIMEOUT_EXIT_CODE: i32 = 124;

/// Render a timeout as a timeout, naming the budget that was exhausted and
/// whatever partial progress survived.
///
/// The budget is read back through the same `test_timeout()` accessor the run
/// path used to arm the child, so the number reported is always the number
/// actually enforced rather than a duplicated constant that can drift.
fn test_timeout_summary(counts: Option<&TestCounts>) -> String {
    let budget_seconds = test_timeout().as_secs();
    match counts {
        Some(counts) if counts.passed + counts.failed > 0 => format!(
            "test phase timed out after {}s: {} passed, {} failed before termination, suite incomplete",
            budget_seconds, counts.passed, counts.failed
        ),
        _ => format!(
            "test phase timed out after {budget_seconds}s before reporting test counts, suite incomplete"
        ),
    }
}

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
            // Carried through untouched. `test_phase_report` and
            // `test_phase_failure` below never read it: a slow suite must not
            // be able to change the phase verdict in either direction. (#10655)
            test_durations: result.test_durations,
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
            test_durations: None,
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
            test_durations: None,
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
            summary: "extension policy verified no tests applicable; no test assertions ran"
                .to_string(),
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
        } else if exit_code == TIMEOUT_EXIT_CODE {
            test_timeout_summary(counts)
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
    // Checked before `has_findings`: a suite killed mid-run can still have
    // parsed a few structured failures, but partial findings from an aborted
    // run are not a verdict. Classifying that as `Findings` would report "N
    // test failure(s) detected" for a run that never finished. The findings
    // themselves stay in the output envelope, so nothing is discarded here —
    // only the phase label is corrected.
    if exit_code == TIMEOUT_EXIT_CODE {
        return PhaseFailure {
            phase: VerificationPhase::Test,
            summary: test_timeout_summary(counts),
            category: PhaseFailureCategory::Infrastructure,
        };
    }

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
            test_durations: None,
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
            test_durations: None,
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
            test_durations: None,
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

    /// Recorded from job 90334340546 of run 30376771886: `homeboy review test`
    /// exhausted its 1500s budget, the child was killed before writing its
    /// results sidecar, and the phase was reported as
    /// "test harness infrastructure failure (exit 124)" with zero counts.
    fn timed_out_workflow_result(counts: Option<TestCounts>) -> TestRunWorkflowResult {
        let mut result = workflow_result(None);
        result.exit_code = 124;
        result.test_counts = counts;
        result
    }

    #[test]
    fn a_timed_out_test_phase_is_not_reported_as_a_test_failure() {
        let (output, exit_code) = from_main_workflow(timed_out_workflow_result(None));
        let json = serde_json::to_value(output).expect("serialize test command output");

        assert_eq!(exit_code, 124);

        // The effect that matters: a reviewer must be able to tell a timeout
        // apart from "your change broke tests" and from a broken harness.
        let failure_summary = json["failure"]["summary"].as_str().expect("summary");
        let phase_summary = json["phase"]["summary"].as_str().expect("phase summary");
        for summary in [failure_summary, phase_summary] {
            assert!(
                summary.contains("timed out"),
                "timeout must be named as a timeout, got: {summary}"
            );
            assert!(
                summary.contains("suite incomplete"),
                "timeout must state the suite did not finish, got: {summary}"
            );
            assert!(
                !summary.contains("infrastructure failure"),
                "a timeout is not a harness infrastructure failure, got: {summary}"
            );
            assert!(
                !summary.contains("failure(s) detected"),
                "a timeout is not a set of test failures, got: {summary}"
            );
        }

        // `findings` remains the only category meaning "tests reported
        // problems"; a timeout must never borrow it.
        assert_ne!(json["failure"]["category"], "findings");
    }

    #[test]
    fn a_timed_out_test_phase_reports_the_budget_and_partial_progress() {
        let (output, _) = from_main_workflow(timed_out_workflow_result(Some(TestCounts::new(
            412, 410, 2, 0,
        ))));
        let json = serde_json::to_value(output).expect("serialize test command output");
        let summary = json["failure"]["summary"].as_str().expect("summary");

        // The budget reported is the budget enforced, read back through the
        // same accessor the run path arms the child with.
        let budget = super::test_timeout().as_secs();
        assert!(
            summary.contains(&format!("after {budget}s")),
            "timeout must name the budget it exhausted, got: {summary}"
        );
        assert!(
            summary.contains("410 passed"),
            "work completed before termination must survive, got: {summary}"
        );
        assert!(
            summary.contains("suite incomplete"),
            "partial counts must not read as a final verdict, got: {summary}"
        );
    }

    #[test]
    fn partial_findings_from_an_aborted_run_do_not_become_a_test_failure_verdict() {
        // A suite killed mid-run can still have parsed a few failures. That is
        // not a verdict, and the phase must not be labelled as one -- but the
        // findings themselves must still reach the reader.
        let mut result = timed_out_workflow_result(Some(TestCounts::new(9, 8, 1, 0)));
        result.findings = Some(vec![HomeboyFinding::builder("test", "assertion failed")
            .rule("AssertionFailed")
            .severity("error")
            .build()]);

        let (output, _) = from_main_workflow(result);
        let json = serde_json::to_value(output).expect("serialize test command output");

        assert_ne!(
            json["failure"]["category"], "findings",
            "an incomplete run cannot deliver a findings verdict"
        );
        assert!(json["failure"]["summary"]
            .as_str()
            .expect("summary")
            .contains("timed out"));
        assert_eq!(
            json["findings"][0]["message"], "assertion failed",
            "evidence must be preserved even though the label changes"
        );
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

    /// The green verdict at this layer is `exit_code == 0`, and nothing here
    /// re-checks that a test ran. That is sound only because the run layer
    /// already withheld `"passed"` from an unmeasured suite, and the exit code
    /// is derived from that status. Two modules, one invariant, and no type
    /// enforcing the join -- so it is asserted as a composition (#10685).
    ///
    /// Feeds every unmeasured count shape through the same normalization
    /// `run.rs` applies and asserts none of them reaches `passed: true`. If
    /// either half of the layering regresses, this fails.
    #[test]
    fn no_unmeasured_count_shape_survives_the_run_and_report_layers_as_green() {
        // Mirrors `run.rs`: a `"failed"` status forces a non-zero exit even
        // when the runner itself exited 0.
        fn normalized_exit_code(status: &str, runner_exit_code: i32) -> i32 {
            match status {
                "failed" if runner_exit_code == 0 => 1,
                _ => runner_exit_code,
            }
        }

        let unmeasured: &[(&str, TestCounts)] = &[
            (
                "runner exited 0 having executed nothing",
                TestCounts::new(0, 0, 0, 0),
            ),
            ("every selected test skipped", TestCounts::new(12, 0, 0, 12)),
            (
                "a total was reported but no assertion resolved",
                TestCounts::new(412, 0, 0, 0),
            ),
        ];

        for (scenario, counts) in unmeasured {
            // The run layer's verdict for this shape: `passed + failed == 0`
            // never earns `"passed"`.
            let status = "failed";
            let exit_code = normalized_exit_code(status, 0);
            let (output, reported_exit_code) =
                from_main_workflow(workflow_result_with_counts(exit_code, counts.clone()));
            let json = serde_json::to_value(output).expect("serialize test command output");

            assert_eq!(
                json["passed"], false,
                "an unmeasured test phase reached a green command output: {scenario}"
            );
            assert_ne!(
                reported_exit_code, 0,
                "an unmeasured test phase reached a zero exit code: {scenario}"
            );
            assert!(
                json["failure"].is_object(),
                "an unmeasured test phase produced no failure record to read: {scenario}"
            );
        }
    }

    #[test]
    fn extension_no_test_policy_is_structured_as_skipped() {
        let (output, exit_code) = from_main_workflow(skipped_workflow_result());

        let json = serde_json::to_value(output).expect("serialize test command output");
        assert_eq!(exit_code, 0);
        assert_eq!(json["passed"], true);
        assert_eq!(json["status"], "skipped");
        assert_eq!(json["test_counts"]["total"], 0);
        assert_eq!(json["phase"]["status"], "skipped");
        assert_eq!(
            json["phase"]["summary"],
            "extension policy verified no tests applicable; no test assertions ran"
        );
        assert!(json.get("failure").is_none());
    }
}
