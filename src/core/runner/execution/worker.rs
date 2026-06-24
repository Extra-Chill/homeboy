use std::collections::HashMap;

use crate::core::error::Result;

use super::super::capabilities::{
    runner_capability_snapshot_for_preflight, validate_runner_capability_preflight,
};
use super::super::{load, Runner, RunnerCapabilityPreflight, RunnerKind};

use super::extension_parity::{required_extensions_for_command, validate_runner_extension_parity};
use super::policy::{validate_runner_policy, RunnerPolicyRequest};

#[allow(unused_imports)]
use super::*;

pub(crate) fn exec_worker_local_until_cancelled(
    runner_id: &str,
    options: RunnerExecOptions,
    is_cancelled: impl FnMut() -> bool,
) -> Result<(RunnerExecOutput, i32)> {
    let mut is_cancelled = is_cancelled;
    exec_worker_local_with_process_output(runner_id, options, |plan| {
        execute_runner_process_until_cancelled(plan, &mut is_cancelled)
    })
}

pub(super) fn exec_worker_local_with_process_output(
    runner_id: &str,
    options: RunnerExecOptions,
    execute: impl FnOnce(&PreparedRunnerProcess) -> Result<ProcessOutput>,
) -> Result<(RunnerExecOutput, i32)> {
    let secret_env_names = runner_exec_secret_env_names(
        &options.command,
        options.capability_preflight.as_ref(),
        &options.secret_env_names,
    );
    let mut runner = load(runner_id)?;
    runner.kind = RunnerKind::Local;
    runner.server_id = None;
    let plan = prepare_daemon_local_process(RunnerProcessRequest {
        runner_id: runner_id.to_string(),
        runner: Some(runner),
        cwd: options.cwd.clone(),
        project_id: options.project_id.clone(),
        command: options.command.clone(),
        env: options.env.clone(),
        secret_env_names: secret_env_names.clone(),
        capture_patch: options.capture_patch,
        raw_exec: options.raw_exec,
        source_snapshot: options.source_snapshot.clone(),
        require_paths: options.require_paths.clone(),
        validate_require_paths_on_host: true,
    })?;
    super::super::workload::validate_runner_workload_dispatch(
        options.runner_workload.as_ref(),
        runner_id,
        Some(&plan.cwd),
        &options.command,
        &secret_env_names,
        options.capture_patch,
    )?;
    let required_extensions = required_extensions_for_command(
        &options.command,
        &super::super::workload::merge_runner_workload_required_extensions(
            options.required_extensions.clone(),
            options.runner_workload.as_ref(),
        ),
    );
    validate_runner_extension_parity(runner_id, &plan.runner, &plan.cwd, &required_extensions)?;
    validate_runner_policy(
        &plan.runner,
        &plan.cwd,
        RunnerPolicyRequest {
            project_id: options.project_id.as_deref(),
            command: &options.command,
            capture_patch: options.capture_patch,
            raw_exec: options.raw_exec,
        },
    )?;
    let capability_preflight = super::super::workload::merge_runner_workload_capability_preflight(
        options.capability_preflight.clone(),
        options.runner_workload.as_ref(),
    )?;
    preflight_worker_local_capability_plan(&plan.runner, capability_preflight.as_ref(), &plan.env)?;
    let output = execute(&plan)?;
    Ok(exec_output(
        &plan.runner,
        RunnerExecMode::Local,
        plan.cwd,
        plan.command,
        output,
        Some(plan.source_snapshot),
        plan.require_paths,
        &plan.env,
        &[],
    ))
}

pub(super) fn preflight_worker_local_capability_plan(
    runner: &Runner,
    preflight: Option<&RunnerCapabilityPreflight>,
    request_env: &HashMap<String, String>,
) -> Result<()> {
    let Some(preflight) = preflight else {
        return Ok(());
    };
    if preflight.is_empty() {
        return Ok(());
    }

    let capabilities = runner_capability_snapshot_for_preflight(runner, preflight)?;
    validate_runner_capability_preflight(&runner.id, preflight, &capabilities, request_env)
}
