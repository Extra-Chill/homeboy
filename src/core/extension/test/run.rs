use crate::component::Component;
use crate::extension::test::{
    build_test_runner, build_test_summary, compute_changed_test_scope, parse_coverage_file,
    parse_failures_file, parse_test_results_file, parse_test_results_text, CoverageOutput,
    TestScopeOutput, TestSummaryOutput,
};
use crate::refactor::{
    auto::{self, AutofixMode},
    run_test_refactor, AppliedRefactor, TestSourceOptions,
};
use crate::test_analyze::{self, TestAnalysis, TestAnalysisInput};
use crate::test_baseline::{self, TestBaselineComparison, TestCounts};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct TestRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub skip_lint: bool,
    pub fix: bool,
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
) -> crate::Result<TestRunWorkflowResult> {
    let changed_scope = if let Some(ref git_ref) = args.changed_since {
        Some(compute_changed_test_scope(component, git_ref)?)
    } else {
        None
    };

    let coverage_enabled = args.coverage || args.coverage_min.is_some();
    let coverage_file = if coverage_enabled {
        Some(std::env::temp_dir().join(format!("homeboy-coverage-{}.json", std::process::id())))
    } else {
        None
    };
    let results_file =
        std::env::temp_dir().join(format!("homeboy-test-results-{}.json", std::process::id()));
    let failures_file = if args.analyze {
        Some(
            std::env::temp_dir().join(format!("homeboy-test-failures-{}.json", std::process::id())),
        )
    } else {
        None
    };

    let planned_autofix = if args.fix {
        let selected_files = changed_scope
            .as_ref()
            .map(|scope| scope.selected_files.clone());
        let plan = run_test_refactor(
            component.clone(),
            source_path.clone(),
            args.settings.clone(),
            TestSourceOptions {
                selected_files,
                skip_lint: args.skip_lint,
                script_args: args.passthrough_args.clone(),
            },
            true,
        )?;

        let outcome = auto::standard_outcome(
            AutofixMode::Write,
            plan.files_modified,
            Some(format!("homeboy test {} --analyze", args.component_id)),
            plan.hints.clone(),
        );

        Some((plan, outcome))
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

    let results_file_str = results_file.to_string_lossy().to_string();
    let coverage_file_str = coverage_file
        .as_ref()
        .map(|file| file.to_string_lossy().to_string());
    let failures_file_str = failures_file
        .as_ref()
        .map(|file| file.to_string_lossy().to_string());

    let output = build_test_runner(
        component,
        args.path_override.clone(),
        &args.settings,
        args.skip_lint,
        coverage_enabled,
        &results_file_str,
        coverage_file_str.as_deref(),
        failures_file_str.as_deref(),
        args.coverage_min,
        changed_test_files,
    )?
    .script_args(&args.passthrough_args)
    .run()?;

    let test_counts =
        parse_test_results_file(&results_file).or_else(|| parse_test_results_text(&output.stdout));
    let _ = std::fs::remove_file(&results_file);

    let test_autofix = planned_autofix
        .as_ref()
        .map(|(plan, outcome)| AppliedRefactor::from_plan(plan, outcome.rerun_recommended));

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
    if let Some(ref file) = coverage_file {
        let _ = std::fs::remove_file(file);
    }

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

        if let Some(ref file) = failures_file {
            let _ = std::fs::remove_file(file);
        }

        Some(test_analyze::analyze(&args.component_id, &analysis_input))
    } else {
        if let Some(ref file) = failures_file {
            let _ = std::fs::remove_file(file);
        }
        None
    };

    if args.baseline {
        if let Some(ref counts) = test_counts {
            let _ = test_baseline::save_baseline(source_path, &args.component_id, counts)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline && !args.ignore_baseline {
        if let Some(ref counts) = test_counts {
            let resolved_baseline = test_baseline::load_baseline(source_path).or_else(|| {
                args.changed_since.as_ref().and_then(|git_ref| {
                    test_baseline::load_baseline_from_ref(&source_path.to_string_lossy(), git_ref)
                })
            });

            if let Some(existing_baseline) = resolved_baseline {
                let comparison = test_baseline::compare(counts, &existing_baseline);

                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if (comparison.passed_delta > 0 || comparison.failed_delta < 0)
                    && args.ratchet
                {
                    let _ = test_baseline::save_baseline(source_path, &args.component_id, counts);
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let mut hints = Vec::new();
    if let Some((_, outcome)) = &planned_autofix {
        hints.extend(outcome.hints.clone());
    }

    if status == "failed" && args.passthrough_args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy test {} -- --filter=TestName",
            args.component_id
        ));
    }

    if !args.skip_lint && !args.fix {
        hints.push(format!(
            "Auto-fix lint issues: homeboy test {} --fix",
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
