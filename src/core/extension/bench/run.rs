//! Bench main workflow: invoke extension runner, load JSON, apply baseline.

use std::path::PathBuf;

use serde::Serialize;

use crate::component::Component;
use crate::engine::baseline::BaselineFlags;
use crate::engine::run_dir::{self, RunDir};
use crate::error::Result;
use crate::extension::bench::baseline::{self, BenchBaselineComparison};
use crate::extension::bench::parsing::{self, BenchResults};
use crate::extension::{resolve_execution_context, ExtensionCapability, ExtensionRunner};

#[derive(Debug, Clone)]
pub struct BenchRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub iterations: u64,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub json_summary: bool,
    pub passthrough_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    pub results: Option<BenchResults>,
    pub baseline_comparison: Option<BenchBaselineComparison>,
    pub hints: Option<Vec<String>>,
}

/// Runs the extension's bench script and produces a structured result.
///
/// Same runner contract as test/lint/build: the script writes a JSON
/// envelope to `$HOMEBOY_BENCH_RESULTS_FILE`. Iteration count is passed
/// via `$HOMEBOY_BENCH_ITERATIONS`. Runner exit code is taken as the
/// primary signal; baseline regressions can override to 1.
pub fn run_main_bench_workflow(
    component: &Component,
    source_path: &PathBuf,
    args: BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchRunWorkflowResult> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    let execution_context = resolve_execution_context(component, ExtensionCapability::Bench)?;

    let runner_output = ExtensionRunner::for_context(execution_context)
        .component(component.clone())
        .path_override(args.path_override.clone())
        .settings(&args.settings)
        .with_run_dir(run_dir)
        .env("HOMEBOY_BENCH_ITERATIONS", &args.iterations.to_string())
        .script_args(&args.passthrough_args)
        .run()?;

    let parsed = if results_file.exists() {
        parsing::parse_bench_results_file(&results_file).ok()
    } else {
        None
    };

    let status = if runner_output.success {
        "passed"
    } else {
        "failed"
    };

    if args.baseline_flags.baseline {
        if let Some(ref r) = parsed {
            let _ = baseline::save_baseline(source_path, &args.component_id, r)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(ref r) = parsed {
            if let Some(existing) = baseline::load_baseline(source_path) {
                let comparison = baseline::compare(r, &existing, args.regression_threshold_percent);

                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if comparison.has_improvements && args.baseline_flags.ratchet {
                    let _ = baseline::save_baseline(source_path, &args.component_id, r);
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let mut hints = Vec::new();
    if parsed.is_some() && !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save bench baseline: homeboy bench {} --baseline",
            args.component_id
        ));
    }
    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: homeboy bench {} --ratchet",
            args.component_id
        ));
    }
    if let Some(ref cmp) = baseline_comparison {
        if cmp.regression {
            hints.push(format!(
                "Regression threshold: {}%. Raise it with --regression-threshold=<PCT> if expected.",
                cmp.threshold_percent
            ));
        }
    }
    hints.push("Full options: homeboy docs commands/bench".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    let exit_code = baseline_exit_override.unwrap_or(runner_output.exit_code);

    Ok(BenchRunWorkflowResult {
        status: status.to_string(),
        component: args.component_label,
        exit_code,
        iterations: args.iterations,
        results: parsed,
        baseline_comparison,
        hints,
    })
}
