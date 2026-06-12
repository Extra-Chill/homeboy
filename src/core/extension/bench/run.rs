//! Bench main workflow: invoke extension runner, load JSON, apply baseline.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use serde::Serialize;

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::invocation::InvocationRequirements;
use crate::core::engine::resource::{self, ExtensionChildResourceSummary};
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::bench::aggregate_runs;
use crate::core::extension::bench::artifact::BenchArtifact;
use crate::core::extension::bench::baseline::{self, BenchBaselineComparison};
use crate::core::extension::bench::diagnostic::{self, BenchDiagnostic};
use crate::core::extension::bench::failure_diagnostic::bench_failure_stderr_tail;
use crate::core::extension::bench::parsing::{
    self, BenchResults, BenchRunExecution, BenchRunMetadata, BenchRunnerMetadata, BenchScenario,
};
use crate::core::extension::bench::phase_events::BenchPhaseFailureClassification;
use crate::core::extension::bench::responsiveness::{
    memory_sample, read_responsiveness_summary, BenchFailureMemorySample,
    BenchResponsivenessSummary,
};
use crate::core::extension::bench::run_metadata::stamp_run_metadata;
use crate::core::extension::{
    build_scenario_runner, resolve_execution_context, ExtensionCapability,
    ExtensionExecutionContext, ExtensionRunner, ScenarioRunnerOptions,
};
use crate::core::gate::HomeboyGateResult;

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
    pub warmup_iterations: Option<u64>,
    pub execution: BenchRunExecution,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub json_summary: bool,
    pub ci_env: Vec<(String, String)>,
    pub passthrough_args: Vec<String>,
    /// Exact scenario ids selected by the CLI. Empty means run every
    /// discovered scenario.
    pub scenario_ids: Vec<String>,
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
    /// Rig-declared out-of-tree workloads to run alongside in-tree discovery.
    /// Exported to dispatchers as `HOMEBOY_BENCH_EXTRA_WORKLOADS`.
    pub extra_workloads: Vec<PathBuf>,
    /// Generic Homeboy isolation requirements for each child workload
    /// invocation. Rigs can use this for browser/server/wasm benchmarks without
    /// runner-specific namespace logic.
    pub invocation_requirements: InvocationRequirements,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    pub results: Option<BenchResults>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<HomeboyGateResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gate_failures: Vec<String>,
    pub baseline_comparison: Option<BenchBaselineComparison>,
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<BenchRunFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchRunFailure {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub exit_code: i32,
    pub stderr_tail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<BenchPhaseFailureClassification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responsiveness: Option<BenchResponsivenessSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_sample: Option<BenchFailureMemorySample>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct BenchListWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub passthrough_args: Vec<String>,
    pub scenario_ids: Vec<String>,
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
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    if component.has_script(ExtensionCapability::Bench) {
        let source_path = crate::core::extension::component_script::source_path(
            component,
            args.path_override.as_deref(),
        );
        let output = crate::core::extension::component_script::run_component_scripts_with_run_dir(
            component,
            ExtensionCapability::Bench,
            &source_path,
            run_dir,
            true,
            &[("HOMEBOY_BENCH_LIST_ONLY".to_string(), "1".to_string())],
            &args.passthrough_args,
        )?;
        ensure_bench_list_success(
            output.exit_code,
            output.success,
            &output.stdout,
            &output.stderr,
        )?;
        return bench_list_result(args.component_label, results_file, &args.scenario_ids);
    }

    let execution_context = resolve_execution_context(component, ExtensionCapability::Bench)?;
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
            warmup_iterations: None,
            execution: BenchRunExecution {
                runs: 1,
                concurrency: 1,
            },
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent: 0.0,
            json_summary: false,
            ci_env: Vec::new(),
            passthrough_args: args.passthrough_args,
            scenario_ids: args.scenario_ids.clone(),
            rig_id: None,
            shared_state: None,
            extra_workloads: args.extra_workloads,
            invocation_requirements: InvocationRequirements::default(),
        },
        run_dir,
        None,
    )?
    .env("HOMEBOY_BENCH_LIST_ONLY", "1")
    .run()?;

    ensure_bench_list_success(
        runner_output.exit_code,
        runner_output.success,
        &runner_output.stdout,
        &runner_output.stderr,
    )?;
    bench_list_result(args.component_label, results_file, &args.scenario_ids)
}

