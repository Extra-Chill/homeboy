use crate::component::Component;
use crate::engine::baseline::BaselineFlags;
use crate::engine::run_dir::{self, RunDir};
use crate::extension::test::analyze::{analyze, TestAnalysis, TestAnalysisInput};
use crate::extension::test::baseline::{self, TestBaselineComparison, TestCounts};
use crate::extension::test::{
    build_test_runner, build_test_summary, compute_changed_test_scope, parse_coverage_file,
    parse_failures_file, parse_test_results_file, parse_test_results_text,
    parse_test_results_text_with_spec, CoverageOutput, FailedTest, TestScopeOutput,
    TestSummaryOutput,
};
use crate::extension::{self, ExtensionCapability};
use crate::refactor::AppliedRefactor;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TestRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub skip_lint: bool,
    pub coverage: bool,
    pub coverage_min: Option<f64>,
    pub analyze: bool,
    pub baseline_flags: BaselineFlags,
    pub changed_since: Option<String>,
    pub json_summary: bool,
    pub passthrough_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub test_counts: Option<TestCounts>,
    pub failed_tests: Option<Vec<FailedTest>>,
    pub coverage: Option<CoverageOutput>,
    pub baseline_comparison: Option<TestBaselineComparison>,
    pub analysis: Option<TestAnalysis>,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub test_scope: Option<TestScopeOutput>,
    pub summary: Option<TestSummaryOutput>,
    /// Tail of the runner's stdout/stderr, surfaced when tests fail so users
    /// can see PHPUnit/cargo output (bootstrap errors, stack traces) without
    /// having to re-run with a different flag. (#1143)
    pub raw_output: Option<RawTestOutput>,
}

/// Captured tail of a test runner's stdout/stderr.
///
/// Surfaced on failure so the actual tool output (PHPUnit, cargo test, etc.)
/// is visible in the structured JSON response. The tail is bounded by
/// `RAW_OUTPUT_TAIL_LINES` to keep JSON payloads small while still showing
/// the last error / stack frame, which is almost always the relevant part
/// for bootstrap failures. (#1143)
#[derive(Debug, Clone, Serialize)]
pub struct RawTestOutput {
    /// Last N lines of stdout. Empty string if the runner emitted no stdout.
    pub stdout_tail: String,
    /// Last N lines of stderr. Empty string if the runner emitted no stderr.
    pub stderr_tail: String,
    /// Whether either tail was truncated from the original output.
    pub truncated: bool,
}

const RAW_OUTPUT_TAIL_LINES: usize = 80;

fn failed_tests_from_analysis_input(input: &TestAnalysisInput) -> Option<Vec<FailedTest>> {
    if input.failures.is_empty() {
        return None;
    }

    Some(
        input
            .failures
            .iter()
            .map(|failure| {
                let detail = if failure.error_type.is_empty() {
                    failure.message.clone()
                } else if failure.message.is_empty() {
                    failure.error_type.clone()
                } else {
                    format!("{}: {}", failure.error_type, failure.message)
                };

                let location = if !failure.source_file.is_empty() {
                    if failure.source_line > 0 {
                        format!("{}:{}", failure.source_file, failure.source_line)
                    } else {
                        failure.source_file.clone()
                    }
                } else {
                    failure.test_file.clone()
                };

                FailedTest {
                    name: failure.test_name.clone(),
                    detail: (!detail.is_empty()).then_some(detail),
                    location: (!location.is_empty()).then_some(location),
                }
            })
            .collect(),
    )
}

fn tail_lines(s: &str, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        (s.to_string(), false)
    } else {
        let start = lines.len() - max_lines;
        (lines[start..].join("\n"), true)
    }
}

