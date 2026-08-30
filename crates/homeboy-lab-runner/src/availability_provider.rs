//! Runner-side implementation of core's `RunnerAvailabilityProvider` hook.
//!
//! Computes the controller-action availability verdict from the runner's live
//! status report, keeping `RunnerStatusReport` inside the runner layer.

use homeboy_agents::agent_task_controller_service::runner_availability::RunnerAvailabilityProvider;
use homeboy_agents::agent_task_loop_runner_policy::AgentTaskLoopRunnerAvailability;

/// The runner layer's `RunnerAvailabilityProvider`. Registered with core at startup.
pub struct RunnerAvailability;

impl RunnerAvailabilityProvider for RunnerAvailability {
    fn controller_runner_availability(&self, runner_id: &str) -> AgentTaskLoopRunnerAvailability {
        match crate::RunnerDiscoveryService::readiness(runner_id) {
            Ok(readiness) if readiness.accepting_jobs => {
                AgentTaskLoopRunnerAvailability::Available
            }
            Ok(readiness) => AgentTaskLoopRunnerAvailability::Unavailable {
                reason: format!(
                    "runner `{runner_id}` is not available for controller action execution: connected={}, reasons={}",
                    readiness.connected,
                    readiness.reasons.join(",")
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
    homeboy_agents::agent_task_controller_service::runner_availability::register_runner_availability_provider(
        Box::new(RunnerAvailability),
    );
}
