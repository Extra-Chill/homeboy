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
    homeboy_core::daemon::step_job::register_step_runner_wait_context(std::sync::Arc::new(
        |runner| {
            let snapshot = crate::runner_status_admission_snapshot(runner).ok();
            serde_json::json!({
                "controller_build": homeboy_core::build_identity::current(),
                "runner_build": snapshot.as_ref().and_then(|snapshot| snapshot.summary.daemon_build_identity.as_ref()),
                "pressure": snapshot.as_ref().and_then(|snapshot| snapshot.summary.pressure.as_ref()),
                "pressure_thresholds": snapshot.as_ref()
                    .and_then(|snapshot| snapshot.status.daemon_freshness.as_ref())
                    .and_then(|freshness| freshness.runtime_paths.as_ref())
                    .map(|paths| &paths.pressure_thresholds),
                "pressure_holders": snapshot.as_ref().map(|snapshot| &snapshot.status.active_jobs),
            })
        },
    ));
    // A `step` that waits for a runner spawns only once that runner accepts jobs.
    homeboy_core::daemon::step_job::register_step_runner_eligibility(std::sync::Arc::new(
        runner_accepts_jobs,
    ));
    homeboy_core::daemon::step_job::register_step_runner_lab_waiter(std::sync::Arc::new(
        lab_waiter_heartbeat,
    ));
}

/// Register, renew, or release a waiting step's reservation on the runner's
/// Lab daemon. Any failure (no session, older daemon, transport) is `None`:
/// the wait then keeps its controller-local ranking.
fn lab_waiter_heartbeat(
    runner_id: &str,
    beat: &homeboy_core::lab_capacity::RemoteWaiterHeartbeat,
) -> Option<serde_json::Value> {
    let session = crate::connection::read_session(runner_id).ok()??;
    let data = crate::execution::daemon_api_post_json_for_session_with_timeout(
        &session,
        "/capacity/waiters",
        &serde_json::to_value(beat).ok()?,
        None,
        std::time::Duration::from_secs(5),
    )
    .ok()?;
    crate::execution::canonical_daemon_body(&data, "capacity waiter reply")
        .ok()
        .cloned()
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