pub fn run_main_test_workflow(
    component: &Component,
    source_path: &PathBuf,
    args: TestRunWorkflowArgs,
    run_dir: &RunDir,
) -> crate::Result<TestRunWorkflowResult> {
    let changed_scope = if let Some(ref git_ref) = args.changed_since {
        Some(compute_changed_test_scope(component, git_ref)?)
    } else {
        None
    };

    let coverage_enabled = args.coverage || args.coverage_min.is_some();
    let results_file = run_dir.step_file(run_dir::files::TEST_RESULTS);
    let coverage_file = if coverage_enabled {
        Some(run_dir.step_file(run_dir::files::COVERAGE))
    } else {
        None
    };
    let failures_file = run_dir.step_file(run_dir::files::TEST_FAILURES);

    let changed_test_files = changed_scope
        .as_ref()
        .map(|scope| scope.selected_files.as_slice());

    if let Some(ref scope) = changed_scope {
        if scope.selected_files.is_empty() {
            let hints = Some(vec![
                format!(
                    "No impacted tests found for --changed-since {}",
                    scope.changed_since.as_deref().unwrap_or("unknown")
                ),
                format!(
                    "Run full suite if needed: homeboy test {}",
                    args.component_id
                ),
            ]);

            return Ok(TestRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                test_counts: None,
                failed_tests: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints,
                test_scope: Some(scope.clone()),
                summary: if args.json_summary {
                    Some(build_test_summary(None, None, 0))
                } else {
                    None
                },
                raw_output: None,
            });
        }
    }

    let result_parse = crate::extension::test::resolve_test_command(component)
        .ok()
        .and_then(|context| crate::extension::load_extension(&context.extension_id).ok())
        .and_then(|extension| extension.test.and_then(|test| test.result_parse));

    let output = build_test_runner(
        component,
        args.path_override.clone(),
        &args.settings,
        args.skip_lint,
        coverage_enabled,
        args.coverage_min,
        changed_test_files,
        run_dir,
    )?
    .script_args(&args.passthrough_args)
    .run()?;

    let test_counts = parse_test_results_file(&results_file).or_else(|| {
        result_parse
            .as_ref()
            .and_then(|spec| parse_test_results_text_with_spec(&output.stdout, spec))
            .or_else(|| parse_test_results_text(&output.stdout))
    });

    // Autofix is owned by `refactor --from test --write`; the test command is read-only.
    let test_autofix: Option<AppliedRefactor> = None;

    let status = if let Some(ref counts) = test_counts {
        if counts.failed == 0 {
            "passed"
        } else {
            "failed"
        }
    } else if output.success {
        "passed"
    } else {
        "failed"
    };

    let coverage = coverage_file
        .as_ref()
        .and_then(|file| parse_coverage_file(file).ok());

    let failure_analysis_input = parse_failures_file(&failures_file);
    let failed_tests = failure_analysis_input
        .as_ref()
        .and_then(failed_tests_from_analysis_input);

    let analysis = if args.analyze {
        let analysis_input = failure_analysis_input.unwrap_or_else(|| TestAnalysisInput {
            failures: Vec::new(),
            total: test_counts.as_ref().map(|counts| counts.total).unwrap_or(0),
            passed: test_counts
                .as_ref()
                .map(|counts| counts.passed)
                .unwrap_or(0),
        });

        Some(analyze(&args.component_id, &analysis_input))
    } else {
        None
    };

    if args.baseline_flags.baseline {
        if let Some(ref counts) = test_counts {
            let _ = baseline::save_baseline(source_path, &args.component_id, counts)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(ref counts) = test_counts {
            let resolved_baseline = baseline::load_baseline(source_path).or_else(|| {
                args.changed_since.as_ref().and_then(|git_ref| {
                    baseline::load_baseline_from_ref(&source_path.to_string_lossy(), git_ref)
                })
            });

            if let Some(existing_baseline) = resolved_baseline {
                let comparison = baseline::compare(counts, &existing_baseline);

                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if (comparison.passed_delta > 0 || comparison.failed_delta < 0)
                    && args.baseline_flags.ratchet
                {
                    let _ = baseline::save_baseline(source_path, &args.component_id, counts);
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let mut hints = Vec::new();

    if status == "failed" && args.passthrough_args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy test {} -- --filter=TestName",
            args.component_id
        ));
    }

    if !args.skip_lint {
        hints.push(format!(
            "Auto-fix lint issues: homeboy refactor {} --from lint --write",
            args.component_id
        ));
    }

    if !coverage_enabled {
        hints.push(format!(
            "Collect coverage: homeboy test {} --coverage",
            args.component_id
        ));
    }

    if test_counts.is_some() && !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save test baseline: homeboy test {} --baseline",
            args.component_id
        ));
    }

    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: homeboy test {} --ratchet",
            args.component_id
        ));
    }

    if status == "failed" && !args.analyze {
        hints.push(format!(
            "Analyze failures: homeboy test {} --analyze",
            args.component_id
        ));
    }

    if args.passthrough_args.is_empty() {
        hints.push("Pass args to test runner: homeboy test <component> -- [args]".to_string());
    }

    hints.push("Full options: homeboy docs commands/test".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };
    let test_exit_code = if status == "passed" {
        0
    } else {
        output.exit_code
    };
    let exit_code = baseline_exit_override.unwrap_or(test_exit_code);
    let summary = if args.json_summary {
        Some(build_test_summary(
            test_counts.as_ref(),
            analysis.as_ref(),
            exit_code,
        ))
    } else {
        None
    };

    // When the run failed, surface a tail of the runner's stdout/stderr so the
    // user can see the actual PHPUnit / cargo / etc. output — including
    // bootstrap errors like database connection failures that produce zero
    // parsed test results. Without this, `status: failed, exit_code: 1, 0
    // tests ran` leaves the user guessing. (#1143)
    let raw_output = if status == "failed" {
        let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, RAW_OUTPUT_TAIL_LINES);
        let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, RAW_OUTPUT_TAIL_LINES);
        if stdout_tail.is_empty() && stderr_tail.is_empty() {
            None
        } else {
            Some(RawTestOutput {
                stdout_tail,
                stderr_tail,
                truncated: stdout_truncated || stderr_truncated,
            })
        }
    } else {
        None
    };

    // When tests failed with no parseable counts, surface a dedicated hint so
    // the user understands `raw_output` is the only signal about what went
    // wrong (typically a bootstrap error). (#1143)
    let mut hints_vec = hints.unwrap_or_default();
    if status == "failed" && test_counts.is_none() && raw_output.is_some() {
        hints_vec.insert(
            0,
            "No tests ran — the runner failed before producing results. \
             See raw_output.stderr_tail / raw_output.stdout_tail for the underlying error \
             (bootstrap failure, missing deps, DB connection, etc.)."
                .to_string(),
        );
    }
    let hints = if hints_vec.is_empty() {
        None
    } else {
        Some(hints_vec)
    };

    Ok(TestRunWorkflowResult {
        status: status.to_string(),
        component: args.component_label,
        exit_code,
        test_counts,
        failed_tests,
        coverage,
        baseline_comparison,
        analysis,
        autofix: test_autofix,
        hints,
        test_scope: changed_scope,
        summary,
        raw_output,
    })
}

