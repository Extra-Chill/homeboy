//! Main bench workflow orchestration: validate, run, gate, baseline.

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::bench::baseline;
use crate::core::extension::bench::diagnostic::{self, BenchDiagnostic};
use crate::core::extension::bench::failure_diagnostic::bench_failure_stderr_tail;
use crate::core::extension::bench::parsing::{BenchRunMetadata, BenchRunnerMetadata};
use crate::core::extension::bench::responsiveness::{memory_sample, read_responsiveness_summary};
use crate::core::extension::bench::run_metadata::stamp_run_metadata;
use crate::core::extension::{resolve_execution_context, ExtensionCapability};

use super::failure::classify_bench_failure;
use super::memory_timeline::{
    attach_memory_timeline_artifacts, preserve_memory_timeline_artifacts,
};
use super::results::{parse_execution_results_file, workload_status_failures};
use super::runner::{
    build_runner, clear_responsiveness_file, run_concurrent_instances, run_sequential_runs,
};
use super::scenario::{
    apply_scenario_filter, discover_bench_scenarios, failure_scenario_id,
    filter_extra_workloads_by_scenario_ids,
};
use super::types::{BenchRunFailure, BenchRunWorkflowArgs, BenchRunWorkflowResult};

pub(crate) fn validate_bench_run_args(args: &BenchRunWorkflowArgs) -> Result<()> {
    require_positive("concurrency", args.execution.concurrency as u64)?;
    require_positive("runs", args.execution.runs)?;
    if args.execution.concurrency > 1 && args.shared_state.is_none() {
        return Err(Error::validation_invalid_argument(
            "concurrency",
            "--concurrency > 1 requires --shared-state <DIR>; \
             N parallel cold-boots without shared state are N independent \
             runs, not a multi-instance contention test",
            None,
            None,
        ));
    }

    Ok(())
}

pub(crate) fn require_positive(name: &str, value: u64) -> Result<()> {
    if value == 0 {
        return Err(Error::validation_invalid_argument(
            name,
            "must be >= 1",
            None,
            None,
        ));
    }

    Ok(())
}

