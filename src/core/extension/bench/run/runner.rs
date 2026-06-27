//! Runner construction and single/sequential/concurrent dispatch.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread;

use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::bench::aggregate_runs;
use crate::core::extension::bench::failure_diagnostic::bench_failure_stderr_tail;
use crate::core::extension::bench::parsing::{self, BenchResults, BenchScenario};
use crate::core::extension::bench::responsiveness::{memory_sample, BenchFailureMemorySample};
use crate::core::extension::{
    build_scenario_runner, ExtensionExecutionContext, ExtensionRunner, ScenarioRunnerOptions,
};

use super::memory_timeline::{
    attach_memory_timeline_artifacts, preserve_memory_timeline_artifacts,
};
use super::results::{is_unmeasured_inventory_results, parse_execution_results_file};
use super::scenario::apply_scenario_filter;
use super::types::{BenchRunExecutionOutcome, BenchRunWorkflowArgs};

pub(crate) fn run_sequential_runs(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchRunExecutionOutcome> {
    let mut parsed_runs = Vec::new();
    let mut all_success = true;
    let mut first_failure_exit: Option<i32> = None;
    let mut first_failure_stderr_tail: Option<String> = None;
    let mut first_failure_memory_sample: Option<BenchFailureMemorySample> = None;
    let mut observed_elapsed_ms: Option<u128> = None;

    for _ in 0..args.execution.runs {
        let (parsed, success, exit_code, stderr_tail, memory_sample, run_elapsed_ms) =
            if args.execution.concurrency <= 1 {
                run_single_dispatcher(execution_context, component, args, run_dir)?
            } else {
                run_concurrent_instances(execution_context, component, args, run_dir)?
            };
        observed_elapsed_ms = Some(
            observed_elapsed_ms
                .unwrap_or_default()
                .max(run_elapsed_ms.unwrap_or_default()),
        );
        if !success {
            all_success = false;
            if first_failure_exit.is_none() {
                first_failure_exit = Some(exit_code);
            }
            if first_failure_stderr_tail.is_none() {
                first_failure_stderr_tail = stderr_tail;
            }
            if first_failure_memory_sample.is_none() {
                first_failure_memory_sample = memory_sample;
            }
        }
        if let Some(result) = parsed {
            parsed_runs.push(result);
        }
    }

    let merged = if parsed_runs.is_empty() {
        None
    } else {
        Some(aggregate_runs(&parsed_runs)?)
    };
    let exit_code = if all_success {
        0
    } else {
        first_failure_exit.unwrap_or(1)
    };

    Ok((
        merged,
        all_success,
        exit_code,
        first_failure_stderr_tail,
        first_failure_memory_sample,
        observed_elapsed_ms,
    ))
}

pub(crate) fn run_single_dispatcher(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchRunExecutionOutcome> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    if results_file.exists() {
        std::fs::remove_file(&results_file).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to clear previous bench results file {}: {}",
                    results_file.display(),
                    e
                ),
                Some("bench.run.results_file".to_string()),
            )
        })?;
    }

    let runner_output = build_runner(execution_context, component, args, run_dir, None)?.run()?;
    let parsed = parse_execution_results_file(
        &results_file,
        &args.scenario_ids,
        runner_output.success,
        args.rig_id.as_deref(),
    )?;
    let failure_stderr_tail = if !runner_output.success {
        Some(bench_failure_stderr_tail(&runner_output.stderr, args))
    } else {
        None
    };
    Ok((
        parsed,
        runner_output.success,
        runner_output.exit_code,
        failure_stderr_tail,
        memory_sample(runner_output.child_resource.as_ref()),
        runner_output
            .child_resource
            .as_ref()
            .map(|resource| resource.duration_ms),
    ))
}

/// Per-instance results filename within the run dir.
///
/// Single source of truth so the runner-side override and the parent
/// reader agree on the path. Single-instance runs keep the legacy
/// `bench-results.json` filename for backward compatibility with any
/// extension that hardcodes it.
pub(crate) fn instance_results_filename(instance_id: u32) -> String {
    format!("bench-results-i{}.json", instance_id)
}

