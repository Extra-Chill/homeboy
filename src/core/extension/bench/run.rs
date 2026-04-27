//! Bench main workflow: invoke extension runner, load JSON, apply baseline.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use serde::Serialize;

use crate::component::Component;
use crate::engine::baseline::BaselineFlags;
use crate::engine::run_dir::{self, RunDir};
use crate::error::{Error, Result};
use crate::extension::bench::baseline::{self, BenchBaselineComparison};
use crate::extension::bench::parsing::{
    self, BenchMetrics, BenchResults, BenchRunDistribution, BenchRunSnapshot, BenchScenario,
};
use crate::extension::{
    resolve_execution_context, ExtensionCapability, ExtensionExecutionContext, ExtensionRunner,
};

#[derive(Debug, Clone)]
pub struct BenchRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    /// Typed-JSON setting overrides from `--setting-json key=<json>`.
    /// Applied after `settings` (string overrides) so JSON wins on
    /// conflict. Required for object-shaped settings like
    /// `wp_config_defines` / `bench_env` whose dispatchers expect a JSON
    /// object, not a JSON-string-of-an-object.
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub iterations: u64,
    pub runs: u64,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub json_summary: bool,
    pub passthrough_args: Vec<String>,
    /// Optional rig identifier when bench was invoked via `--rig <id>`.
    /// Threads through to the baseline storage key so rig-pinned and
    /// unpinned baselines stay in separate slots inside `homeboy.json`.
    /// `None` preserves the original baseline shape exactly.
    pub rig_id: Option<String>,
    /// Optional shared-state directory mounted across iterations and
    /// instances. When set, the dispatcher exposes the path to workloads
    /// via `$HOMEBOY_BENCH_SHARED_STATE` so they can persist on-disk
    /// state (SQLite files, content directories, counter files) that
    /// outlives a single iteration. Required when `concurrency > 1`.
    pub shared_state: Option<PathBuf>,
    /// Number of parallel runner instances to spawn. `1` (default)
    /// preserves single-instance behaviour. `> 1` requires `shared_state`
    /// to be set — N independent cold-boots without shared state would
    /// be N independent runs, not a multi-instance contention test.
    pub concurrency: u32,
    /// Rig-declared out-of-tree workloads to run alongside in-tree discovery.
    /// Exported to dispatchers as `HOMEBOY_BENCH_EXTRA_WORKLOADS`.
    pub extra_workloads: Vec<PathBuf>,
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

#[derive(Debug, Clone)]
pub struct BenchListWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub passthrough_args: Vec<String>,
    pub extra_workloads: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchListWorkflowResult {
    pub component: String,
    pub component_id: String,
    pub scenarios: Vec<BenchScenario>,
    pub count: usize,
}