fn ensure_bench_list_success(
    exit_code: i32,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    if success {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "bench_list",
        format!("bench scenario discovery failed with exit code {exit_code}"),
        Some(format!("stdout:\n{stdout}\n\nstderr:\n{stderr}")),
        None,
    ))
}

fn bench_list_result(
    component_label: String,
    results_file: PathBuf,
    scenario_ids: &[String],
) -> Result<BenchListWorkflowResult> {
    let parsed = apply_scenario_filter(
        parsing::parse_bench_results_file_with_artifact_context(&results_file, None)?,
        scenario_ids,
    )?;
    let count = parsed.scenarios.len();

    Ok(BenchListWorkflowResult {
        component: component_label,
        component_id: parsed.component_id,
        scenarios: parsed.scenarios,
        count,
    })
}

fn apply_scenario_filter(
    mut results: BenchResults,
    scenario_ids: &[String],
) -> Result<BenchResults> {
    if scenario_ids.is_empty() {
        return Ok(results);
    }

    let discovered: Vec<String> = results.scenarios.iter().map(|s| s.id.clone()).collect();
    let missing: Vec<String> = scenario_ids
        .iter()
        .filter(|id| !discovered.contains(id))
        .cloned()
        .collect();

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "scenario",
            format!(
                "unknown bench scenario id(s): {}; discovered ids: {}",
                missing.join(", "),
                if discovered.is_empty() {
                    "<none>".to_string()
                } else {
                    discovered.join(", ")
                }
            ),
            Some(missing.join(", ")),
            Some(discovered),
        ));
    }

    results
        .scenarios
        .retain(|scenario| scenario_ids.contains(&scenario.id));
    Ok(results)
}

fn scenario_id_for_workload_path(path: &std::path::Path) -> String {
    let basename = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let name = basename
        .split_once(".bench.")
        .map(|(stem, _)| stem)
        .unwrap_or_else(|| {
            basename
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(&basename)
        });

    let mut slug = String::new();
    let mut prev_was_separator = true;
    let mut prev_was_lower_or_digit = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() && prev_was_lower_or_digit && !prev_was_separator {
            slug.push('-');
        }

        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_was_separator = false;
            prev_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else if !prev_was_separator {
            slug.push('-');
            prev_was_separator = true;
            prev_was_lower_or_digit = false;
        } else {
            prev_was_lower_or_digit = false;
        }
    }

    slug.trim_matches('-').to_string()
}

fn filter_extra_workloads_by_scenario_ids(
    workloads: &[PathBuf],
    scenario_ids: &[String],
) -> Vec<PathBuf> {
    if scenario_ids.is_empty() {
        return workloads.to_vec();
    }

    workloads
        .iter()
        .filter(|path| scenario_ids.contains(&scenario_id_for_workload_path(path)))
        .cloned()
        .collect()
}

fn parse_execution_results_file(
    results_file: &Path,
    scenario_ids: &[String],
    runner_success: bool,
    rig_id: Option<&str>,
) -> Result<Option<BenchResults>> {
    if !results_file.exists() {
        return Ok(None);
    }

    if runner_success {
        return Ok(Some(apply_scenario_filter(
            parsing::parse_bench_results_file_with_artifact_context_and_scenarios(
                results_file,
                rig_id,
                scenario_ids,
            )?,
            scenario_ids,
        )?));
    }

    Ok(
        parsing::parse_bench_results_file_with_artifact_context_and_scenarios(
            results_file,
            rig_id,
            scenario_ids,
        )
        .ok(),
    )
}

fn failure_scenario_id(scenario_ids: &[String]) -> Option<String> {
    if scenario_ids.len() == 1 {
        Some(scenario_ids[0].clone())
    } else {
        None
    }
}

fn workload_status_failures(results: &BenchResults) -> Vec<String> {
    results
        .scenarios
        .iter()
        .filter_map(|scenario| {
            if !scenario.passed {
                return Some(format!(
                    "bench scenario `{}` reported passed=false",
                    scenario.id
                ));
            }

            let failed = child_status_count(scenario, "failed").unwrap_or(0.0);
            let unsupported = child_status_count(scenario, "unsupported").unwrap_or(0.0);
            let warning = child_status_count(scenario, "warning").unwrap_or(0.0);
            let passed = child_status_count(scenario, "passed");
            let total = scenario.metrics.get("adapter_count");

            if failed > 0.0 || unsupported > 0.0 || warning > 0.0 {
                return Some(format!(
                    "bench scenario `{}` reported failed_count={} unsupported_count={} warning_count={} passed_count={}",
                    scenario.id,
                    format_count(failed),
                    format_count(unsupported),
                    format_count(warning),
                    passed
                        .map(format_count)
                        .unwrap_or_else(|| "<unknown>".to_string())
                ));
            }

            if passed == Some(0.0) && total.is_some_and(|count| count > 0.0) {
                return Some(format!(
                    "bench scenario `{}` reported no passing children out of adapter_count={} (failed_count={} unsupported_count={} warning_count={})",
                    scenario.id,
                    format_count(total.unwrap_or(0.0)),
                    format_count(failed),
                    format_count(unsupported),
                    format_count(warning)
                ));
            }

            None
        })
        .collect()
}