/// Build the `ExtensionRunner` for a single bench invocation.
///
/// `instance` is `Some((id, total))` for multi-instance runs (each gets
/// its own results file + instance/concurrency env vars), or `None` for
/// the legacy single-instance path.
pub(crate) fn build_runner(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
    instance: Option<(u32, u32)>,
) -> Result<ExtensionRunner> {
    let mut runner = build_scenario_runner(ScenarioRunnerOptions {
        execution_context,
        component,
        path_override: args.path_override.clone(),
        settings: &args.settings,
        settings_json: &args.settings_json,
        run_dir,
        results_env: None,
        scenario_env: None,
        artifact_env: None,
        list_only_env: None,
        extra_workloads_env: Some((
            "HOMEBOY_BENCH_EXTRA_WORKLOADS",
            &args.extra_workloads,
            "bench_workloads",
        )),
        env_provider_extensions: &args.env_provider_extensions,
        invocation_requirements: args.invocation_requirements.clone(),
    })?
    .env("HOMEBOY_BENCH_ITERATIONS", &args.iterations.to_string())
    .env(
        "HOMEBOY_BENCH_RESPONSIVENESS_MISSED_MS",
        &crate::core::extension::bench::responsiveness::missed_ping_window_ms().to_string(),
    )
    .env("HOMEBOY_BENCH_PROGRESS", bench_progress_env_value())
    .env("HOMEBOY_BENCH_PROGRESS_STREAM", "stderr")
    .script_args(&args.passthrough_args)
    .passthrough(false)
    .stderr_passthrough(bench_progress_enabled());

    for (key, value) in &args.ci_env {
        runner = runner.env(key, value);
    }

    if let Some(warmup_iterations) = args.warmup_iterations {
        runner = runner.env(
            "HOMEBOY_BENCH_WARMUP_ITERATIONS",
            &warmup_iterations.to_string(),
        );
    }

    if !args.scenario_ids.is_empty() {
        runner = runner.env("HOMEBOY_BENCH_SCENARIOS", &args.scenario_ids.join(","));
    }

    if let Some(ref shared) = args.shared_state {
        runner = runner.env("HOMEBOY_BENCH_SHARED_STATE", &shared.to_string_lossy());
    }

    if let Some((instance_id, concurrency)) = instance {
        let results_path = run_dir.step_file(&instance_results_filename(instance_id));
        runner = runner
            .env(
                "HOMEBOY_BENCH_RESULTS_FILE",
                &results_path.to_string_lossy(),
            )
            .env("HOMEBOY_BENCH_INSTANCE_ID", &instance_id.to_string())
            .env("HOMEBOY_BENCH_CONCURRENCY", &concurrency.to_string());
    }

    Ok(runner)
}

pub(crate) fn clear_responsiveness_file(run_dir: &RunDir) -> Result<()> {
    let path = run_dir.step_file(run_dir::files::BENCH_RESPONSIVENESS);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to clear previous bench responsiveness file {}: {}",
                    path.display(),
                    e
                ),
                Some("bench.run.responsiveness_file".to_string()),
            )
        })?;
    }
    Ok(())
}

pub(crate) fn bench_progress_enabled() -> bool {
    match std::env::var("HOMEBOY_BENCH_PROGRESS") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => std::env::var_os("CI").is_none(),
    }
}

pub(crate) fn bench_progress_env_value() -> &'static str {
    if bench_progress_enabled() {
        "1"
    } else {
        "0"
    }
}

