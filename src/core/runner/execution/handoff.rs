use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobEvent};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};

use super::super::broker_http;
use super::super::evidence::mirror_daemon_job_progress;
use super::super::{load, status, Runner, RunnerTunnelMode};

#[allow(unused_imports)]
use super::*;

pub(crate) fn lab_offload_handoff_hints(
    runner_id: &str,
    remote_cwd: Option<&str>,
    job_id: &str,
    persisted_run_id: Option<&str>,
    state: DaemonJobHandoffState,
    supports_cancellation: bool,
) -> Vec<String> {
    let runner_exec_prefix = match remote_cwd.filter(|cwd| !cwd.trim().is_empty()) {
        Some(cwd) => format!(
            "homeboy runner exec {runner_id} --cwd {} --",
            shell::quote_arg(cwd)
        ),
        None => format!("homeboy runner exec {runner_id} --"),
    };
    let remote_run_filter =
        format!("{runner_exec_prefix} homeboy runs list --status running --limit 20");
    let mut hints = match state {
        DaemonJobHandoffState::InFlight => vec![format!(
            "Lab offload handoff: runner `{runner_id}` has daemon job `{job_id}` still in flight; the runner-side Homeboy command may continue after this controller exits."
        )],
        DaemonJobHandoffState::Terminal(status) => vec![format!(
            "Lab offload handoff: runner `{runner_id}` daemon job `{job_id}` finished with status `{}`.",
            status.daemon_status_label()
        )],
    };

    if let Some(run_id) = persisted_run_id.filter(|run_id| !run_id.trim().is_empty()) {
        hints.push(format!(
            "Persisted run id: `{run_id}`. Status: `homeboy runs show {run_id}`; evidence: `homeboy runs evidence {run_id}`; artifacts: `homeboy runs artifacts {run_id}`."
        ));
        hints.push(
            "If the command succeeded but those artifact readers show zero artifacts, promote or attach the produced output directory before using the run as review evidence; see `homeboy docs operators/artifact-loop-runner-matrix`.".to_string(),
        );
    } else if state == DaemonJobHandoffState::InFlight {
        hints.push(format!(
            "Persisted runner-side run id is not known yet; list active runner runs with `{remote_run_filter}`."
        ));
    } else {
        hints.push(
            "Persisted runner-side run id is not known; inspect daemon job events for final result details."
                .to_string(),
        );
    }

    match state {
        DaemonJobHandoffState::InFlight => hints.push(format!(
            "Runner-side status/evidence/artifacts: `{remote_run_filter}` then `{runner_exec_prefix} homeboy runs show <run-id>`, `{runner_exec_prefix} homeboy runs evidence <run-id>`, and `{runner_exec_prefix} homeboy runs artifacts <run-id>`."
        )),
        DaemonJobHandoffState::Terminal(_) => hints.push(format!(
            "Final daemon job events/result: `homeboy runner job logs {runner_id} {job_id}`."
        )),
    }
    hints.push(format!(
        "Daemon job logs: `homeboy runner job logs {runner_id} {job_id} --follow`."
    ));
    if state == DaemonJobHandoffState::InFlight && supports_cancellation {
        hints.push(format!(
            "Cancel: `homeboy runner job cancel {runner_id} {job_id}`."
        ));
    }
    hints
}

pub(super) fn print_lab_offload_handoff(
    runner_id: &str,
    remote_cwd: Option<&str>,
    job_id: &str,
    persisted_run_id: Option<&str>,
    state: DaemonJobHandoffState,
) {
    eprintln!("Lab offload handoff:");
    for hint in
        lab_offload_handoff_hints(runner_id, remote_cwd, job_id, persisted_run_id, state, true)
    {
        eprintln!("- {hint}");
    }
}

/// Outcome of the opt-in best-effort remote cancellation attempted when the
/// controller's runner-exec wait budget expires. `Disabled` is the default
/// (opt-in off) and preserves the historical contract: the remote job is left
/// in flight and uncancelled (#6891).
#[derive(Debug)]
pub(super) enum WaitTimeoutCancelOutcome {
    Disabled,
    Cancelled,
    Failed(String),
}

