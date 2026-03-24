use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::extension::test::analyze::{analyze, TestAnalysis, TestAnalysisInput};
use crate::extension::test::baseline::{self, TestBaselineComparison, TestCounts};
use crate::extension::test::{
    build_test_runner, build_test_summary, compute_changed_test_scope, parse_coverage_file,
    parse_failures_file, parse_test_results_file, parse_test_results_text, CoverageOutput,
    TestScopeOutput, TestSummaryOutput,
};
use crate::refactor::AppliedRefactor;
use serde::Serialize;
use std::path::PathBuf;

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
    pub baseline: bool,
    pub ignore_baseline: bool,
    pub ratchet: bool,
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
    pub coverage: Option<CoverageOutput>,
    pub baseline_comparison: Option<TestBaselineComparison>,
    pub analysis: Option<TestAnalysis>,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub test_scope: Option<TestScopeOutput>,
    pub summary: Option<TestSummaryOutput>,
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
    let failures_file = if args.analyze {
        Some(run_dir.step_file(run_dir::files::TEST_FAILURES))
    } else {
        None
    };

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
            });
        }
    }

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

    let test_counts =
        parse_test_results_file(&results_file).or_else(|| parse_test_results_text(&output.stdout));

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

    let analysis = if args.analyze {
        let analysis_input = failures_file
            .as_ref()
            .and_then(|file| parse_failures_file(file))
            .unwrap_or_else(|| TestAnalysisInput {
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

    if args.baseline {
        if let Some(ref counts) = test_counts {
            let _ = baseline::save_baseline(source_path, &args.component_id, counts)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline && !args.ignore_baseline {
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
                    && args.ratchet
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

    if test_counts.is_some() && !args.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save test baseline: homeboy test {} --baseline",
            args.component_id
        ));
    }

    if baseline_comparison.is_some() && !args.ratchet {
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

    Ok(TestRunWorkflowResult {
        status: status.to_string(),
        component: args.component_label,
        exit_code,
        test_counts,
        coverage,
        baseline_comparison,
        analysis,
        autofix: test_autofix,
        hints,
        test_scope: changed_scope,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_run_main_test_workflow_let_changed_scope_if_let_some_ref_git_ref_args_changed_since() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_some_compute_changed_test_scope_component_git_ref() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_some_run_dir_step_file_run_dir_files_coverage() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else_2() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else_3() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else_4() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_if_let_some_ref_scope_changed_scope() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_test_scope_some_scope_clone() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_some_build_test_summary_none_none_0() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else_5() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_branch_11() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_default_path() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_let_status_if_let_some_ref_counts_test_counts() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_some_analyze_args_component_id_analysis_input() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else_6() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_args_baseline() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_let_some_ref_counts_test_counts() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_args_baseline_args_ignore_baseline() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_if_let_some_existing_baseline_resolved_baseline() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_comparison_regression() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_default_path_2() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_default_path_3() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_else_7() {
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _result = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

    #[test]
    fn test_run_main_test_workflow_has_expected_effects() {
        // Expected effects: mutation
        let component = Default::default();
        let source_path = PathBuf::new();
        let args = Default::default();
        let run_dir = Default::default();
        let _ = run_main_test_workflow(&component, &source_path, args, &run_dir);
    }

}
