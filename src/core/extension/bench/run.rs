//! Bench main workflow: invoke extension runner, load JSON, apply baseline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::component::Component;
use crate::engine::baseline::BaselineFlags;
use crate::engine::run_dir::{self, RunDir};
use crate::error::{Error, Result};
use crate::extension::bench::aggregate_runs;
use crate::extension::bench::baseline::{self, BenchBaselineComparison};
use crate::extension::bench::parsing::{
    self, BenchResults, BenchRunMetadata, BenchRunnerMetadata, BenchScenario, BenchWorkloadMetadata,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gate_failures: Vec<String>,
    pub baseline_comparison: Option<BenchBaselineComparison>,
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<BenchRunFailure>,
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
            scenario_ids: Vec::new(),
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

    let parsed = apply_scenario_filter(
        parsing::parse_bench_results_file(&results_file)?,
        &args.scenario_ids,
    )?;
    let count = parsed.scenarios.len();

    Ok(BenchListWorkflowResult {
        component: args.component_label,
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

    let mut discovery_args = args.clone();
    discovery_args.scenario_ids.clear();

    let runner_output = build_runner(execution_context, component, &discovery_args, run_dir, None)?
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

    parsing::parse_bench_results_file(&results_file)
}

fn validate_bench_run_args(args: &BenchRunWorkflowArgs) -> Result<()> {
    require_positive("concurrency", args.concurrency as u64)?;
    require_positive("runs", args.runs)?;
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

    let execution_context = resolve_execution_context(component, ExtensionCapability::Bench)?;

    let mut execution_args = args.clone();
    if !args.scenario_ids.is_empty() {
        let discovered = discover_bench_scenarios(&execution_context, component, &args, run_dir)?;
        apply_scenario_filter(discovered, &args.scenario_ids)?;
        execution_args.extra_workloads =
            filter_extra_workloads_by_scenario_ids(&args.extra_workloads, &args.scenario_ids);
    }

    let (mut parsed, runner_success, runner_exit_code, failure_stderr_tail) =
        if execution_args.runs > 1 {
            run_sequential_runs(&execution_context, component, &execution_args, run_dir)?
        } else if execution_args.concurrency <= 1 {
            let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
            let runner_output = build_runner(
                &execution_context,
                component,
                &execution_args,
                run_dir,
                None,
            )?
            .run()?;
            let parsed = if results_file.exists() {
                parsing::parse_bench_results_file(&results_file)
                    .ok()
                    .map(|results| apply_scenario_filter(results, &execution_args.scenario_ids))
                    .transpose()?
            } else {
                None
            };
            let failure_stderr_tail = if !runner_output.success {
                Some(stderr_tail(&runner_output.stderr))
            } else {
                None
            };
            (
                parsed,
                runner_output.success,
                runner_output.exit_code,
                failure_stderr_tail,
            )
        } else {
            run_concurrent_instances(&execution_context, component, &execution_args, run_dir)?
        };

    if let Some(results) = parsed.as_mut() {
        stamp_run_metadata(results, &execution_context, component, &args, &started_at);
    }

    let gate_failures = parsed
        .as_mut()
        .map(parsing::evaluate_gates)
        .unwrap_or_default();
    let gates_passed = gate_failures.is_empty();

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
    hints.push("Full options: homeboy docs commands/bench".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    let exit_code = if runner_exit_code != 0 {
        runner_exit_code
    } else if !gates_passed {
        1
    } else {
        baseline_exit_override.unwrap_or(0)
    };
    let failure = if parsed.is_none() && !runner_success {
        failure_stderr_tail.map(|stderr_tail| BenchRunFailure {
            component_id: args.component_id.clone(),
            component_path: args
                .path_override
                .clone()
                .or_else(|| Some(component.local_path.clone())),
            scenario_id: None,
            exit_code: runner_exit_code,
            stderr_tail,
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
        gate_failures,
        baseline_comparison,
        hints,
        failure,
    })
}

fn stderr_tail(stderr: &str) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = stderr.lines().collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

fn stamp_run_metadata(
    results: &mut BenchResults,
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    started_at: &str,
) {
    let mut workloads = workload_metadata(&results.scenarios, component, &args.extra_workloads);
    workloads.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.path.cmp(&b.path)));

    results.run_metadata = Some(BenchRunMetadata {
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        started_at: started_at.to_string(),
        shared_state: args
            .shared_state
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        iterations: args.iterations,
        runs: args.runs,
        concurrency: args.concurrency,
        warmup_iterations: bench_warmup_iterations(),
        selected_scenarios: args.scenario_ids.clone(),
        env_overrides: bench_env_overrides(),
        workloads,
        runner: Some(BenchRunnerMetadata {
            extension: execution_context.extension_id.clone(),
            path: execution_context
                .extension_path
                .to_string_lossy()
                .to_string(),
            source_revision: source_revision_at(&execution_context.extension_path),
        }),
    });
}

fn workload_metadata(
    scenarios: &[BenchScenario],
    component: &Component,
    extra_workloads: &[PathBuf],
) -> Vec<BenchWorkloadMetadata> {
    let mut workloads = Vec::new();
    let mut seen_paths = BTreeSet::new();

    for scenario in scenarios {
        let resolved = scenario
            .file
            .as_deref()
            .map(|path| resolve_workload_path(path, component));
        if let Some(path) = &resolved {
            seen_paths.insert(path.to_string_lossy().to_string());
        }
        workloads.push(BenchWorkloadMetadata {
            id: scenario.id.clone(),
            source: scenario.source.clone(),
            path: resolved
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            sha256: resolved.as_deref().and_then(sha256_file),
        });
    }

    for path in extra_workloads {
        let path_string = path.to_string_lossy().to_string();
        if !seen_paths.insert(path_string.clone()) {
            continue;
        }
        workloads.push(BenchWorkloadMetadata {
            id: path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("extra-workload")
                .to_string(),
            source: Some("rig".to_string()),
            path: Some(path_string),
            sha256: sha256_file(path),
        });
    }

    workloads
}

fn resolve_workload_path(path: &str, component: &Component) -> PathBuf {
    let workload_path = PathBuf::from(path);
    if workload_path.is_absolute() {
        workload_path
    } else {
        PathBuf::from(&component.local_path).join(workload_path)
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let hash = Sha256::digest(&bytes);
    Some(hash.iter().map(|byte| format!("{:02x}", byte)).collect())
}

fn bench_warmup_iterations() -> Option<u64> {
    std::env::var("HOMEBOY_BENCH_WARMUP_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
}

fn bench_env_overrides() -> BTreeMap<String, String> {
    bench_env_overrides_from_iter(std::env::vars())
}

fn bench_env_overrides_from_iter<I, K, V>(vars: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    vars.into_iter()
        .filter_map(|(key, value)| {
            let key = key.into();
            if key.starts_with("HOMEBOY_BENCH_") && !is_secret_like_env_key(&key) {
                Some((key, value.into()))
            } else {
                None
            }
        })
        .collect()
}

fn is_secret_like_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "AUTH",
        "API_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn source_revision_at(path: &Path) -> Option<String> {
    git_short_revision_at(path).or_else(|| {
        std::fs::read_to_string(path.join(".source-revision"))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn git_short_revision_at(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!revision.is_empty()).then_some(revision)
}

fn run_sequential_runs(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(Option<BenchResults>, bool, i32, Option<String>)> {
    let mut parsed_runs = Vec::new();
    let mut all_success = true;
    let mut first_failure_exit: Option<i32> = None;
    let mut first_failure_stderr_tail: Option<String> = None;

    for _ in 0..args.runs {
        let (parsed, success, exit_code, stderr_tail) = if args.concurrency <= 1 {
            run_single_dispatcher(execution_context, component, args, run_dir)?
        } else {
            run_concurrent_instances(execution_context, component, args, run_dir)?
        };
        if !success {
            all_success = false;
            if first_failure_exit.is_none() {
                first_failure_exit = Some(exit_code);
            }
            if first_failure_stderr_tail.is_none() {
                first_failure_stderr_tail = stderr_tail;
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

    Ok((merged, all_success, exit_code, first_failure_stderr_tail))
}

fn run_single_dispatcher(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<(Option<BenchResults>, bool, i32, Option<String>)> {
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
        Some(apply_scenario_filter(
            parsing::parse_bench_results_file(&results_file)?,
            &args.scenario_ids,
        )?)
    } else {
        None
    };
    let failure_stderr_tail = if !runner_output.success {
        Some(stderr_tail(&runner_output.stderr))
    } else {
        None
    };
    Ok((
        parsed,
        runner_output.success,
        runner_output.exit_code,
        failure_stderr_tail,
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
) -> Result<(Option<BenchResults>, bool, i32, Option<String>)> {
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
    let mut first_failure_stderr_tail: Option<String> = None;
    for (_, output) in &per_instance {
        if !output.success {
            all_success = false;
            if first_failure_exit.is_none() {
                first_failure_exit = Some(output.exit_code);
            }
            if first_failure_stderr_tail.is_none() {
                first_failure_stderr_tail = Some(stderr_tail(&output.stderr));
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
        let parsed = match parsing::parse_bench_results_file(&path)
            .and_then(|results| apply_scenario_filter(results, &args.scenario_ids))
        {
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
            run_metadata: None,
            scenarios: merged_scenarios,
            metric_policies: metric_policies_seen,
        })
    };

    Ok((merged, all_success, exit_code, first_failure_stderr_tail))
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
                gates: Vec::new(),
                gate_results: Vec::new(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
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
                scenario_ids: Vec::new(),
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

    #[test]
    fn run_metadata_captures_reproducible_bench_context() {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let workload_dir = component_dir.path().join("tests/bench");
        std::fs::create_dir_all(&workload_dir).expect("workload dir");
        let workload = workload_dir.join("boot.rs");
        std::fs::write(&workload, "fn main() {}\n").expect("workload file");
        let extension_dir = tempfile::TempDir::new().expect("extension dir");

        let component = Component {
            id: "homeboy".to_string(),
            local_path: component_dir.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let execution_context = ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Bench,
            extension_id: "rust".to_string(),
            extension_path: extension_dir.path().to_path_buf(),
            script_path: "bench-runner.sh".to_string(),
            settings: Vec::new(),
        };
        let args = BenchRunWorkflowArgs {
            component_label: "homeboy".to_string(),
            component_id: "homeboy".to_string(),
            path_override: None,
            settings: Vec::new(),
            settings_json: Vec::new(),
            iterations: 7,
            runs: 3,
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent: 5.0,
            json_summary: false,
            passthrough_args: Vec::new(),
            scenario_ids: vec!["boot".to_string()],
            rig_id: Some("studio".to_string()),
            shared_state: Some(component_dir.path().join("shared")),
            concurrency: 2,
            extra_workloads: Vec::new(),
        };
        let mut results = BenchResults {
            component_id: "homeboy".to_string(),
            iterations: 7,
            run_metadata: None,
            scenarios: vec![BenchScenario {
                id: "boot".to_string(),
                file: Some("tests/bench/boot.rs".to_string()),
                source: Some("in_tree".to_string()),
                default_iterations: None,
                tags: Vec::new(),
                iterations: 7,
                metrics: parsing::BenchMetrics {
                    values: BTreeMap::new(),
                    distributions: BTreeMap::new(),
                },
                gates: Vec::new(),
                gate_results: Vec::new(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                runs: None,
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
        };

        stamp_run_metadata(
            &mut results,
            &execution_context,
            &component,
            &args,
            "2026-04-28T00:00:00Z",
        );

        let metadata = results.run_metadata.expect("metadata stamped");
        assert_eq!(
            metadata.homeboy_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(metadata.started_at, "2026-04-28T00:00:00Z");
        assert_eq!(metadata.iterations, 7);
        assert_eq!(metadata.runs, 3);
        assert_eq!(metadata.concurrency, 2);
        assert_eq!(metadata.selected_scenarios, vec!["boot".to_string()]);
        assert_eq!(metadata.runner.as_ref().unwrap().extension, "rust");
        assert_eq!(metadata.workloads.len(), 1);
        assert_eq!(metadata.workloads[0].id, "boot");
        assert_eq!(metadata.workloads[0].source.as_deref(), Some("in_tree"));
        assert_eq!(
            metadata.workloads[0].path.as_deref(),
            Some(workload.to_string_lossy().as_ref())
        );
        assert_eq!(metadata.workloads[0].sha256.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn bench_env_overrides_are_allow_listed_and_secret_safe() {
        let vars = vec![
            ("HOMEBOY_BENCH_WARMUP_ITERATIONS", "0"),
            ("HOMEBOY_BENCH_PROFILE", "cold"),
            ("HOMEBOY_BENCH_TOKEN", "secret"),
            ("HOMEBOY_BENCH_API_KEY", "secret"),
            ("DATABASE_URL", "postgres://user:pass@example/db"),
        ];

        let captured = bench_env_overrides_from_iter(vars);

        assert_eq!(
            captured.get("HOMEBOY_BENCH_WARMUP_ITERATIONS"),
            Some(&"0".to_string())
        );
        assert_eq!(
            captured.get("HOMEBOY_BENCH_PROFILE"),
            Some(&"cold".to_string())
        );
        assert!(!captured.contains_key("HOMEBOY_BENCH_TOKEN"));
        assert!(!captured.contains_key("HOMEBOY_BENCH_API_KEY"));
        assert!(!captured.contains_key("DATABASE_URL"));
    }
}
