//! Runner-side implementation of core's `RunnerAvailabilityProvider` hook.
//!
//! Computes the controller-action availability verdict from the runner's live
//! status report, keeping `RunnerStatusReport` inside the runner layer.

use homeboy_agents::agent_task_controller_service::runner_availability::RunnerAvailabilityProvider;
use homeboy_agents::agent_task_loop_runner_policy::AgentTaskLoopRunnerAvailability;
use homeboy_core::Result;
use homeboy_runner_contract::{
    RunnerApiReadinessRequest, RunnerApiReadinessResponse, RunnerReadiness,
    RUNNER_API_READINESS_REQUEST_SCHEMA, RUNNER_API_V1,
};

/// The runner layer's `RunnerAvailabilityProvider`. Registered with core at startup.
pub struct RunnerAvailability;

impl RunnerAvailabilityProvider for RunnerAvailability {
    fn controller_runner_availability(&self, runner_id: &str) -> AgentTaskLoopRunnerAvailability {
        availability_from_response(
            runner_id,
            crate::RunnerDiscoveryService::readiness_api(&RunnerApiReadinessRequest {
                schema: RUNNER_API_READINESS_REQUEST_SCHEMA.to_string(),
                api_version: RUNNER_API_V1,
                runner_id: runner_id.to_string(),
            }),
        )
    }
}

fn availability_from_response(
    runner_id: &str,
    response: Result<RunnerApiReadinessResponse>,
) -> AgentTaskLoopRunnerAvailability {
    match response
        .map_err(|error| error.to_string())
        .and_then(readiness_from_response)
    {
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

fn readiness_from_response(
    response: RunnerApiReadinessResponse,
) -> std::result::Result<RunnerReadiness, String> {
    if let Some(failure) = response.failure {
        return Err(failure.message);
    }

    response.readiness.ok_or_else(|| {
        "Runner API readiness response omitted both readiness and failure".to_string()
    })
}

/// Register the runner availability provider with core. Called once at startup.
pub fn register() {
    homeboy_agents::agent_task_controller_service::runner_availability::register_runner_availability_provider(
        Box::new(RunnerAvailability),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_runner_contract::{RUNNER_API_READINESS_RESPONSE_SCHEMA, RUNNER_READINESS_SCHEMA};

    fn response(readiness: Option<RunnerReadiness>) -> RunnerApiReadinessResponse {
        RunnerApiReadinessResponse {
            schema: RUNNER_API_READINESS_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "runner-a".to_string(),
            readiness,
            failure: None,
        }
    }

    #[test]
    fn accepting_readiness_is_available() {
        let availability = availability_from_response(
            "runner-a",
            Ok(response(Some(RunnerReadiness {
                schema: RUNNER_READINESS_SCHEMA.to_string(),
                runner_id: "runner-a".to_string(),
                connected: true,
                accepting_jobs: true,
                active_job_count: 0,
                capacity: Some(1),
                reasons: Vec::new(),
            }))),
        );

        assert_eq!(availability, AgentTaskLoopRunnerAvailability::Available);
    }

    #[test]
    fn non_accepting_readiness_preserves_the_unavailable_projection() {
        let availability = availability_from_response(
            "runner-a",
            Ok(response(Some(RunnerReadiness {
                schema: RUNNER_READINESS_SCHEMA.to_string(),
                runner_id: "runner-a".to_string(),
                connected: false,
                accepting_jobs: false,
                active_job_count: 0,
                capacity: Some(1),
                reasons: vec!["daemon_not_fresh".to_string(), "stale_jobs".to_string()],
            }))),
        );

        assert_eq!(
            availability,
            AgentTaskLoopRunnerAvailability::Unavailable {
                reason: "runner `runner-a` is not available for controller action execution: connected=false, reasons=daemon_not_fresh,stale_jobs".to_string(),
            }
        );
    }

    #[test]
    fn unknown_runner_api_failures_are_unavailable() {
        let availability =
            RunnerAvailability.controller_runner_availability("runner-that-does-not-exist");

        assert_eq!(
            availability,
            AgentTaskLoopRunnerAvailability::Unavailable {
                reason: "runner `runner-that-does-not-exist` is not available for controller action execution: Runner not found".to_string(),
            }
        );
    }

    #[test]
    fn empty_readiness_envelopes_are_unavailable() {
        let availability = availability_from_response("runner-a", Ok(response(None)));

        assert_eq!(
            availability,
            AgentTaskLoopRunnerAvailability::Unavailable {
                reason: "runner `runner-a` is not available for controller action execution: Runner API readiness response omitted both readiness and failure".to_string(),
            }
        );
    }
}