pub fn run_self_check_test_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> crate::Result<TestRunWorkflowResult> {
    let output =
        extension::self_check::run_self_checks(component, ExtensionCapability::Test, source_path)?;
    let status = if output.success { "passed" } else { "failed" }.to_string();
    let raw_output = (!output.success).then(|| {
        let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, RAW_OUTPUT_TAIL_LINES);
        let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, RAW_OUTPUT_TAIL_LINES);
        RawTestOutput {
            stdout_tail,
            stderr_tail,
            truncated: stdout_truncated || stderr_truncated,
        }
    });

    Ok(TestRunWorkflowResult {
        status,
        component: component_label,
        exit_code: output.exit_code,
        test_counts: None,
        failed_tests: None,
        coverage: None,
        baseline_comparison: None,
        analysis: None,
        autofix: None,
        hints: (!output.success).then(|| {
            vec![format!(
                "Fix the failing self-check command declared in {}'s homeboy.json self_checks.test",
                component.id
            )]
        }),
        test_scope: None,
        summary: if json_summary {
            Some(build_test_summary(None, None, output.exit_code))
        } else {
            None
        },
        raw_output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::SelfCheckConfig;
    use crate::extension::test::TestFailure;

    #[test]
    fn tail_lines_returns_full_text_when_under_limit() {
        let input = "line 1\nline 2\nline 3";
        let (tail, truncated) = tail_lines(input, 10);
        assert_eq!(tail, input);
        assert!(!truncated);
    }

    #[test]
    fn tail_lines_trims_to_last_n_lines() {
        let input: String = (1..=20)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (tail, truncated) = tail_lines(&input, 5);
        assert!(truncated);
        let kept: Vec<&str> = tail.lines().collect();
        assert_eq!(
            kept,
            vec!["line 16", "line 17", "line 18", "line 19", "line 20"]
        );
    }

    #[test]
    fn tail_lines_handles_empty_input() {
        let (tail, truncated) = tail_lines("", 10);
        assert_eq!(tail, "");
        assert!(!truncated);
    }

    #[test]
    fn tail_lines_at_exact_limit_is_not_truncated() {
        let input = "a\nb\nc";
        let (tail, truncated) = tail_lines(input, 3);
        assert_eq!(tail, input);
        assert!(!truncated);
    }

    #[test]
    fn failed_tests_from_analysis_input_preserves_name_detail_and_location() {
        let input = TestAnalysisInput {
            failures: vec![TestFailure {
                test_name: "tests::fails".to_string(),
                test_file: "tests/fails.rs".to_string(),
                error_type: "AssertionFailed".to_string(),
                message: "expected true".to_string(),
                source_file: "src/lib.rs".to_string(),
                source_line: 42,
            }],
            total: 2,
            passed: 1,
        };

        let failed_tests = failed_tests_from_analysis_input(&input).expect("failed tests");
        assert_eq!(failed_tests.len(), 1);
        assert_eq!(failed_tests[0].name, "tests::fails");
        assert_eq!(
            failed_tests[0].detail.as_deref(),
            Some("AssertionFailed: expected true")
        );
        assert_eq!(failed_tests[0].location.as_deref(), Some("src/lib.rs:42"));
    }

    #[test]
    fn test_run_self_check_test_workflow() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("test.sh"), "printf test-ok\n")
            .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.self_checks = Some(SelfCheckConfig {
            lint: Vec::new(),
            test: vec!["sh test.sh".to_string()],
        });

        let result =
            run_self_check_test_workflow(&component, dir.path(), "fixture".to_string(), true)
                .expect("test self-check should run");

        assert_eq!(result.status, "passed");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.component, "fixture");
        assert!(result.summary.is_some());
    }
}