/// Runs the extension's bench script and produces a structured result.
///
/// Same runner contract as test/lint/build: the script writes a JSON
/// envelope to `$HOMEBOY_BENCH_RESULTS_FILE`. Iteration count is passed
/// via `$HOMEBOY_BENCH_ITERATIONS`. Runner exit code is taken as the
/// primary signal; baseline regressions can override to 1.
///
/// ## Shared state and concurrency
///
/// When `args.shared_state` is set, the path is exported as
/// `$HOMEBOY_BENCH_SHARED_STATE` so workloads can persist on-disk state
/// across iterations.
///
/// When `args.execution.concurrency > 1`, N runner instances are spawned in
/// parallel threads. Each gets a distinct `$HOMEBOY_BENCH_INSTANCE_ID`
/// (`0..N-1`), `$HOMEBOY_BENCH_CONCURRENCY` (`N`), and a per-instance
/// results file (`bench-results-i<n>.json` under the run dir). After all
/// instances finish, their `BenchResults` are merged: scenario IDs are
/// suffixed with `:i<n>` so each instance's measurements stay
/// distinguishable in the aggregated envelope and the baseline. This
/// keeps the regression checker working unchanged — a regression in
/// instance 2 surfaces as a regression on `<id>:i2`, not as silent
/// averaging.
pub fn run_main_bench_workflow(
    component: &Component,
    source_path: &Path,
    args: BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchRunWorkflowResult> {
    validate_bench_run_args(&args)?;
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    if let Some(ref shared) = args.shared_state {
        std::fs::create_dir_all(shared).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to create shared-state dir {}: {}",
                    shared.display(),
                    e
                ),
                Some("bench.run.shared_state".to_string()),
            )
        })?;
    }

    if component.has_script(ExtensionCapability::Bench) {
        clear_responsiveness_file(run_dir)?;
        let component_env = bench_component_script_env(&args, run_dir)?;
        let script_output =
            crate::core::extension::component_script::run_component_scripts_with_run_dir(
                component,
                ExtensionCapability::Bench,
                source_path,
                run_dir,
                true,
                &component_env,
                &args.passthrough_args,
            )?;
        preserve_memory_timeline_artifacts(script_output.child_resource.as_ref(), run_dir, None)?;
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        let mut parsed = if results_file.exists() {
            parse_execution_results_file(
                &results_file,
                &args.scenario_ids,
                script_output.success,
                args.rig_id.as_deref(),
            )?
        } else {
            None
        };
        if let Some(results) = parsed.as_mut() {
            results.run_metadata = Some(BenchRunMetadata {
                homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                started_at: started_at.clone(),
                shared_state: args
                    .shared_state
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string()),
                iterations: args.iterations,
                execution: args.execution,
                warmup_iterations: args.warmup_iterations,
                selected_scenarios: args.scenario_ids.clone(),
                env_overrides: BTreeMap::new(),
                workloads: Vec::new(),
                provenance: results.provenance.clone(),
                runner: Some(BenchRunnerMetadata {
                    extension: "component-script".to_string(),
                    path: source_path.to_string_lossy().to_string(),
                    source_revision: None,
                }),
                lifecycle: None,
                diagnostics: Vec::new(),
            });
            attach_memory_timeline_artifacts(
                results,
                script_output.child_resource.as_ref(),
                run_dir,
                None,
            )?;
        }
        let responsiveness = read_responsiveness_summary(
            &run_dir.step_file(run_dir::files::BENCH_RESPONSIVENESS),
            script_output
                .child_resource
                .as_ref()
                .map(|resource| resource.duration_ms),
        )?;
        if let Some(results) = parsed.as_mut() {
            results.responsiveness = responsiveness.clone();
        }
        let mut gate_failures = parsed
            .as_mut()
            .map(crate::core::extension::bench::gate::evaluate_gates)
            .unwrap_or_default();
        if let Some(results) = parsed.as_ref() {
            gate_failures.extend(workload_status_failures(results));
        }
        let gate_results = parsed
            .as_ref()
            .map(crate::core::extension::bench::gate::normalized_gate_results)
            .unwrap_or_default();
        let gates_passed = gate_failures.is_empty();
        let status = if script_output.success && gates_passed {
            "passed"
        } else {
            "failed"
        };
        let failure_classification = classify_bench_failure(
            script_output.success,
            script_output.exit_code,
            &script_output.stderr,
            responsiveness.as_ref(),
        );
        if let Some(results) = parsed.as_mut() {
            if !script_output.success && results.failure_classification.is_none() {
                results.failure_classification = failure_classification.clone();
            }
        }
        let failure = (!script_output.success).then(|| BenchRunFailure {
            component_id: args.component_id.clone(),
            component_path: Some(source_path.to_string_lossy().to_string()),
            scenario_id: failure_scenario_id(&args.scenario_ids),
            exit_code: script_output.exit_code,
            stderr_tail: bench_failure_stderr_tail(&script_output.stderr, &args),
            failure_classification,
            responsiveness,
            memory_sample: None,
            diagnostics: Vec::new(),
        });
        let exit_code = if script_output.exit_code != 0 {
            script_output.exit_code
        } else if !gates_passed {
            1
        } else {
            0
        };
        let mut hints = vec![
            "Component scripts use the extension runner env contract without extension resolution."
                .to_string(),
        ];
        hints.extend(gate_failures.iter().cloned());

        return Ok(BenchRunWorkflowResult {
            status: status.to_string(),
            component: args.component_label,
            exit_code,
            iterations: args.iterations,
            results: parsed,
            gate_results,
            gate_failures,
            baseline_comparison: None,
            hints: Some(hints),
            failure,
            diagnostics: Vec::new(),
        });
    }

    let execution_context = resolve_execution_context(component, ExtensionCapability::Bench)?;

    let mut execution_args = args.clone();
    if !args.scenario_ids.is_empty() {
        let discovered = discover_bench_scenarios(&execution_context, component, &args, run_dir)?;
        apply_scenario_filter(discovered, &args.scenario_ids)?;
        execution_args.extra_workloads =
            filter_extra_workloads_by_scenario_ids(&args.extra_workloads, &args.scenario_ids);
    }

    clear_responsiveness_file(run_dir)?;
    let (
        mut parsed,
        runner_success,
        runner_exit_code,
        failure_stderr_tail,
        failure_memory_sample,
        responsiveness_observed_elapsed_ms,
    ) = if execution_args.execution.runs > 1 {
        run_sequential_runs(&execution_context, component, &execution_args, run_dir)?
    } else if execution_args.execution.concurrency <= 1 {
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        let runner_output = build_runner(
            &execution_context,
            component,
            &execution_args,
            run_dir,
            None,
        )?
        .run()?;
        preserve_memory_timeline_artifacts(runner_output.child_resource.as_ref(), run_dir, None)?;
        let parsed = parse_execution_results_file(
            &results_file,
            &execution_args.scenario_ids,
            runner_output.success,
            execution_args.rig_id.as_deref(),
        )?;
        let mut parsed = parsed;
        if let Some(results) = parsed.as_mut() {
            attach_memory_timeline_artifacts(
                results,
                runner_output.child_resource.as_ref(),
                run_dir,
                None,
            )?;
        }
        let failure_stderr_tail = if !runner_output.success {
            Some(bench_failure_stderr_tail(
                &runner_output.stderr,
                &execution_args,
            ))
        } else {
            None
        };
        (
            parsed,
            runner_output.success,
            runner_output.exit_code,
            failure_stderr_tail,
            memory_sample(runner_output.child_resource.as_ref()),
            runner_output
                .child_resource
                .as_ref()
                .map(|resource| resource.duration_ms),
        )
    } else {
        run_concurrent_instances(&execution_context, component, &execution_args, run_dir)?
    };

    let responsiveness = read_responsiveness_summary(
        &run_dir.step_file(run_dir::files::BENCH_RESPONSIVENESS),
        responsiveness_observed_elapsed_ms,
    )?;

    if let Some(results) = parsed.as_mut() {
        results.responsiveness = responsiveness.clone();
        stamp_run_metadata(
            results,
            &execution_context,
            component,
            &execution_args,
            &started_at,
        );
    }

    let mut gate_failures = parsed
        .as_mut()
        .map(crate::core::extension::bench::gate::evaluate_gates)
        .unwrap_or_default();
    if let Some(results) = parsed.as_ref() {
        gate_failures.extend(workload_status_failures(results));
    }
    let gate_results = parsed
        .as_ref()
        .map(crate::core::extension::bench::gate::normalized_gate_results)
        .unwrap_or_default();
    let gates_passed = gate_failures.is_empty();
    let diagnostics = diagnostic::collect_diagnostics(parsed.as_ref());

    if let Some(results) = parsed.as_mut() {
        if let Some(metadata) = results.run_metadata.as_mut() {
            metadata.diagnostics = diagnostics.clone();
        }
    }

    let failure_classification = classify_bench_failure(
        runner_success,
        runner_exit_code,
        failure_stderr_tail.as_deref().unwrap_or_default(),
        responsiveness.as_ref(),
    );
    if let Some(results) = parsed.as_mut() {
        if !runner_success && results.failure_classification.is_none() {
            results.failure_classification = failure_classification.clone();
        }
    }

    let status = if runner_success && gates_passed {
        "passed"
    } else {
        "failed"
    };

    let rig_id = args.rig_id.as_deref();

    if args.baseline_flags.baseline && gates_passed {
        if let Some(ref r) = parsed {
            let _ = baseline::save_baseline(source_path, &args.component_id, r, rig_id)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(ref r) = parsed {
            if let Some(existing) = baseline::load_baseline(source_path, rig_id) {
                let comparison = baseline::compare(r, &existing, args.regression_threshold_percent);

                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if comparison.has_improvements && args.baseline_flags.ratchet {
                    let _ = baseline::save_baseline(source_path, &args.component_id, r, rig_id);
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let bench_invocation = match rig_id {
        Some(id) => format!("homeboy bench {} --rig {}", args.component_id, id),
        None => format!("homeboy bench {}", args.component_id),
    };

    let mut hints = Vec::new();
    if parsed.is_some() && !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save bench baseline: {} --baseline",
            bench_invocation
        ));
    }
    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: {} --ratchet",
            bench_invocation
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
    for failure in &gate_failures {
        hints.push(failure.clone());
    }
    for diagnostic in &diagnostics {
        hints.push(format_diagnostic_hint(diagnostic));
    }
    hints.push("Full options: homeboy docs commands/bench".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    let exit_code = if runner_exit_code != 0 {
        runner_exit_code
    } else if !gates_passed {
        1
    } else {
        baseline_exit_override.unwrap_or(0)
    };
    let failure = if !runner_success {
        failure_stderr_tail.map(|stderr_tail| BenchRunFailure {
            component_id: args.component_id.clone(),
            component_path: args
                .path_override
                .clone()
                .or_else(|| Some(component.local_path.clone())),
            scenario_id: failure_scenario_id(&execution_args.scenario_ids),
            exit_code: runner_exit_code,
            stderr_tail,
            failure_classification,
            responsiveness,
            memory_sample: failure_memory_sample,
            diagnostics: diagnostics.clone(),
        })
    } else {
        None
    };

    Ok(BenchRunWorkflowResult {
        status: status.to_string(),
        component: args.component_label,
        exit_code,
        iterations: args.iterations,
        results: parsed,
        gate_results,
        gate_failures,
        baseline_comparison,
        hints,
        failure,
        diagnostics,
    })
}

pub(crate) fn bench_component_script_env(
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<Vec<(String, String)>> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    let mut env = vec![
        (
            "HOMEBOY_BENCH_RESULTS_FILE".to_string(),
            results_file.to_string_lossy().to_string(),
        ),
        (
            "HOMEBOY_BENCH_ITERATIONS".to_string(),
            args.iterations.to_string(),
        ),
        (
            "HOMEBOY_BENCH_WARMUP_ITERATIONS".to_string(),
            args.warmup_iterations.unwrap_or(0).to_string(),
        ),
        (
            "HOMEBOY_BENCH_RESPONSIVENESS_MISSED_MS".to_string(),
            crate::core::extension::bench::responsiveness::missed_ping_window_ms().to_string(),
        ),
        (
            "HOMEBOY_BENCH_SCENARIOS".to_string(),
            args.scenario_ids.join(","),
        ),
        (
            "HOMEBOY_SETTINGS_JSON".to_string(),
            crate::core::extension::build_settings_json_from_manifest(
                &serde_json::json!({}),
                &[],
                &args.settings,
                &args.settings_json,
            )?,
        ),
    ];
    if let Some(run_id) = args
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        env.push(("HOMEBOY_BENCH_RUN_ID".to_string(), run_id.to_string()));
    }
    env.extend(args.ci_env.iter().cloned());
    Ok(env)
}

pub(crate) fn format_diagnostic_hint(diagnostic: &BenchDiagnostic) -> String {
    match diagnostic.message.as_deref() {
        Some(message) => format!("Diagnostic `{}`: {}", diagnostic.class, message),
        None => format!("Diagnostic `{}`", diagnostic.class),
    }
}
