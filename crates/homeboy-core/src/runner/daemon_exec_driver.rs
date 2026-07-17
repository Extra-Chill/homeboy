//! Runner-side implementation of core's `RunnerExecDriver` hook.
//!
//! Core's daemon `/exec` endpoint prepares and runs a runner job as a local
//! child. This adapter builds the runner process plan and drives the child,
//! keeping runner types (`Runner`, `PreparedRunnerProcess`, `ProcessOutput`)
//! inside the runner layer while the daemon sees only slim core types.

use std::sync::Arc;

use serde_json::Value;

use crate::daemon::runner_exec_driver::{
    DaemonExecOutput, ExecCancellationProbe, ExecChildStarted, ExecProgressSink,
    PreparedDaemonExec, RunnerExecDriver, RunnerExecPrepareRequest,
};
use crate::error::Result;
use crate::secret_env_plan::SecretEnvPlan;

use super::execution::{
    execute_runner_process_until_cancelled_with_progress, prepare_daemon_local_process,
    PreparedRunnerProcess, RunnerProcessRequest,
};
use super::Runner;

/// The runner layer's `RunnerExecDriver`. Registered with core at startup.
pub struct RunnerDaemonExecDriver;

impl RunnerExecDriver for RunnerDaemonExecDriver {
    fn prepare(&self, request: RunnerExecPrepareRequest) -> Result<PreparedDaemonExec> {
        let runner: Option<Runner> = match request.runner {
            Some(value) => Some(serde_json::from_value(value).map_err(|err| {
                crate::error::Error::validation_invalid_argument(
                    "runner",
                    format!("invalid runner descriptor in exec request: {err}"),
                    None,
                    None,
                )
            })?),
            None => None,
        };
        let secret_env_plan: Option<SecretEnvPlan> = match request.secret_env_plan {
            Some(Value::Null) | None => None,
            Some(value) => Some(serde_json::from_value(value).map_err(|err| {
                crate::error::Error::validation_invalid_argument(
                    "secret_env_plan",
                    format!("invalid secret env plan in exec request: {err}"),
                    None,
                    None,
                )
            })?),
        };

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: request.runner_id,
            runner,
            cwd: request.cwd,
            project_id: request.project_id,
            command: request.command,
            env: request.env,
            secret_env_names: request.secret_env_names,
            secret_env_plan,
            capture_patch: request.capture_patch,
            raw_exec: request.raw_exec,
            source_snapshot: request.source_snapshot,
            require_paths: request.require_paths,
            validate_require_paths_on_host: request.validate_require_paths_on_host,
        })?;

        Ok(PreparedDaemonExec::new(
            plan.runner.id.clone(),
            plan.cwd.clone(),
            plan.command.clone(),
            plan.env.clone(),
            plan.secret_env_names.clone(),
            plan.source_snapshot.clone(),
            plan.require_paths.clone(),
            Arc::new(plan),
        ))
    }

    fn execute(
        &self,
        prepared: &PreparedDaemonExec,
        is_cancelled: ExecCancellationProbe,
        progress_sink: Option<ExecProgressSink>,
        require_child_identity_acknowledgement: bool,
        child_started: Option<ExecChildStarted>,
    ) -> Result<DaemonExecOutput> {
        let base = prepared
            .plan_token()
            .downcast_ref::<PreparedRunnerProcess>()
            .ok_or_else(|| {
                crate::error::Error::internal_unexpected(
                    "runner exec driver received a plan it did not prepare",
                )
            })?;

        // The daemon may have mutated `prepared.env` (e.g. injecting the child
        // reservation id) between prepare and execute, so run with that env.
        let mut plan = base.clone();
        plan.env = prepared.env.clone();

        let mut is_cancelled = is_cancelled;
        let output = execute_runner_process_until_cancelled_with_progress(
            &plan,
            || is_cancelled(),
            progress_sink,
            require_child_identity_acknowledgement,
            child_started,
        )?;

        Ok(DaemonExecOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            metrics: output
                .metrics
                .and_then(|metrics| serde_json::to_value(metrics).ok()),
            capture: output
                .capture
                .and_then(|capture| serde_json::to_value(capture).ok()),
        })
    }
}

/// Register the runner daemon-exec driver with core. Called once at startup.
pub fn register() {
    crate::daemon::runner_exec_driver::register_runner_exec_driver(Arc::new(
        RunnerDaemonExecDriver,
    ));
}
