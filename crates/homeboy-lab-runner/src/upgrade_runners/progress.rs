//! Live position of `homeboy upgrade` runner convergence.
//!
//! The upgrade heartbeat samples this (through
//! `homeboy_upgrade::upgrade::register_runner_convergence_progress`) so a slow
//! convergence reports the step it is in and what it waits on, instead of a
//! fixed label for its whole duration.

use std::sync::Mutex;

use homeboy_core::api_jobs::{Job, JobEvent, JobStatus};
use homeboy_upgrade::upgrade::RunnerConvergenceProgress;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConvergenceStep {
    Binary,
    ExtensionSync,
    ServiceBinaries,
}

struct State {
    runner_id: String,
    step: ConvergenceStep,
    item: Option<String>,
    waiting_on: Option<String>,
    run_id: Option<String>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state(update: impl FnOnce(&mut Option<State>)) {
    update(
        &mut STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
}

/// Enter a convergence step for `runner_id`, dropping the previous step's
/// item, wait, and runner job.
pub(crate) fn begin_step(runner_id: &str, step: ConvergenceStep) {
    with_state(|state| {
        *state = Some(State {
            runner_id: runner_id.to_string(),
            step,
            item: None,
            waiting_on: None,
            run_id: None,
        });
    });
}

/// Name the item the current step is working on.
pub(crate) fn begin_item(item: String) {
    with_state(|state| {
        if let Some(state) = state.as_mut() {
            state.item = Some(item);
            state.waiting_on = None;
            state.run_id = None;
        }
    });
}

/// Convergence has finished; later runner jobs are not upgrade work.
pub(crate) fn finish() {
    with_state(|state| *state = None);
}

/// Record the runner job the current step is waiting on. A no-op outside an
/// upgrade's runner convergence.
pub(crate) fn observe_runner_job(run_id: Option<&str>, job: &Job, events: &[JobEvent]) {
    with_state(|state| {
        if let Some(state) = state.as_mut() {
            if let Some(run_id) = run_id {
                state.run_id = Some(run_id.to_string());
            }
            state.waiting_on = runner_job_wait(job, events);
        }
    });
}

/// What a queued runner job waits on: daemon admission (with the reason the
/// daemon recorded) or the plain job queue. Started jobs wait on nothing.
pub(crate) fn runner_job_wait(job: &Job, events: &[JobEvent]) -> Option<String> {
    if job.status != JobStatus::Queued {
        return None;
    }
    let admission = events.iter().rev().find_map(|event| {
        let data = event.data.as_ref()?;
        (data.get("status").and_then(serde_json::Value::as_str)
            == Some("pending_resource_admission"))
        .then(|| {
            data.get("pending_admission_reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("capacity")
                .to_string()
        })
    });
    Some(match admission {
        Some(reason) => format!("Lab admission ({reason})"),
        None => "runner job queue".to_string(),
    })
}

/// The heartbeat sample: the current step, with the live `refresh-homeboy`
/// sub-phase while a binary refresh runs.
pub(crate) fn sample() -> Option<RunnerConvergenceProgress> {
    let state = STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = state.as_ref()?;
    let refresh = crate::homeboy_refresh::refresh_live_progress().map(|live| live.sub_phase);
    let (sub_phase, item) = match state.step {
        ConvergenceStep::Binary => (
            refresh.unwrap_or_else(|| "binary_convergence".to_string()),
            state.item.clone(),
        ),
        ConvergenceStep::ExtensionSync => ("extension_sync".to_string(), state.item.clone()),
        ConvergenceStep::ServiceBinaries => (
            "service_binaries".to_string(),
            refresh
                .map(|refresh| format!("refresh:{refresh}"))
                .or_else(|| state.item.clone()),
        ),
    };
    Some(RunnerConvergenceProgress {
        runner_id: state.runner_id.clone(),
        sub_phase,
        item,
        waiting_on: state.waiting_on.clone(),
        run_id: state.run_id.clone(),
    })
}
