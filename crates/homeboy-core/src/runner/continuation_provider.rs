//! Runner-side implementation of core's `RunnerContinuationProvider` hook.
//!
//! Core's `agent_task_lifecycle` calls this contract to reconcile and resume a
//! run that was dispatched to a remote runner, without depending on runner
//! behavior directly. This adapter delegates to the runner connection,
//! execution, and evidence functions.

use crate::agent_task_lifecycle::RunnerContinuationProvider;
use crate::api_jobs::RunnerJobLogSnapshot;
use crate::error::Result;

/// The runner layer's `RunnerContinuationProvider`. Registered with core at startup.
pub struct RunnerContinuation;

impl RunnerContinuationProvider for RunnerContinuation {
    fn runner_job_log_snapshot(
        &self,
        runner_id: &str,
        job_id: &str,
    ) -> Result<RunnerJobLogSnapshot> {
        super::evidence::runner_job_log_snapshot(runner_id, job_id)
    }

    fn is_runner_connected(&self, runner_id: &str) -> bool {
        // Preserve the original lifecycle semantics: only an affirmative
        // `connected == false` should be treated as disconnected. A status
        // *error* (transient lookup failure) must NOT annotate the run as
        // disconnected, so assume connected when the status can't be read.
        super::connection::status(runner_id)
            .map(|report| report.connected)
            .unwrap_or(true)
    }

    fn runner_exists(&self, runner_id: &str) -> bool {
        super::exists(runner_id)
    }

    fn run_continuation_exec(
        &self,
        runner_id: &str,
        cwd: &str,
        command: &[String],
        run_id: &str,
    ) -> Result<i32> {
        let (_, exit_code) = super::execution::exec(
            runner_id,
            super::execution::RunnerExecOptions {
                cwd: Some(cwd.to_string()),
                command: command.to_vec(),
                run_id: Some(run_id.to_string()),
                ..Default::default()
            },
        )?;
        Ok(exit_code)
    }
}

/// Register the runner continuation provider with core. Called once at startup.
pub fn register() {
    crate::agent_task_lifecycle::register_runner_continuation_provider(Box::new(
        RunnerContinuation,
    ));
}
