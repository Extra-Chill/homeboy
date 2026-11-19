//! Local gate admission: binds a Cook's local gate to the host-wide queue in
//! [`homeboy_core::local_gate_admission`], which the Lab daemon on the same
//! host also charges against its step budget.
use homeboy_core::lab_capacity::{LabPriority, LAB_BUDGET_ENV, LAB_PRIORITY_ENV};
use homeboy_core::local_gate_admission::{LocalGateClaim, LocalGatePermit, Registry};
use homeboy_core::Result;
use std::time::Duration;

/// Outcome of waiting for a local gate slot.
pub(super) enum Admission {
    /// Admitted after `wait`; the permit holds the slot until dropped.
    Admitted(Option<LocalGatePermit>, Duration),
    /// Cancelled while queued; the gate never started.
    Cancelled,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn cook_id(cwd: &std::path::Path) -> String {
    homeboy_core::observation::phase_timing::current()
        .map(|(_, id)| {
            crate::agent_task_lifecycle::status(&id)
                .ok()
                .and_then(|record| record.metadata["cook_id"].as_str().map(str::to_string))
                .unwrap_or(id)
        })
        .unwrap_or_else(|| cwd.display().to_string())
}

pub(super) fn acquire(
    cwd: &std::path::Path,
    index: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Admission> {
    // Unit tests run gates in parallel processes; a host-wide queue would
    // couple them. The registry itself is covered by its own tests.
    if cfg!(test) {
        return Ok(Admission::Admitted(None, Duration::ZERO));
    }
    let Some(registry) = Registry::host()? else {
        return Ok(Admission::Admitted(None, Duration::ZERO));
    };
    let cook_id = cook_id(cwd);
    let claim = LocalGateClaim {
        // A workflow instance is the fairness unit, as in the Lab step queue.
        owner: env(homeboy_core::daemon::workflow_job::WORKFLOW_INSTANCE_ENV)
            .unwrap_or_else(|| cook_id.clone()),
        cook_id,
        // Only a public gate index is persisted: private command text is never a capacity channel.
        gate: format!("gate-{index}"),
        priority: LabPriority::from_env_value(env(LAB_PRIORITY_ENV).as_deref()),
        budget: env(LAB_BUDGET_ENV)
            .and_then(|seconds| seconds.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs),
        // A Cook supervised by a daemon step or runner child already holds a
        // daemon slot; its gates queue here but the daemon does not charge them.
        daemon_job: env(homeboy_core::daemon::step_job::LOCAL_COOK_SUPERVISOR_JOB_ID_ENV)
            .or_else(|| env(homeboy_core::daemon::step_job::STEP_JOB_ID_ENV))
            .or_else(|| env(homeboy_core::runner_job_execution_context::RUNNER_JOB_ID_ENV)),
    };
    let permit = registry.enqueue(claim)?;
    Ok(match permit.wait(cancelled)? {
        Some(wait) => Admission::Admitted(Some(permit), wait),
        None => Admission::Cancelled,
    })
}