fn child_status_count(scenario: &BenchScenario, status: &str) -> Option<f64> {
    scenario
        .metrics
        .get(&format!("{status}_count"))
        .or_else(|| metadata_result_count(scenario, status))
}

fn metadata_result_count(scenario: &BenchScenario, status: &str) -> Option<f64> {
    scenario
        .metadata
        .get("result_counts")?
        .as_object()?
        .get(status)?
        .as_f64()
}

fn format_count(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

fn discover_bench_scenarios(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchResults> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    if results_file.exists() {
        std::fs::remove_file(&results_file).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to clear previous bench discovery results file {}: {}",
                    results_file.display(),
                    e
                ),
                Some("bench.discovery.results_file".to_string()),
            )
        })?;
    }

    let runner_output = build_runner(execution_context, component, args, run_dir, None)?
        .env("HOMEBOY_BENCH_LIST_ONLY", "1")
        .run()?;

    if !runner_output.success {
        return Err(Error::validation_invalid_argument(
            "scenario",
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

    parsing::parse_bench_results_file_with_artifact_context(&results_file, None)
}

fn validate_bench_run_args(args: &BenchRunWorkflowArgs) -> Result<()> {
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

fn require_positive(name: &str, value: u64) -> Result<()> {
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
    source_path: &PathBuf,
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
        let script_output =
            crate::core::extension::component_script::run_component_scripts_with_run_dir(
                component,
                ExtensionCapability::Bench,
                source_path,
                run_dir,
                true,
                &bench_component_script_env(&args),
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
        let gate_failures = parsed
            .as_mut()
            .map(super::gate::evaluate_gates)
            .unwrap_or_default();
        let gate_results = parsed
            .as_ref()
            .map(super::gate::normalized_gate_results)
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
        .map(super::gate::evaluate_gates)
        .unwrap_or_default();
    if let Some(results) = parsed.as_ref() {
        gate_failures.extend(workload_status_failures(results));
    }
    let gate_results = parsed
        .as_ref()
        .map(super::gate::normalized_gate_results)
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

fn bench_component_script_env(args: &BenchRunWorkflowArgs) -> Vec<(String, String)> {
    let mut env = vec![
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
    ];
    env.extend(args.ci_env.iter().cloned());
    env
}

fn format_diagnostic_hint(diagnostic: &BenchDiagnostic) -> String {
    match diagnostic.message.as_deref() {
        Some(message) => format!("Diagnostic `{}`: {}", diagnostic.class, message),
        None => format!("Diagnostic `{}`", diagnostic.class),
    }
}

fn run_sequential_runs(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(
    Option<BenchResults>,
    bool,
    i32,
    Option<String>,
    Option<BenchFailureMemorySample>,
    Option<u128>,
)> {
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

fn run_single_dispatcher(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(
    Option<BenchResults>,
    bool,
    i32,
    Option<String>,
    Option<BenchFailureMemorySample>,
    Option<u128>,
)> {
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

fn clear_responsiveness_file(run_dir: &RunDir) -> Result<()> {
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

fn classify_bench_failure(
    success: bool,
    exit_code: i32,
    stderr: &str,
    responsiveness: Option<&BenchResponsivenessSummary>,
) -> Option<BenchPhaseFailureClassification> {
    if success {
        return None;
    }

    if responsiveness.is_some_and(BenchResponsivenessSummary::responsiveness_lost) {
        return Some(classification(
            "responsiveness_loss",
            "responsiveness",
            "missed_ping",
            responsiveness.and_then(|summary| {
                summary.last_ping_at.as_ref().map(|last| {
                    format!(
                        "UI responsiveness ping gap exceeded {}ms; last ping at {}",
                        summary.missed_ping_window_ms, last
                    )
                })
            }),
        ));
    }

    let stderr_lower = stderr.to_ascii_lowercase();
    if exit_code == 124 || stderr_lower.contains("timeout") || stderr_lower.contains("timed out") {
        return Some(classification("timeout", "bench", "timeout", None));
    }

    Some(classification("assertion_failure", "bench", "failed", None))
}

fn classification(
    kind: &str,
    phase: &str,
    status: &str,
    message: Option<String>,
) -> BenchPhaseFailureClassification {
    BenchPhaseFailureClassification {
        kind: kind.to_string(),
        phase: phase.to_string(),
        status: status.to_string(),
        message,
    }
}

fn bench_progress_enabled() -> bool {
    match std::env::var("HOMEBOY_BENCH_PROGRESS") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => std::env::var_os("CI").is_none(),
    }
}

fn bench_progress_env_value() -> &'static str {
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
fn run_concurrent_instances(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(
    Option<BenchResults>,
    bool,
    i32,
    Option<String>,
    Option<BenchFailureMemorySample>,
    Option<u128>,
)> {
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

fn attach_memory_timeline_artifacts(
    results: &mut BenchResults,
    child_resource: Option<&ExtensionChildResourceSummary>,
    run_dir: &RunDir,
    suffix: Option<&str>,
) -> Result<()> {
    let Some(child_resource) = child_resource else {
        return Ok(());
    };
    let Some((
        json_filename,
        csv_filename,
        peak_rss_bytes,
        peak_rss_mb,
        sample_count,
        peak_child_count,
        peak_at_ms,
    )) = preserve_memory_timeline_artifacts(Some(child_resource), run_dir, suffix)?
    else {
        return Ok(());
    };

    let memory_metadata = serde_json::json!({
        "peak_rss_bytes": peak_rss_bytes,
        "peak_rss_mb": peak_rss_mb,
        "peak_at_ms": peak_at_ms,
        "peak_child_count": peak_child_count,
        "sample_count": sample_count,
        "timeline_json": json_filename,
        "timeline_csv": csv_filename,
    });
    results
        .metadata
        .insert("memory_timeline".to_string(), memory_metadata);

    let phase_resources = phase_child_resources(run_dir);
    let phase_memory = if phase_resources.is_empty() {
        None
    } else {
        Some(preserve_phase_memory_timeline_artifacts(
            &phase_resources,
            run_dir,
            suffix,
        )?)
    };
    let phase_metrics = phase_memory
        .as_ref()
        .map(|phase_memory| phase_memory.metrics.clone())
        .unwrap_or_default();
    if let Some(phase_memory) = phase_memory.as_ref() {
        results.metadata.insert(
            "phase_memory".to_string(),
            serde_json::json!({
                "phases": phase_memory.phases,
                "timeline_json": phase_memory.json_filename,
                "timeline_csv": phase_memory.csv_filename,
                "sample_count": phase_memory.sample_count,
            }),
        );
    }
    results
        .metric_groups
        .entry("memory".to_string())
        .or_default()
        .extend([
            ("peak_rss_mb".to_string(), peak_rss_mb),
            ("peak_child_count".to_string(), peak_child_count as f64),
            ("sample_count".to_string(), sample_count as f64),
            ("peak_at_ms".to_string(), peak_at_ms as f64),
        ]);
    results
        .metric_groups
        .entry("memory".to_string())
        .or_default()
        .extend(phase_metrics.clone());

    for scenario in &mut results.scenarios {
        scenario
            .metrics
            .values
            .insert("peak_rss_mb".to_string(), peak_rss_mb);
        scenario
            .metrics
            .values
            .insert("peak_child_count".to_string(), peak_child_count as f64);
        scenario
            .metrics
            .values
            .insert("memory_sample_count".to_string(), sample_count as f64);
        scenario.metrics.values.extend(phase_metrics.clone());
        scenario.artifacts.insert(
            "memory_timeline_json".to_string(),
            BenchArtifact {
                path: Some(json_filename.clone()),
                artifact_type: Some("file".to_string()),
                kind: Some("bench_memory_timeline".to_string()),
                label: Some("Bench memory timeline (JSON)".to_string()),
                ..BenchArtifact::default()
            },
        );
        scenario.artifacts.insert(
            "memory_timeline_csv".to_string(),
            BenchArtifact {
                path: Some(csv_filename.clone()),
                artifact_type: Some("file".to_string()),
                kind: Some("bench_memory_timeline".to_string()),
                label: Some("Bench memory timeline (CSV)".to_string()),
                ..BenchArtifact::default()
            },
        );
        if let Some(phase_memory) = phase_memory.as_ref() {
            scenario.artifacts.insert(
                "phase_memory_timeline_json".to_string(),
                BenchArtifact {
                    path: Some(phase_memory.json_filename.clone()),
                    artifact_type: Some("file".to_string()),
                    kind: Some("bench_memory_timeline".to_string()),
                    label: Some("Bench phase memory timeline (JSON)".to_string()),
                    ..BenchArtifact::default()
                },
            );
            scenario.artifacts.insert(
                "phase_memory_timeline_csv".to_string(),
                BenchArtifact {
                    path: Some(phase_memory.csv_filename.clone()),
                    artifact_type: Some("file".to_string()),
                    kind: Some("bench_memory_timeline".to_string()),
                    label: Some("Bench phase memory timeline (CSV)".to_string()),
                    ..BenchArtifact::default()
                },
            );
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct PhaseMemoryArtifacts {
    json_filename: String,
    csv_filename: String,
    phases: BTreeMap<String, serde_json::Value>,
    metrics: BTreeMap<String, f64>,
    sample_count: usize,
}

fn phase_child_resources(run_dir: &RunDir) -> Vec<ExtensionChildResourceSummary> {
    resource::read_extension_child_resources(run_dir)
        .into_iter()
        .filter(|summary| {
            summary
                .phase
                .as_deref()
                .is_some_and(|phase| !phase.is_empty())
        })
        .collect()
}

fn preserve_phase_memory_timeline_artifacts(
    phase_resources: &[ExtensionChildResourceSummary],
    run_dir: &RunDir,
    suffix: Option<&str>,
) -> Result<PhaseMemoryArtifacts> {
    let artifact_stem = match suffix {
        Some(suffix) => format!("bench-memory-timeline-phases-{suffix}"),
        None => "bench-memory-timeline-phases".to_string(),
    };
    let json_filename = format!("{artifact_stem}.json");
    let csv_filename = format!("{artifact_stem}.csv");
    let json_path = run_dir.step_file(&json_filename);
    let csv_path = run_dir.step_file(&csv_filename);

    let mut phases = BTreeMap::new();
    let mut metrics = BTreeMap::new();
    let mut sample_count = 0;
    for resource in phase_resources {
        let Some(phase) = resource.phase.as_deref().filter(|phase| !phase.is_empty()) else {
            continue;
        };
        let peak_rss_bytes = resource.sampled_peak_rss_bytes.unwrap_or(0);
        let peak_rss_mb = bytes_to_mb(peak_rss_bytes);
        sample_count += resource.samples.len();
        phases.insert(
            phase.to_string(),
            serde_json::json!({
                "peak_rss_bytes": peak_rss_bytes,
                "peak_rss_mb": peak_rss_mb,
                "peak_at_ms": resource.sampled_peak_at_ms.unwrap_or(0),
                "peak_child_count": resource.sampled_peak_child_count.unwrap_or(0),
                "sample_count": resource.samples.len(),
                "root_pid": resource.child.root_pid,
                "command_label": resource.child.command_label,
            }),
        );
        metrics.insert(
            format!("peak_{}_rss_mb", metric_phase_slug(phase)),
            peak_rss_mb,
        );
    }

    let json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "homeboy/bench-memory-timeline/v1",
        "phases": phases.clone(),
        "sample_count": sample_count,
        "resources": phase_resources,
    }))
    .map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize bench phase memory timeline".to_string()),
        )
    })?;
    fs::write(&json_path, json).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench phase memory timeline {}: {}",
                json_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    fs::write(&csv_path, phase_memory_timeline_csv(phase_resources)).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench phase memory timeline CSV {}: {}",
                csv_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    Ok(PhaseMemoryArtifacts {
        json_filename,
        csv_filename,
        phases,
        metrics,
        sample_count,
    })
}

type MemoryTimelineArtifacts = (String, String, u64, f64, usize, usize, u128);

fn preserve_memory_timeline_artifacts(
    child_resource: Option<&ExtensionChildResourceSummary>,
    run_dir: &RunDir,
    suffix: Option<&str>,
) -> Result<Option<MemoryTimelineArtifacts>> {
    let Some(child_resource) = child_resource else {
        return Ok(None);
    };
    if child_resource.samples.is_empty() {
        return Ok(None);
    }

    let artifact_stem = match suffix {
        Some(suffix) => format!("bench-memory-timeline-{suffix}"),
        None => "bench-memory-timeline".to_string(),
    };
    let json_filename = format!("{artifact_stem}.json");
    let csv_filename = format!("{artifact_stem}.csv");
    let json_path = run_dir.step_file(&json_filename);
    let csv_path = run_dir.step_file(&csv_filename);

    let peak_rss_bytes = child_resource.sampled_peak_rss_bytes.unwrap_or(0);
    let peak_rss_mb = bytes_to_mb(peak_rss_bytes);
    let sample_count = child_resource.samples.len();
    let peak_child_count = child_resource.sampled_peak_child_count.unwrap_or(0);
    let peak_at_ms = child_resource.sampled_peak_at_ms.unwrap_or(0);

    let json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "homeboy/bench-memory-timeline/v1",
        "root_pid": child_resource.child.root_pid,
        "command_label": child_resource.child.command_label,
        "started_at": child_resource.started_at,
        "finished_at": child_resource.finished_at,
        "duration_ms": child_resource.duration_ms,
        "peak_rss_bytes": peak_rss_bytes,
        "peak_rss_mb": peak_rss_mb,
        "peak_at_ms": peak_at_ms,
        "peak_child_count": peak_child_count,
        "sample_count": sample_count,
        "samples": child_resource.samples,
    }))
    .map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize bench memory timeline".to_string()),
        )
    })?;
    fs::write(&json_path, json).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench memory timeline {}: {}",
                json_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    fs::write(&csv_path, memory_timeline_csv(child_resource)).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write bench memory timeline CSV {}: {}",
                csv_path.display(),
                e
            ),
            Some("bench.memory_timeline".to_string()),
        )
    })?;

    Ok(Some((
        json_filename,
        csv_filename,
        peak_rss_bytes,
        peak_rss_mb,
        sample_count,
        peak_child_count,
        peak_at_ms,
    )))
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

