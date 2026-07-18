//! Bench scenario list (discovery-only) workflow.

use std::path::{Path, PathBuf};

use crate::bench::parsing::{self, BenchRunExecution};
use crate::{resolve_execution_context, ExtensionCapability};
use homeboy_core::component::Component;
use homeboy_core::engine::invocation::InvocationRequirements;
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::error::{Error, Result};
use homeboy_engine_primitives::baseline::BaselineFlags;

use super::runner::build_runner;
use super::scenario::{apply_scenario_filter, normalize_workload_json_scenario_ids};
use super::types::{BenchListWorkflowArgs, BenchListWorkflowResult, BenchRunWorkflowArgs};

pub fn run_bench_list_workflow(
    component: &Component,
    args: BenchListWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchListWorkflowResult> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    let preferred_workspace_path =
        preferred_workspace_path(component, &args).map(Path::to_path_buf);
    if component.has_script(ExtensionCapability::Bench) && args.extra_workloads.is_empty() {
        let list_env = bench_component_script_list_env(&args)?;
        let source_path =
            crate::component_script::source_path(component, args.path_override.as_deref());
        let output = crate::component_script::run_component_scripts_with_run_dir(
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
        return bench_list_result(
            args.component_label,
            results_file,
            &args.scenario_ids,
            preferred_workspace_path.as_deref(),
            args.rig_package,
            args.profiles,
        );
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
            env_provider_extensions: args.env_provider_extensions,
            rig_package: args.rig_package.clone(),
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
    bench_list_result(
        args.component_label,
        results_file,
        &args.scenario_ids,
        preferred_workspace_path.as_deref(),
        args.rig_package,
        args.profiles,
    )
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
    preferred_workspace_path: Option<&Path>,
    rig_package: Option<crate::bench::parsing::RigPackageEvidence>,
    profiles: Vec<super::types::BenchListProfile>,
) -> Result<BenchListWorkflowResult> {
    let mut parsed =
        parsing::parse_bench_results_file_with_artifact_context_scenarios_and_workspace(
            &results_file,
            None,
            scenario_ids,
            preferred_workspace_path,
        )?;
    normalize_workload_json_scenario_ids(&mut parsed);
    let parsed = apply_scenario_filter(parsed, scenario_ids)?;
    let count = parsed.scenarios.len();

    Ok(BenchListWorkflowResult {
        component: component_label,
        component_id: parsed.component_id,
        scenarios: parsed.scenarios,
        count,
        profiles,
        hints: Vec::new(),
        rig_package,
    })
}

fn preferred_workspace_path<'a>(
    component: &'a Component,
    args: &'a BenchListWorkflowArgs,
) -> Option<&'a Path> {
    args.path_override
        .as_deref()
        .map(Path::new)
        .or_else(|| (!component.local_path.is_empty()).then(|| Path::new(&component.local_path)))
}

pub(crate) fn bench_component_script_list_env(
    args: &BenchListWorkflowArgs,
) -> Result<Vec<(String, String)>> {
    let mut env = vec![("HOMEBOY_BENCH_LIST_ONLY".to_string(), "1".to_string())];
    env.push((
        "HOMEBOY_BENCH_ARGS_JSON".to_string(),
        serde_json::to_string(&args.passthrough_args).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("serialize bench passthrough args".to_string()),
            )
        })?,
    ));
    env.push((
        "HOMEBOY_SETTINGS_JSON".to_string(),
        crate::build_settings_json_from_manifest(
            &serde_json::json!({}),
            &[],
            &args.settings,
            &args.settings_json,
        )?,
    ));
    Ok(env)
}