/// Discover bench scenarios without executing workloads.
pub fn run_bench_list_workflow(
    component: &Component,
    args: BenchListWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchListWorkflowResult> {
    let execution_context = resolve_execution_context(component, ExtensionCapability::Bench)?;
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);

    let runner_output = build_runner(
        &execution_context,
        component,
        &BenchRunWorkflowArgs {
            component_label: args.component_label.clone(),
            component_id: args.component_id.clone(),
            path_override: args.path_override,
            settings: args.settings,
            settings_json: args.settings_json,
            iterations: 0,
            runs: 1,
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent: 0.0,
            json_summary: false,
            passthrough_args: args.passthrough_args,
            rig_id: None,
            shared_state: None,
            concurrency: 1,
            extra_workloads: args.extra_workloads,
        },
        run_dir,
        None,
    )?
    .env("HOMEBOY_BENCH_LIST_ONLY", "1")
    .run()?;

    if !runner_output.success {
        return Err(Error::validation_invalid_argument(
            "bench_list",
            format!(
                "bench scenario discovery failed with exit code {}",
                runner_output.exit_code
            ),
            Some(format!(
                "stdout:\n{}\n\nstderr:\n{}",
                runner_output.stdout, runner_output.stderr
            )),
            None,
        ));
    }

    let parsed = parsing::parse_bench_results_file(&results_file)?;
    let count = parsed.scenarios.len();

    Ok(BenchListWorkflowResult {
        component: args.component_label,
        component_id: parsed.component_id,
        scenarios: parsed.scenarios,
        count,
    })
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
/// When `args.concurrency > 1`, N runner instances are spawned in
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
    source_path: &PathBuf,
    args: BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchRunWorkflowResult> {
    if args.concurrency == 0 {
        return Err(Error::validation_invalid_argument(
            "concurrency",
            "must be >= 1",
            None,
            None,
        ));
    }
    if args.runs == 0 {
        return Err(Error::validation_invalid_argument(
            "runs",
            "must be >= 1",
            None,
            None,
        ));
    }
    if args.concurrency > 1 && args.shared_state.is_none() {
        return Err(Error::validation_invalid_argument(
            "concurrency",
            "--concurrency > 1 requires --shared-state <DIR>; \
             N parallel cold-boots without shared state are N independent \
             runs, not a multi-instance contention test",
            None,
            None,
        ));
    }

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

    let execution_context = resolve_execution_context(component, ExtensionCapability::Bench)?;

    let (parsed, runner_success, runner_exit_code) = if args.runs > 1 {
        run_sequential_runs(&execution_context, component, &args, run_dir)?
    } else if args.concurrency <= 1 {
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        let runner_output =
            build_runner(&execution_context, component, &args, run_dir, None)?.run()?;
        let parsed = if results_file.exists() {
            parsing::parse_bench_results_file(&results_file).ok()
        } else {
            None
        };
        (parsed, runner_output.success, runner_output.exit_code)
    } else {
        run_concurrent_instances(&execution_context, component, &args, run_dir)?
    };

    let status = if runner_success { "passed" } else { "failed" };

    let rig_id = args.rig_id.as_deref();

    if args.baseline_flags.baseline {
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
    hints.push("Full options: homeboy docs commands/bench".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    let exit_code = baseline_exit_override.unwrap_or(runner_exit_code);

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

fn run_sequential_runs(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(Option<BenchResults>, bool, i32)> {
    let mut parsed_runs = Vec::new();
    let mut all_success = true;
    let mut first_failure_exit: Option<i32> = None;

    for _ in 0..args.runs {
        let (parsed, success, exit_code) = if args.concurrency <= 1 {
            run_single_dispatcher(execution_context, component, args, run_dir)?
        } else {
            run_concurrent_instances(execution_context, component, args, run_dir)?
        };
        if !success {
            all_success = false;
            if first_failure_exit.is_none() {
                first_failure_exit = Some(exit_code);
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

    Ok((merged, all_success, exit_code))
}

fn run_single_dispatcher(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(Option<BenchResults>, bool, i32)> {
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
    let parsed = if results_file.exists() {
        Some(parsing::parse_bench_results_file(&results_file)?)
    } else {
        None
    };
    Ok((parsed, runner_output.success, runner_output.exit_code))
}

pub fn aggregate_runs(runs: &[BenchResults]) -> Result<BenchResults> {
    let first = runs
        .first()
        .ok_or_else(|| Error::internal_unexpected("cannot aggregate zero bench runs"))?;
    let mut metric_policies = BTreeMap::new();
    let mut grouped: BTreeMap<String, (BenchScenario, Vec<BenchScenario>)> = BTreeMap::new();

    for result in runs {
        if result.component_id != first.component_id {
            return Err(Error::validation_invalid_argument(
                "bench_results.component_id",
                format!(
                    "bench run component_id mismatch: expected `{}`, got `{}`",
                    first.component_id, result.component_id
                ),
                None,
                None,
            ));
        }
        if result.iterations != first.iterations {
            return Err(Error::validation_invalid_argument(
                "bench_results.iterations",
                format!(
                    "bench run iterations mismatch: expected `{}`, got `{}`",
                    first.iterations, result.iterations
                ),
                None,
                None,
            ));
        }
        for (key, policy) in &result.metric_policies {
            metric_policies
                .entry(key.clone())
                .or_insert_with(|| policy.clone());
        }
        for scenario in &result.scenarios {
            grouped
                .entry(scenario.id.clone())
                .and_modify(|(_, scenarios)| scenarios.push(scenario.clone()))
                .or_insert_with(|| (scenario.clone(), vec![scenario.clone()]));
        }
    }

    let scenarios = grouped
        .into_values()
        .map(|(template, scenarios)| aggregate_scenario(template, scenarios))
        .collect();

    Ok(BenchResults {
        component_id: first.component_id.clone(),
        iterations: first.iterations,
        scenarios,
        metric_policies,
    })
}

fn aggregate_scenario(mut template: BenchScenario, scenarios: Vec<BenchScenario>) -> BenchScenario {
    let mut metric_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for scenario in &scenarios {
        for (name, value) in &scenario.metrics.values {
            metric_values.entry(name.clone()).or_default().push(*value);
        }
    }

    let mut values = BTreeMap::new();
    let mut summary = BTreeMap::new();
    for (name, samples) in metric_values {
        values.insert(name.clone(), percentile(&samples, 50.0));
        summary.insert(name, distribution(&samples));
    }

    template.metrics = BenchMetrics {
        values,
        distributions: BTreeMap::new(),
    };
    template.memory = None;
    template.runs = Some(
        scenarios
            .iter()
            .map(|scenario| BenchRunSnapshot {
                metrics: scenario.metrics.clone(),
                memory: scenario.memory.clone(),
            })
            .collect(),
    );
    template.runs_summary = Some(summary);
    template
}

fn distribution(samples: &[f64]) -> BenchRunDistribution {
    let n = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / n;
    let variance = samples
        .iter()
        .map(|value| {
            let delta = value - mean;
            delta * delta
        })
        .sum::<f64>()
        / n;
    let stdev = variance.sqrt();
    let cv_pct = if mean == 0.0 {
        0.0
    } else {
        stdev / mean * 100.0
    };

    BenchRunDistribution {
        stdev,
        cv_pct,
        n: samples.len() as u64,
    }
}

fn percentile(samples: &[f64], pct: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = pct / 100.0 * (sorted.len() as f64 - 1.0);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = rank - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

/// Per-instance results filename within the run dir.
///
/// Single source of truth so the runner-side override and the parent
/// reader agree on the path. Single-instance runs keep the legacy
/// `bench-results.json` filename for backward compatibility with any
/// extension that hardcodes it.
fn instance_results_filename(instance_id: u32) -> String {
    format!("bench-results-i{}.json", instance_id)
}

/// Build the `ExtensionRunner` for a single bench invocation.
///
/// `instance` is `Some((id, total))` for multi-instance runs (each gets
/// its own results file + instance/concurrency env vars), or `None` for
/// the legacy single-instance path.
fn build_runner(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
    instance: Option<(u32, u32)>,
) -> Result<ExtensionRunner> {
    let mut runner = ExtensionRunner::for_context(execution_context.clone())
        .component(component.clone())
        .path_override(args.path_override.clone())
        .settings(&args.settings)
        .settings_json(&args.settings_json)
        .with_run_dir(run_dir)
        .env("HOMEBOY_BENCH_ITERATIONS", &args.iterations.to_string())
        .script_args(&args.passthrough_args);

    if !args.extra_workloads.is_empty() {
        runner = runner.env(
            "HOMEBOY_BENCH_EXTRA_WORKLOADS",
            &extra_workloads_env_value(&args.extra_workloads)?,
        );
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

fn extra_workloads_env_value(paths: &[PathBuf]) -> Result<String> {
    let joined = std::env::join_paths(paths)
        .map_err(|e| {
            Error::validation_invalid_argument(
                "bench_workloads",
                format!("bench workload path cannot be exported: {}", e),
                None,
                None,
            )
        })?
        .to_string_lossy()
        .to_string();
    Ok(joined)
}

/// Spawn N runner instances in parallel, wait for all, aggregate.
///
/// Returns `(merged_results, all_succeeded, exit_code)`. Per-instance
/// scenarios are merged with `:i<n>` suffixed IDs so each instance's
/// measurements stay distinct in the envelope and the baseline. If any
/// instance failed, the aggregate run reports failure with that
/// instance's exit code (first failure wins).
fn run_concurrent_instances(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(Option<BenchResults>, bool, i32)> {
    let concurrency = args.concurrency;
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

    let mut per_instance: Vec<(u32, crate::extension::RunnerOutput)> =
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
    for (_, output) in &per_instance {
        if !output.success {
            all_success = false;
            if first_failure_exit.is_none() {
                first_failure_exit = Some(output.exit_code);
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
    let mut component_id_seen: Option<String> = None;
    let mut iterations_seen: Option<u64> = None;
    let mut metric_policies_seen: std::collections::BTreeMap<String, parsing::BenchMetricPolicy> =
        std::collections::BTreeMap::new();

    for (instance_id, _) in &per_instance {
        let path = run_dir.step_file(&instance_results_filename(*instance_id));
        if !path.exists() {
            continue;
        }
        let parsed = match parsing::parse_bench_results_file(&path) {
            Ok(p) => p,
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
            scenarios: merged_scenarios,
            metric_policies: metric_policies_seen,
        })
    };

    Ok((merged, all_success, exit_code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn instance_results_filename_is_distinct_per_instance() {
        assert_eq!(instance_results_filename(0), "bench-results-i0.json");
        assert_eq!(instance_results_filename(7), "bench-results-i7.json");
        assert_ne!(instance_results_filename(0), instance_results_filename(1));
    }

    #[test]
    fn extra_workloads_env_value_joins_paths_for_runner_contract() {
        let paths = vec![
            PathBuf::from("/tmp/bench-one.php"),
            PathBuf::from("/tmp/bench-two.php"),
        ];

        assert_eq!(
            extra_workloads_env_value(&paths).unwrap(),
            "/tmp/bench-one.php:/tmp/bench-two.php"
        );
    }

    #[test]
    fn test_run_bench_list_workflow() {
        let result = BenchListWorkflowResult {
            component: "homeboy".to_string(),
            component_id: "homeboy".to_string(),
            count: 1,
            scenarios: vec![BenchScenario {
                id: "audit-self".to_string(),
                file: Some("src/bin/bench-audit-self.rs".to_string()),
                source: Some("in_tree".to_string()),
                default_iterations: Some(10),
                tags: Vec::new(),
                iterations: 0,
                metrics: parsing::BenchMetrics {
                    values: BTreeMap::new(),
                    distributions: BTreeMap::new(),
                },
                memory: None,
                runs: None,
                runs_summary: None,
            }],
        };

        assert_eq!(result.count, result.scenarios.len());
        assert_eq!(result.scenarios[0].iterations, 0);
        assert!(result.scenarios[0].metrics.values.is_empty());
        assert_eq!(result.scenarios[0].default_iterations, Some(10));
    }

    #[test]
    fn test_run_main_bench_workflow() {
        let run_dir = RunDir::create().expect("run dir");
        let err = run_main_bench_workflow(
            &Component::default(),
            &PathBuf::from("/tmp/homeboy"),
            BenchRunWorkflowArgs {
                component_label: "homeboy".to_string(),
                component_id: "homeboy".to_string(),
                path_override: None,
                settings: Vec::new(),
                settings_json: Vec::new(),
                iterations: 1,
                runs: 1,
                baseline_flags: BaselineFlags {
                    baseline: false,
                    ignore_baseline: true,
                    ratchet: false,
                },
                regression_threshold_percent: 5.0,
                json_summary: false,
                passthrough_args: Vec::new(),
                rig_id: None,
                shared_state: None,
                concurrency: 0,
                extra_workloads: Vec::new(),
            },
            &run_dir,
        )
        .expect_err("zero concurrency must fail before runner resolution");

        assert!(format!("{}", err).contains("concurrency"));
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/runs_flag_test.rs"]
mod runs_flag_test;
