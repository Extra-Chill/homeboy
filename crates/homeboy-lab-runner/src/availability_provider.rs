//! Runner-side implementation of core's `RunnerAvailabilityProvider` hook.
//!
//! Computes the controller-action availability verdict from the runner's live
//! status report, keeping `RunnerStatusReport` inside the runner layer.

use homeboy_agents::agent_task_controller_service::runner_availability::RunnerAvailabilityProvider;
use homeboy_agents::agent_task_loop_runner_policy::AgentTaskLoopRunnerAvailability;

use crate::RunnerActiveJobState;

/// The runner layer's `RunnerAvailabilityProvider`. Registered with core at startup.
pub struct RunnerAvailability;

impl RunnerAvailabilityProvider for RunnerAvailability {
    fn controller_runner_availability(&self, runner_id: &str) -> AgentTaskLoopRunnerAvailability {
        match crate::status(runner_id) {
            // A report that established nothing does not block execution — it
            // is surfaced as `daemon_verification` so the operator can see the
            // gap without the runner dropping out of service (#11106).
            Ok(status)
                if status.connected
                    && crate::load(runner_id).is_ok_and(|runner| {
                        !crate::lab::offload::metadata::lab_runner_homeboy_has_blocking_status_drift(
                            &status,
                            crate::lab::offload::metadata::require_exact_runner_version(
                                &runner.settings,
                            ),
                        )
                    })
                    && status.active_job_state == RunnerActiveJobState::Available =>
            {
                AgentTaskLoopRunnerAvailability::Available
            }
            Ok(status) => AgentTaskLoopRunnerAvailability::Unavailable {
                reason: format!(
                    "runner `{runner_id}` is not available for controller action execution: state={:?}, connected={}, stale_daemon={}, daemon_verification={}, active_job_state={:?}",
                    status.state,
                    status.connected,
                    status.admission_blocking_stale_daemon().is_some(),
                    status
                        .stale_daemon
                        .as_ref()
                        .map_or("compared", |warning| if warning.is_unverified() {
                            "unverified"
                        } else {
                            "compared"
                        }),
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
    homeboy_agents::agent_task_controller_service::runner_availability::register_runner_availability_provider(
        Box::new(RunnerAvailability),
    );
}