fn memory_timeline_csv(child_resource: &ExtensionChildResourceSummary) -> String {
    let mut csv = String::from(
        "timestamp,elapsed_ms,phase,root_pid,rss_bytes,rss_mb,cpu_percent,child_count,process_count\n",
    );
    for sample in &child_resource.samples {
        csv.push_str(&format!(
            "{},{},{},{},{},{:.6},{:.3},{},{}\n",
            sample.timestamp,
            sample.elapsed_ms,
            csv_field(sample.phase.as_deref().unwrap_or("")),
            sample.root_pid,
            sample.rss_bytes,
            bytes_to_mb(sample.rss_bytes),
            sample.cpu_percent,
            sample.child_count,
            sample.processes.len(),
        ));
    }
    csv
}

fn phase_memory_timeline_csv(phase_resources: &[ExtensionChildResourceSummary]) -> String {
    let mut csv = String::from(
        "timestamp,elapsed_ms,phase,root_pid,rss_bytes,rss_mb,cpu_percent,child_count,process_count\n",
    );
    for resource in phase_resources {
        for sample in &resource.samples {
            let phase = sample
                .phase
                .as_deref()
                .or(resource.phase.as_deref())
                .unwrap_or("");
            csv.push_str(&format!(
                "{},{},{},{},{},{:.6},{:.3},{},{}\n",
                sample.timestamp,
                sample.elapsed_ms,
                csv_field(phase),
                sample.root_pid,
                sample.rss_bytes,
                bytes_to_mb(sample.rss_bytes),
                sample.cpu_percent,
                sample.child_count,
                sample.processes.len(),
            ));
        }
    }
    csv
}

