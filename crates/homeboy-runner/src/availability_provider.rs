//! Runner-side implementation of core's `RunnerAvailabilityProvider` hook.
//!
//! Computes the controller-action availability verdict from the runner's live
//! status report, keeping `RunnerStatusReport` inside the runner layer.

use homeboy_core::agent_task_controller_service::runner_availability::RunnerAvailabilityProvider;
use homeboy_core::agent_task_loop_runner_policy::AgentTaskLoopRunnerAvailability;

use crate::RunnerActiveJobState;

/// The runner layer's `RunnerAvailabilityProvider`. Registered with core at startup.
pub struct RunnerAvailability;

impl RunnerAvailabilityProvider for RunnerAvailability {
    fn controller_runner_availability(&self, runner_id: &str) -> AgentTaskLoopRunnerAvailability {
        match crate::status(runner_id) {
            Ok(status)
                if status.connected
                    && status.stale_daemon.is_none()
                    && status.active_job_state == RunnerActiveJobState::Available =>
            {
                AgentTaskLoopRunnerAvailability::Available
            }
            Ok(status) => AgentTaskLoopRunnerAvailability::Unavailable {
                reason: format!(
                    "runner `{runner_id}` is not available for controller action execution: state={:?}, connected={}, stale_daemon={}, active_job_state={:?}",
                    status.state,
                    status.connected,
                    status.stale_daemon.is_some(),
                    status.active_job_state
                ),
            },
            Err(error) => AgentTaskLoopRunnerAvailability::Unavailable {
                reason: format!(
                    "runner `{runner_id}` is not available for controller action execution: {error}"
                ),
            },
        }
    }
}

/// Register the runner availability provider with core. Called once at startup.
pub fn register() {
    homeboy_core::agent_task_controller_service::runner_availability::register_runner_availability_provider(
        Box::new(RunnerAvailability),
    );
}
