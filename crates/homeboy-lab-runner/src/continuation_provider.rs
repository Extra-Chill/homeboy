//! Runner-side implementation of core's `RunnerContinuationProvider` hook.
//!
//! Core's `agent_task_lifecycle` calls this contract to reconcile and resume a
//! run that was dispatched to a remote runner, without depending on runner
//! behavior directly. This adapter delegates to the runner connection,
//! execution, and evidence functions.

use homeboy_agents::agent_task_lifecycle::{RunnerContinuationProvider, RunnerJobReconciliation};
use homeboy_core::api_jobs::{Job, RemoteRunnerJobRequest, RunnerJobLogSnapshot};
use homeboy_core::error::Result;

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

    fn reconcile_runner_job(&self, runner_id: &str, job_id: &str) -> RunnerJobReconciliation {
        match self.runner_job_log_snapshot(runner_id, job_id) {
            Ok(snapshot) => return RunnerJobReconciliation::Snapshot(snapshot),
            Err(error) if !job_not_found(&error, job_id) => {
                return RunnerJobReconciliation::UnconfirmedAbsence;
            }
            Err(_) => {}
        }

        let Ok(status) = super::connection::status(runner_id) else {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        };
        let Some(session) = status.session.filter(|_| status.connected) else {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        };
        let Ok(generations) = super::generation_store::live_sessions(runner_id, Some(&session))
        else {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        };
        if generations.is_empty() {
            return RunnerJobReconciliation::UnconfirmedAbsence;
        }

        let mut checked_generations = 0;
        for generation in generations {
            if generation.local_url.is_none() {
                return RunnerJobReconciliation::UnconfirmedAbsence;
            }
            checked_generations += 1;
            match super::evidence::runner_job_log_snapshot_for_session(&generation, job_id) {
                Ok(snapshot) => {
                    if super::generation_store::record_job(runner_id, &generation, job_id).is_err()
                    {
                        return RunnerJobReconciliation::UnconfirmedAbsence;
                    }
                    return RunnerJobReconciliation::Snapshot(snapshot);
                }
                Err(error) if job_not_found(&error, job_id) => continue,
                Err(_) => return RunnerJobReconciliation::UnconfirmedAbsence,
            }
        }
        RunnerJobReconciliation::ConfirmedAbsent {
            checked_generations,
        }
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

    fn submit_reverse_broker_job(
        &self,
        runner_id: &str,
        request: RemoteRunnerJobRequest,
    ) -> Result<Job> {
        super::connection::submit_reverse_broker_job(runner_id, request)
    }

    fn lookup_reverse_broker_submission(
        &self,
        runner_id: &str,
        submission_key: &str,
    ) -> Result<homeboy_core::api_jobs::RemoteRunnerSubmissionLookup> {
        super::connection::lookup_reverse_broker_submission(runner_id, submission_key)
    }
}

fn job_not_found(error: &homeboy_core::error::Error, job_id: &str) -> bool {
    error
        .details
        .get("http_status")
        .and_then(serde_json::Value::as_u64)
        == Some(404)
        && error
            .details
            .get("path")
            .and_then(serde_json::Value::as_str)
            == Some(&format!("/jobs/{job_id}"))
}

/// Register the runner continuation provider with core. Called once at startup.
pub fn register() {
    homeboy_agents::agent_task_lifecycle::register_runner_continuation_provider(Box::new(
        RunnerContinuation,
    ));
}
