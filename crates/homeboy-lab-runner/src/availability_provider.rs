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
///
/// A waiting step whose runner's controller tunnel is down attempts the same
/// reconnect `homeboy runner connect` performs, at most once per
/// [`homeboy_core::substrate_wait::RUNNER_RECONNECT_INTERVAL`] per runner
/// (#18744). A successful reconnect is re-probed at once.
fn runner_accepts_jobs(runner_id: &str) -> std::result::Result<(), String> {
    runner_accepts_jobs_with(
        runner_id,
        runner_readiness,
        controller_tunnel_connected,
        |runner| claim_reconnect(runner, std::time::SystemTime::now()),
        reconnect_runner,
    )
}

fn runner_readiness(runner_id: &str) -> std::result::Result<RunnerReadiness, String> {
    crate::RunnerDiscoveryService::readiness_api(&RunnerApiReadinessRequest {
        schema: RUNNER_API_READINESS_REQUEST_SCHEMA.to_string(),
        api_version: RUNNER_API_V1,
        runner_id: runner_id.to_string(),
    })
    .map_err(|error| error.to_string())
    .and_then(readiness_from_response)
}

/// Controller-scoped: a peer controller's live session does not count.
fn controller_tunnel_connected(runner_id: &str) -> bool {
    crate::connection::status_current_controller(runner_id).is_ok_and(|report| report.connected)
}

fn claim_reconnect(runner_id: &str, now: std::time::SystemTime) -> bool {
    homeboy_core::paths::runner_sessions_dir()
        .and_then(|dir| {
            homeboy_core::substrate_wait::claim_runner_reconnect_attempt(&dir, runner_id, now)
        })
        .unwrap_or(false)
}

fn reconnect_runner(runner_id: &str) -> std::result::Result<(), String> {
    homeboy_core::log_status!(
        "wait",
        "runner {runner_id} is disconnected; attempting `homeboy runner connect {runner_id}`"
    );
    match crate::connection::connect_converging_binary(runner_id) {
        Ok((report, 0)) if report.connected => Ok(()),
        Ok((_, code)) => Err(format!("runner connect exited {code}")),
        Err(error) => Err(error.to_string()),
    }
}

fn runner_accepts_jobs_with(
    runner_id: &str,
    readiness: impl Fn(&str) -> std::result::Result<RunnerReadiness, String>,
    tunnel_connected: impl Fn(&str) -> bool,
    claim: impl Fn(&str) -> bool,
    reconnect: impl Fn(&str) -> std::result::Result<(), String>,
) -> std::result::Result<(), String> {
    let observed = readiness(runner_id)?;
    if observed.accepting_jobs
        || !homeboy_core::substrate_wait::runner_reconnect_warranted(
            observed.connected,
            observed.reasons.iter().map(String::as_str),
        )
        || tunnel_connected(runner_id)
        || !claim(runner_id)
    {
        return eligibility_from_readiness(observed);
    }
    match reconnect(runner_id) {
        Ok(()) => readiness(runner_id).and_then(eligibility_from_readiness),
        Err(error) => {
            homeboy_core::log_status!(
                "wait",
                "runner {runner_id} reconnect failed: {error}; next action: homeboy runner connect {runner_id}"
            );
            eligibility_from_readiness(observed)
        }
    }
}

/// A refused readiness reports its first reason, except that a disconnected
/// runner reports `not_connected` so the wait names the reconnect it needs.
fn eligibility_from_readiness(readiness: RunnerReadiness) -> std::result::Result<(), String> {
    if readiness.accepting_jobs {
        return Ok(());
    }
    if readiness
        .reasons
        .iter()
        .any(|reason| reason == "not_connected")
    {
        return Err("not_connected".to_string());
    }
    Err(readiness
        .reasons
        .into_iter()
        .next()
        .unwrap_or_else(|| format!("runner {} is not accepting jobs", readiness.runner_id)))
}

/// Register the runner eligibility check with core. Called once at startup.
pub fn register() {
    homeboy_core::daemon::step_job::register_step_runner_wait_context(std::sync::Arc::new(
        |runner| {
            let snapshot = crate::runner_admission_snapshot(
                runner,
                None,
                crate::RunnerAdmissionReadiness::Configured,
            )
            .ok();
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

    fn readiness(connected: bool, accepting: bool, reasons: &[&str]) -> RunnerReadiness {
        RunnerReadiness {
            schema: RUNNER_READINESS_SCHEMA.to_string(),
            runner_id: "homeboy-lab".to_string(),
            connected,
            accepting_jobs: accepting,
            active_job_count: None,
            capacity: None,
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
        }
    }

    /// #18744: a disconnected runner is reconnected once and re-probed; a busy
    /// runner, a runner whose own controller tunnel is up, or a throttled
    /// claim never reconnects.
    #[test]
    fn waiting_step_reconnects_only_disconnected_runners() {
        use std::cell::{Cell, RefCell};
        struct Case {
            first: RunnerReadiness,
            tunnel_up: bool,
            claim: bool,
            reconnect_ok: bool,
            reconnects: usize,
            result: std::result::Result<(), String>,
        }
        let disconnected = readiness(false, false, &["active_jobs_not_queried", "not_connected"]);
        for case in [
            Case {
                first: disconnected.clone(),
                tunnel_up: false,
                claim: true,
                reconnect_ok: true,
                reconnects: 1,
                result: Ok(()),
            },
            Case {
                first: disconnected.clone(),
                tunnel_up: false,
                claim: true,
                reconnect_ok: false,
                reconnects: 1,
                result: Err("not_connected".into()),
            },
            Case {
                first: disconnected.clone(),
                tunnel_up: false,
                claim: false,
                reconnect_ok: true,
                reconnects: 0,
                result: Err("not_connected".into()),
            },
            Case {
                first: disconnected.clone(),
                tunnel_up: true,
                claim: true,
                reconnect_ok: true,
                reconnects: 0,
                result: Err("not_connected".into()),
            },
            Case {
                first: readiness(true, false, &["capacity_reached"]),
                tunnel_up: false,
                claim: true,
                reconnect_ok: true,
                reconnects: 0,
                result: Err("capacity_reached".into()),
            },
            Case {
                first: readiness(true, false, &["pressure"]),
                tunnel_up: false,
                claim: true,
                reconnect_ok: true,
                reconnects: 0,
                result: Err("pressure".into()),
            },
        ] {
            let reconnects = Cell::new(0);
            let probes = RefCell::new(vec![readiness(true, true, &[]), case.first.clone()]);
            let result = runner_accepts_jobs_with(
                "homeboy-lab",
                |_| Ok(probes.borrow_mut().pop().expect("probe")),
                |_| case.tunnel_up,
                |_| case.claim,
                |_| {
                    reconnects.set(reconnects.get() + 1);
                    if case.reconnect_ok {
                        Ok(())
                    } else {
                        Err("ssh failed".into())
                    }
                },
            );
            assert_eq!(result, case.result, "{:?}", case.first.reasons);
            assert_eq!(
                reconnects.get(),
                case.reconnects,
                "{:?}",
                case.first.reasons
            );
        }
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