fn metric_phase_slug(phase: &str) -> String {
    let slug = phase
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if slug.is_empty() {
        "phase".to_string()
    } else {
        slug
    }
}

fn csv_field(value: &str) -> String {
    if value.contains(&[',', '"', '\n', '\r'][..]) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::resource::{
        ChildProcessIdentity, ExtensionChildProcessSample, ExtensionChildResourceSample,
        ExtensionChildResourceSummary,
    };
    use crate::core::extension::path_list_env_value;
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
            path_list_env_value("bench_workloads", &paths).unwrap(),
            "/tmp/bench-one.php:/tmp/bench-two.php"
        );
    }

    #[test]
    fn filter_extra_workloads_by_selected_scenario_ids_matches_runner_slugs() {
        let workloads = vec![
            PathBuf::from("/tmp/bench/studio-agent-runtime.bench.mjs"),
            PathBuf::from("/tmp/bench/studio-bfb-write-path.bench.js"),
            PathBuf::from("/tmp/bench/WpAdminLoad.php"),
        ];

        let filtered = filter_extra_workloads_by_scenario_ids(
            &workloads,
            &[
                "studio-agent-runtime".to_string(),
                "wp-admin-load".to_string(),
            ],
        );

        assert_eq!(
            filtered,
            vec![
                PathBuf::from("/tmp/bench/studio-agent-runtime.bench.mjs"),
                PathBuf::from("/tmp/bench/WpAdminLoad.php"),
            ]
        );
    }

    #[test]
    fn failed_execution_parse_ignores_unselected_duplicate_scenario_ids() {
        let run_dir = RunDir::create().expect("run dir");
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        fs::write(
            &results_file,
            r#"{
                "component_id": "woocommerce",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "rest-product-batch-import",
                        "file": "tests/bench/rest-product-batch-import.php",
                        "iterations": 1,
                        "metrics": { "p95_ms": 5.0 }
                    },
                    {
                        "id": "checkout-concurrent-create-order",
                        "file": "tests/bench/checkout-concurrent-create-order.php",
                        "iterations": 1,
                        "metrics": { "p95_ms": 10.0 }
                    },
                    {
                        "id": "checkout-concurrent-create-order",
                        "iterations": 1,
                        "metrics": { "p95_ms": 20.0 }
                    }
                ]
            }"#,
        )
        .expect("write results file");

        let parsed = parse_execution_results_file(
            &results_file,
            &["rest-product-batch-import".to_string()],
            false,
            None,
        )
        .expect("failed runner parse should not validate unselected duplicates")
        .expect("parsed results");

        assert_eq!(parsed.scenarios.len(), 1);
        assert_eq!(parsed.scenarios[0].id, "rest-product-batch-import");
    }

    #[test]
    fn workload_status_failures_catches_failed_and_unsupported_children() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 2,
                        "passed_count": 0,
                        "failed_count": 1,
                        "unsupported_count": 1
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        let failures = workload_status_failures(&results);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("workflow-bench"));
        assert!(failures[0].contains("failed_count=1"));
        assert!(failures[0].contains("unsupported_count=1"));
    }

    #[test]
    fn workload_status_failures_catches_warning_children() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 1,
                        "passed_count": 0,
                        "failed_count": 0,
                        "unsupported_count": 0,
                        "warning_count": 1
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        let failures = workload_status_failures(&results);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("workflow-bench"));
        assert!(failures[0].contains("warning_count=1"));
        assert!(failures[0].contains("passed_count=0"));
    }

    #[test]
    fn workload_status_failures_catches_metadata_warning_result_counts() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 1,
                        "passed_count": 0,
                        "failed_count": 0,
                        "unsupported_count": 0
                    },
                    "metadata": {
                        "result_counts": {
                            "warning": 1
                        }
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        let failures = workload_status_failures(&results);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("workflow-bench"));
        assert!(failures[0].contains("warning_count=1"));
        assert!(failures[0].contains("passed_count=0"));
    }

    #[test]
    fn workload_status_failures_allows_clean_child_counters() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 2,
                        "passed_count": 2,
                        "failed_count": 0,
                        "unsupported_count": 0
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        assert!(workload_status_failures(&results).is_empty());
    }

    #[test]
    fn attach_memory_timeline_adds_metrics_and_artifacts() {
        let run_dir = RunDir::create().expect("run dir");
        let mut results = BenchResults {
            component_id: "homeboy".to_string(),
            iterations: 1,
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
            budget_findings: Vec::new(),
            scenarios: vec![BenchScenario {
                id: "cold-start".to_string(),
                file: None,
                source: None,
                default_iterations: None,
                tags: Vec::new(),
                iterations: 1,
                metrics: parsing::BenchMetrics::default(),
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
                runs: None,
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        };
        let child = ExtensionChildResourceSummary {
            child: ChildProcessIdentity {
                root_pid: 42,
                command_label: "bench fixture".to_string(),
            },
            phase: None,
            started_at: "2026-06-08T00:00:00Z".to_string(),
            finished_at: "2026-06-08T00:00:01Z".to_string(),
            duration_ms: 1000,
            sampled_peak_rss_bytes: Some(2 * 1024 * 1024),
            sampled_peak_cpu_percent: Some(3.5),
            sampled_peak_at_ms: Some(100),
            sampled_peak_child_count: Some(1),
            samples: vec![ExtensionChildResourceSample {
                elapsed_ms: 100,
                timestamp: "2026-06-08T00:00:00.100Z".to_string(),
                root_pid: 42,
                phase: None,
                rss_bytes: 2 * 1024 * 1024,
                cpu_percent: 3.5,
                child_count: 1,
                processes: vec![ExtensionChildProcessSample {
                    pid: 42,
                    parent_pid: 1,
                    rss_bytes: 2 * 1024 * 1024,
                    cpu_percent: 3.5,
                    command: "bench".to_string(),
                }],
            }],
            warnings: Vec::new(),
        };
        let mut phase_child = child.clone();
        phase_child.child.root_pid = 43;
        phase_child.child.command_label = "npm install".to_string();
        phase_child.phase = Some("install".to_string());
        phase_child.sampled_peak_rss_bytes = Some(3 * 1024 * 1024);
        phase_child.samples[0].root_pid = 43;
        phase_child.samples[0].phase = Some("install".to_string());
        phase_child.samples[0].rss_bytes = 3 * 1024 * 1024;
        resource::record_extension_child_resource(run_dir.path(), &phase_child)
            .expect("record phase resource");

        attach_memory_timeline_artifacts(&mut results, Some(&child), &run_dir, None)
            .expect("attach memory timeline");

        assert_eq!(results.metric_groups["memory"]["peak_rss_mb"], 2.0);
        assert_eq!(results.metric_groups["memory"]["peak_install_rss_mb"], 3.0);
        assert_eq!(results.scenarios[0].metrics.values["peak_rss_mb"], 2.0);
        assert_eq!(
            results.scenarios[0].metrics.values["peak_install_rss_mb"],
            3.0
        );
        assert_eq!(
            results.metadata["phase_memory"]["phases"]["install"]["peak_rss_mb"].as_f64(),
            Some(3.0)
        );
        assert!(results.scenarios[0]
            .artifacts
            .contains_key("memory_timeline_json"));
        assert!(results.scenarios[0]
            .artifacts
            .contains_key("phase_memory_timeline_json"));
        assert!(run_dir.step_file("bench-memory-timeline.json").is_file());
        assert!(run_dir.step_file("bench-memory-timeline.csv").is_file());
        assert!(run_dir
            .step_file("bench-memory-timeline-phases.json")
            .is_file());
        assert!(run_dir
            .step_file("bench-memory-timeline-phases.csv")
            .is_file());

        run_dir.cleanup();
    }

    #[test]
    fn classifies_failed_run_with_missed_pings_as_responsiveness_loss() {
        let responsiveness = BenchResponsivenessSummary {
            missed_ping_count: 2,
            max_ping_gap_ms: 15_000,
            last_ping_at: Some("2026-06-08T00:00:00Z".to_string()),
            ping_count: 3,
            missed_ping_window_ms: 5_000,
        };

        let classification =
            classify_bench_failure(false, 1, "", Some(&responsiveness)).expect("classification");

        assert_eq!(classification.kind, "responsiveness_loss");
        assert_eq!(classification.phase, "responsiveness");
        assert_eq!(classification.status, "missed_ping");
        assert!(classification
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("last ping"));
    }

    #[test]
    fn classifies_timeout_and_assertion_failures() {
        let timeout = classify_bench_failure(false, 124, "", None).expect("timeout");
        assert_eq!(timeout.kind, "timeout");

        let assertion = classify_bench_failure(false, 1, "expected button to be visible", None)
            .expect("assertion");
        assert_eq!(assertion.kind, "assertion_failure");
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
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
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
                warmup_iterations: None,
                execution: BenchRunExecution {
                    runs: 1,
                    concurrency: 0,
                },
                baseline_flags: BaselineFlags {
                    baseline: false,
                    ignore_baseline: true,
                    ratchet: false,
                },
                regression_threshold_percent: 5.0,
                json_summary: false,
                ci_env: Vec::new(),
                passthrough_args: Vec::new(),
                scenario_ids: Vec::new(),
                rig_id: None,
                shared_state: None,
                extra_workloads: Vec::new(),
                invocation_requirements: InvocationRequirements::default(),
            },
            &run_dir,
        )
        .expect_err("zero concurrency must fail before runner resolution");

        assert!(format!("{}", err).contains("concurrency"));
    }
}