/// Whether the operator opted in to cancelling the remote job on wait-timeout
/// via `HOMEBOY_RUNNER_CANCEL_ON_WAIT_TIMEOUT`. Accepts the usual truthy
/// spellings; anything else (including unset) keeps the default behavior.
pub(super) fn cancel_on_wait_timeout_enabled() -> bool {
    std::env::var(RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Best-effort remote cancellation on wait-timeout. Returns `Disabled` when the
/// opt-in is off so callers preserve the byte-identical default contract. When
/// the opt-in is on, the existing `runner_job_cancel` primitive is invoked and
/// any error is captured rather than propagated (the controller still needs to
/// surface the timeout).
pub(super) fn attempt_wait_timeout_cancel(runner_id: &str, job_id: &str) -> WaitTimeoutCancelOutcome {
    if !cancel_on_wait_timeout_enabled() {
        return WaitTimeoutCancelOutcome::Disabled;
    }
    match invoke_runner_job_cancel(runner_id, job_id) {
        Ok(()) => WaitTimeoutCancelOutcome::Cancelled,
        Err(err) => WaitTimeoutCancelOutcome::Failed(err.message),
    }
}

/// Invoke the remote-cancel primitive. In production this hits the daemon/broker
/// `/jobs/{id}/cancel` endpoint via `runner_job_cancel`; tests inject a hook so
/// the opt-in path can be asserted without a live runner.
fn invoke_runner_job_cancel(runner_id: &str, job_id: &str) -> Result<()> {
    #[cfg(test)]
    {
        if let Some(result) = test_cancel_hook::take_invoke(runner_id, job_id) {
            return result;
        }
    }
    runner_job_cancel(runner_id, job_id).map(|_| ())
}

#[cfg(test)]
pub(super) mod test_cancel_hook {
    use crate::core::error::Result;
    use std::cell::RefCell;

    type CancelHook = Box<dyn FnMut(&str, &str) -> Result<()>>;

    thread_local! {
        static HOOK: RefCell<Option<CancelHook>> = const { RefCell::new(None) };
    }

    /// Install a thread-local cancel hook for the duration of `Guard`'s lifetime.
    pub(in crate::core::runner::execution) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|cell| *cell.borrow_mut() = None);
        }
    }

    pub(in crate::core::runner::execution) fn install(hook: CancelHook) -> Guard {
        HOOK.with(|cell| *cell.borrow_mut() = Some(hook));
        Guard
    }

    pub(super) fn take_invoke(runner_id: &str, job_id: &str) -> Option<Result<()>> {
        HOOK.with(|cell| cell.borrow_mut().as_mut().map(|hook| hook(runner_id, job_id)))
    }
}

pub fn runner_job_cancel(runner_id: &str, job_id: &str) -> Result<(Job, Vec<JobEvent>)> {
    let runner = load(runner_id)?;
    let connected = status(runner_id)?;
    let Some(session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Runner job cancellation requires an active direct daemon or reverse broker transport."
                    .to_string(),
            ]),
        ));
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| {
            Error::internal_unexpected(format!("build runner job cancel client: {err}"))
        })?;
    let path = format!("/jobs/{job_id}/cancel");
    let body = if let Some(local_url) = session.local_url.as_deref() {
        let data = daemon_post(&client, local_url, &path)?;
        canonical_daemon_body(&data, "daemon job cancel response")?.clone()
    } else if session.mode == RunnerTunnelMode::Reverse {
        let Some(broker_url) = session.broker_url.as_deref() else {
            return Err(runner_job_cancel_unsupported(
                &runner.id,
                "reverse runner session has no broker URL",
            ));
        };
        broker_http::post_json(
            &client,
            broker_url,
            &path,
            json!({}),
            "cancel reverse runner broker job",
            super::super::broker_auth::broker_token_from_env().as_deref(),
        )?
    } else {
        return Err(runner_job_cancel_unsupported(
            &runner.id,
            "runner session does not expose a cancellable daemon or broker transport",
        ));
    };
    parse_runner_job_cancel_body(body)
}

pub(super) fn runner_job_cancel_unsupported(runner_id: &str, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "runner",
        format!("runner job cancellation is unsupported for runner `{runner_id}`: {reason}"),
        Some(runner_id.to_string()),
        Some(vec![
            "Use a direct daemon connection or a reverse runner session registered with a broker before cancelling runner jobs."
                .to_string(),
        ]),
    )
}

pub(super) fn parse_runner_job_cancel_body(body: Value) -> Result<(Job, Vec<JobEvent>)> {
    let job: Job = serde_json::from_value(body["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner job cancel response".to_string()),
        )
    })?;
    let events = body
        .get("events")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse runner job cancel events".to_string()),
            )
        })?
        .unwrap_or_default();
    Ok((job, events))
}

pub(super) fn persist_lab_offload_handoff_run(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    run_id: Option<&str>,
) -> Option<String> {
    match mirror_daemon_job_progress(runner, cwd, command, job, &[], run_id) {
        Ok(run) => Some(run.id),
        Err(err) => {
            eprintln!(
                "Lab offload handoff: could not persist controller-side run mirror for runner `{}` daemon job `{}`: {}",
                runner.id, job.id, err.message
            );
            None
        }
    }
}

pub(super) fn runner_exec_wait_timeout() -> Duration {
    std::env::var(RUNNER_EXEC_WAIT_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_RUNNER_EXEC_WAIT_TIMEOUT_SECS))
}