/// Spawn N runner instances in parallel, wait for all, aggregate.
///
/// Returns `(merged_results, all_succeeded, exit_code)`. Per-instance
/// scenarios are merged with `:i<n>` suffixed IDs so each instance's
/// measurements stay distinct in the envelope and the baseline. If any
/// instance failed, the aggregate run reports failure with that
/// instance's exit code (first failure wins).
pub(crate) fn run_concurrent_instances(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchRunExecutionOutcome> {
    let concurrency = args.execution.concurrency;
    let execution_context = Arc::new(execution_context.clone());
    let component = Arc::new(component.clone());
    let args_arc = Arc::new(args.clone());
    let run_dir = Arc::new(run_dir.clone());

    let mut handles = Vec::with_capacity(concurrency as usize);
    for instance_id in 0..concurrency {
        let ctx = Arc::clone(&execution_context);
        let comp = Arc::clone(&component);
        let a = Arc::clone(&args_arc);
        let rd = Arc::clone(&run_dir);
        handles.push(thread::spawn(move || -> Result<(u32, _)> {
            let runner = build_runner(&ctx, &comp, &a, &rd, Some((instance_id, concurrency)))?;
            let output = runner.run()?;
            Ok((instance_id, output))
        }));
    }

    let mut per_instance: Vec<(u32, crate::core::extension::RunnerOutput)> =
        Vec::with_capacity(concurrency as usize);
    for h in handles {
        match h.join() {
            Ok(Ok(pair)) => per_instance.push(pair),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Error::internal_unexpected("bench instance thread panicked")),
        }
    }

    per_instance.sort_by_key(|(id, _)| *id);

    // First failure wins for the exit code surface; status is "all-or-nothing".
    let mut all_success = true;
    let mut first_failure_exit: Option<i32> = None;
    let mut first_failure_stderr_tail: Option<String> = None;
    let mut first_failure_memory_sample: Option<BenchFailureMemorySample> = None;
    let mut observed_elapsed_ms: Option<u128> = None;
    for (_, output) in &per_instance {
        observed_elapsed_ms = Some(
            observed_elapsed_ms.unwrap_or_default().max(
                output
                    .child_resource
                    .as_ref()
                    .map(|resource| resource.duration_ms)
                    .unwrap_or_default(),
            ),
        );
        if !output.success {
            all_success = false;
            if first_failure_exit.is_none() {
                first_failure_exit = Some(output.exit_code);
            }
            if first_failure_stderr_tail.is_none() {
                first_failure_stderr_tail = Some(bench_failure_stderr_tail(&output.stderr, args));
            }
            if first_failure_memory_sample.is_none() {
                first_failure_memory_sample = memory_sample(output.child_resource.as_ref());
            }
        }
    }
    let exit_code = if all_success {
        0
    } else {
        first_failure_exit.unwrap_or(1)
    };

    // Read & merge per-instance results files.
    let mut merged_scenarios: Vec<BenchScenario> = Vec::new();
    let mut budget_findings = Vec::new();
    let mut component_id_seen: Option<String> = None;
    let mut iterations_seen: Option<u64> = None;
    let mut metric_policies_seen: std::collections::BTreeMap<String, parsing::BenchMetricPolicy> =
        std::collections::BTreeMap::new();

    for (instance_id, _) in &per_instance {
        let path = run_dir.step_file(&instance_results_filename(*instance_id));
        let child_resource = per_instance
            .iter()
            .find(|(output_instance_id, _)| output_instance_id == instance_id)
            .and_then(|(_, output)| output.child_resource.as_ref());
        preserve_memory_timeline_artifacts(
            child_resource,
            &run_dir,
            Some(&format!("i{}", instance_id)),
        )?;
        if !path.exists() {
            continue;
        }
        let parsed = match parsing::parse_bench_results_file_with_artifact_context_and_scenarios(
            &path,
            args.rig_id.as_deref(),
            &args.scenario_ids,
        )
        .and_then(|results| apply_scenario_filter(results, &args.scenario_ids))
        {
            Ok(mut p) => {
                if is_unmeasured_inventory_results(&p) {
                    continue;
                }
                attach_memory_timeline_artifacts(
                    &mut p,
                    child_resource,
                    &run_dir,
                    Some(&format!("i{}", instance_id)),
                )?;
                p
            }
            Err(_) => continue,
        };
        if component_id_seen.is_none() {
            component_id_seen = Some(parsed.component_id.clone());
        }
        if iterations_seen.is_none() {
            iterations_seen = Some(parsed.iterations);
        }
        for (k, v) in parsed.metric_policies.into_iter() {
            metric_policies_seen.entry(k).or_insert(v);
        }
        budget_findings.extend(parsed.budget_findings);
        for mut scenario in parsed.scenarios {
            scenario.id = format!("{}:i{}", scenario.id, instance_id);
            merged_scenarios.push(scenario);
        }
    }

    let merged = if merged_scenarios.is_empty() && component_id_seen.is_none() {
        None
    } else {
        Some(BenchResults {
            component_id: component_id_seen.unwrap_or_else(|| args.component_id.clone()),
            iterations: iterations_seen.unwrap_or(args.iterations),
            provenance: Default::default(),
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: Vec::new(),
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            responsiveness: None,
            budget_findings,
            scenarios: merged_scenarios,
            metric_policies: metric_policies_seen,
            metric_policy_presets: std::collections::BTreeMap::new(),
        })
    };

    Ok((
        merged,
        all_success,
        exit_code,
        first_failure_stderr_tail,
        first_failure_memory_sample,
        observed_elapsed_ms,
    ))
}
