//! Runner-availability hook.
//!
//! The controller action loop gates execution on whether a (possibly remote)
//! runner is available. That check inspects the runner's live status report,
//! which is runner behavior, so it is inverted behind this provider: core asks
//! for a slim availability verdict, the runner layer computes it from the
//! runner status.
//!
//! With no provider registered (single-machine, no runner) the no-op provider
//! reports the runner unavailable — the controller loop only reaches this check
//! for a runner-targeted action.

use std::sync::Mutex;

use crate::agent_task_loop_runner_policy::AgentTaskLoopRunnerAvailability;

/// Computes whether a runner is available for controller action execution.
pub trait RunnerAvailabilityProvider: Send + Sync {
    fn controller_runner_availability(&self, runner_id: &str) -> AgentTaskLoopRunnerAvailability;
}

struct NoopProvider;

impl RunnerAvailabilityProvider for NoopProvider {
    fn controller_runner_availability(&self, runner_id: &str) -> AgentTaskLoopRunnerAvailability {
        AgentTaskLoopRunnerAvailability::Unavailable {
            reason: format!(
                "runner `{runner_id}` is not available: the runner subsystem is not present"
            ),
        }
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn RunnerAvailabilityProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn RunnerAvailabilityProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the runner-availability provider. Called once at startup by the
/// runner layer.
pub fn register_runner_availability_provider(provider: Box<dyn RunnerAvailabilityProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("runner availability provider lock");
    *slot = Some(provider);
}

/// The controller-action availability verdict for a runner, via the registered
/// provider (or the no-op provider when the runner subsystem is absent).
pub(crate) fn controller_runner_availability(runner_id: &str) -> AgentTaskLoopRunnerAvailability {
    let slot = provider_slot()
        .lock()
        .expect("runner availability provider lock");
    match slot.as_deref() {
        Some(provider) => provider.controller_runner_availability(runner_id),
        None => NoopProvider.controller_runner_availability(runner_id),
    }
}
