//! Runner-side implementation of core's step runner-eligibility hook.
//!
//! A `step` job that waits for a runner spawns only once that runner's live
//! readiness report accepts jobs, keeping `RunnerStatusReport` inside the
//! runner layer.

use homeboy_runner_contract::{
    RunnerApiReadinessRequest, RunnerApiReadinessResponse, RunnerReadiness,
    RUNNER_API_READINESS_REQUEST_SCHEMA, RUNNER_API_V1,
};

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

/// Whether `runner_id` accepts jobs right now, from its live readiness report.
/// Unknown or failed readiness is not eligibility.
fn runner_accepts_jobs(runner_id: &str) -> std::result::Result<(), String> {
    crate::RunnerDiscoveryService::readiness_api(&RunnerApiReadinessRequest {
        schema: RUNNER_API_READINESS_REQUEST_SCHEMA.to_string(),
        api_version: RUNNER_API_V1,
        runner_id: runner_id.to_string(),
    })
    .map_err(|error| error.to_string())
    .and_then(readiness_from_response)
    .and_then(eligibility_from_readiness)
}

fn eligibility_from_readiness(readiness: RunnerReadiness) -> std::result::Result<(), String> {
    if readiness.accepting_jobs {
        Ok(())
    } else {
        Err(readiness
            .reasons
            .into_iter()
            .next()
            .unwrap_or_else(|| format!("runner {} is not accepting jobs", readiness.runner_id)))
    }
}

/// Register the runner eligibility check with core. Called once at startup.
pub fn register() {
    // A `step` that waits for a runner spawns only once that runner accepts jobs.
    homeboy_core::daemon::step_job::register_step_runner_eligibility(std::sync::Arc::new(
        runner_accepts_jobs,
    ));
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
    fn readiness_reports_accepting_jobs() {
        let readiness = readiness_from_response(response(Some(RunnerReadiness {
            schema: RUNNER_READINESS_SCHEMA.to_string(),
            runner_id: "runner-a".to_string(),
            connected: true,
            accepting_jobs: true,
            active_job_count: Some(0),
            capacity: Some(1),
            reasons: Vec::new(),
        })))
        .expect("readiness");

        assert!(eligibility_from_readiness(readiness.clone()).is_ok());
        let refused = RunnerReadiness {
            accepting_jobs: false,
            reasons: vec!["controller build mismatch".into(), "capacity full".into()],
            ..readiness
        };
        assert_eq!(
            eligibility_from_readiness(refused).unwrap_err(),
            "controller build mismatch"
        );
    }

    #[test]
    fn unknown_runners_are_not_eligible() {
        assert!(runner_accepts_jobs("runner-that-does-not-exist").is_err());
    }

    #[test]
    fn empty_readiness_envelopes_are_errors() {
        assert_eq!(
            readiness_from_response(response(None)).unwrap_err(),
            "Runner API readiness response omitted both readiness and failure"
        );
    }
}
