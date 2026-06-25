//! Bench scenario list (discovery-only) workflow.

use std::path::PathBuf;

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::invocation::InvocationRequirements;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::bench::parsing::{self, BenchRunExecution};
use crate::core::extension::{resolve_execution_context, ExtensionCapability};

use super::runner::build_runner;
use super::scenario::{apply_scenario_filter, normalize_workload_json_scenario_ids};
use super::types::{BenchListWorkflowArgs, BenchListWorkflowResult, BenchRunWorkflowArgs};

pub fn run_bench_list_workflow(
    component: &Component,
    args: BenchListWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchListWorkflowResult> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    if component.has_script(ExtensionCapability::Bench) && args.extra_workloads.is_empty() {
        let list_env = bench_component_script_list_env(&args)?;
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
            &list_env,
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
            run_id: None,
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

pub(crate) fn ensure_bench_list_success(
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

pub(crate) fn bench_list_result(
    component_label: String,
    results_file: PathBuf,
    scenario_ids: &[String],
) -> Result<BenchListWorkflowResult> {
    let mut parsed = parsing::parse_bench_results_file_with_artifact_context(&results_file, None)?;
    normalize_workload_json_scenario_ids(&mut parsed);
    let parsed = apply_scenario_filter(parsed, scenario_ids)?;
    let count = parsed.scenarios.len();

    Ok(BenchListWorkflowResult {
        component: component_label,
        component_id: parsed.component_id,
        scenarios: parsed.scenarios,
        count,
    })
}

pub(crate) fn bench_component_script_list_env(
    args: &BenchListWorkflowArgs,
) -> Result<Vec<(String, String)>> {
    let mut env = vec![("HOMEBOY_BENCH_LIST_ONLY".to_string(), "1".to_string())];
    env.push((
        "HOMEBOY_SETTINGS_JSON".to_string(),
        crate::core::extension::build_settings_json_from_manifest(
            &serde_json::json!({}),
            &[],
            &args.settings,
            &args.settings_json,
        )?,
    ));
    Ok(env)
}
