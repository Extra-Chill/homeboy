//! Job admission versus daemon mutation for one runner.
//!
//! A daemon mutation (start, replacement, recovery) reserves the runner under
//! its state lock, then releases the lock for the remote I/O. Job admission
//! observes the durable reservation and fails closed until the mutation
//! completes or its owner is proven dead. The controller also keeps the ids
//! of the jobs it admitted to the runner's daemon until it observes them
//! terminal; while any remain, a mutation sees an [`AdmissionFence`] and must
//! not create or replace the daemon that owns them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use homeboy_core::error::{Error, Result};
use homeboy_core::paths;
use homeboy_core::process::{
    process_identity_state_with_start_identity, process_start_identity, ProcessIdentityState,
    ProcessStartIdentity,
};

use crate::runner_state_lock::{
    read_durable_json, remove_if_exists, runner_state_dir_in_root, with_runner_registry_lock,
    write_durable_json,
};

/// Unresolved work a daemon mutation must not orphan.
#[derive(Debug, Clone)]
pub(crate) struct AdmissionFence {
    pub active_job_count: usize,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct AdmissionReservation {
    operation_id: String,
    owner_pid: u32,
    owner_start_identity: ProcessStartIdentity,
    created_at: String,
    operation: String,
}

/// Jobs this controller admitted to the runner's daemon and has not yet
/// observed terminal, with the wall-clock millisecond of their admission.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct AdmittedJobs {
    runner_id: String,
    jobs: BTreeMap<String, i64>,
}

fn admission_reservation_path(runner_id: &str) -> Result<PathBuf> {
    Ok(runner_state_dir_in_root(&paths::homeboy()?, runner_id)
        .join("admission-mutation-reservation.json"))
}

/// Peer session discovery treats every JSON child of the runner directory as a
/// session record, so the admitted-job ledger is a non-JSON sidecar.
fn admitted_jobs_path(runner_id: &str) -> Result<PathBuf> {
    Ok(runner_state_dir_in_root(&paths::homeboy()?, runner_id).join("admitted-jobs.state"))
}

fn read_admitted_jobs(path: &Path, runner_id: &str) -> Result<AdmittedJobs> {
    let ledger = read_durable_json::<AdmittedJobs>(path)?.unwrap_or_else(|| AdmittedJobs {
        runner_id: runner_id.to_string(),
        jobs: BTreeMap::new(),
    });
    if ledger.runner_id != runner_id {
        return Err(Error::config_invalid_value(
            "admitted_jobs.runner_id",
            Some(ledger.runner_id),
            format!("admitted job ledger must match runner `{runner_id}`"),
        ));
    }
    Ok(ledger)
}

/// Reserve daemon mutation under the runner state lock, then release it for
/// remote I/O. Job admission observes the durable reservation and fails
/// closed until the mutation completes or its reservation is cleared.
pub(crate) fn with_admission_fence<T>(
    runner_id: &str,
    operation_name: &str,
    operation: impl FnOnce(Option<&AdmissionFence>) -> Result<T>,
) -> Result<T> {
    let (reservation, fence) = with_runner_registry_lock(runner_id, || {
        if let Some(reservation) = active_admission_reservation_locked(runner_id)? {
            return Err(admission_reservation_error(runner_id, &reservation));
        }
        let reservation = AdmissionReservation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            owner_pid: std::process::id(),
            owner_start_identity: process_start_identity(std::process::id())
                .map_err(Error::internal_unexpected)?
                .ok_or_else(|| {
                    Error::internal_unexpected(
                        "current process exited while reserving daemon mutation",
                    )
                })?,
            created_at: Utc::now().to_rfc3339(),
            operation: operation_name.to_string(),
        };
        write_durable_json(&admission_reservation_path(runner_id)?, &reservation)?;
        let active_job_count = read_admitted_jobs(&admitted_jobs_path(runner_id)?, runner_id)?
            .jobs
            .len();
        let fence = (active_job_count > 0).then_some(AdmissionFence { active_job_count });
        Ok((reservation, fence))
    })?;
    let result = operation(fence.as_ref());
    let clear = clear_owned_admission_reservation(runner_id, &reservation);
    match (result, clear) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Err(error), Err(_)) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Record a job the runner's daemon accepted. Fails closed while a daemon
/// mutation owns the runner, exactly as a new admission would.
pub(crate) fn record_admitted_job(runner_id: &str, job_id: &str) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
        if let Some(reservation) = active_admission_reservation_locked(runner_id)? {
            return Err(admission_reservation_error(runner_id, &reservation));
        }
        let path = admitted_jobs_path(runner_id)?;
        let mut ledger = read_admitted_jobs(&path, runner_id)?;
        ledger
            .jobs
            .entry(job_id.to_string())
            .or_insert_with(|| Utc::now().timestamp_millis());
        write_durable_json(&path, &ledger)
    })
}

/// Settle one exact job after the controller authoritatively read its
/// terminal snapshot. Unknown or in-flight jobs never reach this boundary, so
/// they keep fencing daemon mutation.
pub(crate) fn settle_admitted_job(runner_id: &str, job_id: &str) -> Result<bool> {
    with_runner_registry_lock(runner_id, || {
        let path = admitted_jobs_path(runner_id)?;
        let mut ledger = read_admitted_jobs(&path, runner_id)?;
        if ledger.jobs.remove(job_id).is_none() {
            return Ok(false);
        }
        write_durable_json(&path, &ledger)?;
        Ok(true)
    })
}

/// The runner's daemon authoritatively reported zero active jobs in an
/// observation that started at `observed_since_ms`. Every job admitted before
/// that observation is therefore terminal; jobs admitted during or after it
/// are kept. The caller holds the runner state lock.
pub(crate) fn release_admitted_jobs_observed_idle_locked(
    runner_id: &str,
    observed_since_ms: i64,
) -> Result<()> {
    let path = admitted_jobs_path(runner_id)?;
    let mut ledger = read_admitted_jobs(&path, runner_id)?;
    let before = ledger.jobs.len();
    ledger
        .jobs
        .retain(|_, admitted_at_ms| *admitted_at_ms >= observed_since_ms);
    if ledger.jobs.len() == before {
        return Ok(());
    }
    write_durable_json(&path, &ledger)
}

/// Read a reservation while holding the runner state lock. A process that has
/// exited or whose PID was reused cannot still own a remote mutation, so its
/// durable reservation is reclaimed before a new owner proceeds.
fn active_admission_reservation_locked(runner_id: &str) -> Result<Option<AdmissionReservation>> {
    let path = admission_reservation_path(runner_id)?;
    let Some(reservation) = read_durable_json::<AdmissionReservation>(&path)? else {
        return Ok(None);
    };
    match process_identity_state_with_start_identity(
        reservation.owner_pid,
        None,
        Some(&reservation.owner_start_identity),
    ) {
        ProcessIdentityState::Dead | ProcessIdentityState::IdentityMismatch => {
            remove_if_exists(&path)?;
            Ok(None)
        }
        ProcessIdentityState::Live | ProcessIdentityState::Unverifiable => Ok(Some(reservation)),
    }
}

fn clear_owned_admission_reservation(
    runner_id: &str,
    reservation: &AdmissionReservation,
) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
        let path = admission_reservation_path(runner_id)?;
        let Some(current) = read_durable_json::<AdmissionReservation>(&path)? else {
            return Ok(());
        };
        if current.operation_id == reservation.operation_id
            && current.owner_pid == reservation.owner_pid
            && current.owner_start_identity == reservation.owner_start_identity
        {
            remove_if_exists(&path)?;
        }
        Ok(())
    })
}

fn admission_reservation_error(runner_id: &str, reservation: &AdmissionReservation) -> Error {
    let mut error = Error::validation_invalid_argument(
        "runner_mutation_reservation",
        format!(
            "runner `{runner_id}` daemon mutation `{}` is still owned by PID {}; retry admission after it completes or run `homeboy runner reconcile {runner_id}` to recover a dead owner",
            reservation.operation, reservation.owner_pid,
        ),
        Some(runner_id.to_string()),
        Some(vec![format!("homeboy runner reconcile {runner_id}")]),
    )
    .with_retryable(true);
    error.details["reservation_operation"] = serde_json::json!(reservation.operation);
    error.details["reservation_created_at"] = serde_json::json!(reservation.created_at);
    error
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn reservation(
        operation: &str,
        pid: u32,
        identity: ProcessStartIdentity,
    ) -> AdmissionReservation {
        AdmissionReservation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            owner_pid: pid,
            owner_start_identity: identity,
            created_at: "2026-08-03T00:00:00Z".to_string(),
            operation: operation.to_string(),
        }
    }

    fn observed_fence(runner_id: &str) -> Option<usize> {
        with_admission_fence(runner_id, "connect", |fence| {
            Ok(fence.map(|fence| fence.active_job_count))
        })
        .expect("check admission fence")
    }

    #[test]
    fn live_reservation_blocks_admission_with_retryable_reconcile_action() {
        crate::test_support::with_isolated_home(|_| {
            let current_identity = process_start_identity(std::process::id())
                .expect("inspect test process")
                .expect("test process is live");
            write_durable_json(
                &admission_reservation_path("runner-a").expect("reservation path"),
                &reservation("ensure_remote_daemon", std::process::id(), current_identity),
            )
            .expect("write live reservation");

            let error = record_admitted_job("runner-a", "job-a")
                .expect_err("live mutation blocks admission");

            assert_eq!(error.code.as_str(), "validation.invalid_argument");
            assert_eq!(error.retryable, Some(true));
            assert_eq!(error.details["field"], "runner_mutation_reservation");
            assert_eq!(
                error.details["reservation_operation"],
                "ensure_remote_daemon"
            );
            assert_eq!(
                error.details["tried"],
                serde_json::json!(["homeboy runner reconcile runner-a"])
            );
        });
    }

    #[test]
    fn failed_mutation_clears_its_owned_reservation_before_returning() {
        crate::test_support::with_isolated_home(|_| {
            let error = with_admission_fence("runner-a", "ensure_remote_daemon", |_| {
                Err::<(), _>(Error::internal_unexpected("remote mutation failed"))
            })
            .expect_err("operation failure is returned");
            assert_eq!(error.message, "remote mutation failed");
            assert!(
                !admission_reservation_path("runner-a")
                    .expect("reservation path")
                    .exists(),
                "an error path must not strand its reservation"
            );
            record_admitted_job("runner-a", "job-a")
                .expect("admission proceeds after failed mutation cleanup");
        });
    }

    #[test]
    fn concurrent_admission_observes_mutation_reservation_without_waiting_for_remote_work() {
        crate::test_support::with_isolated_home(|_| {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let holder_barrier = std::sync::Arc::clone(&barrier);
            let holder = std::thread::spawn(move || {
                with_admission_fence("runner-a", "ensure_remote_daemon", |_| {
                    holder_barrier.wait();
                    holder_barrier.wait();
                    Ok(())
                })
            });

            barrier.wait();
            let error = record_admitted_job("runner-a", "job-a")
                .expect_err("admission races a durable mutation reservation");
            assert_eq!(error.retryable, Some(true));
            assert_eq!(
                error.details["reservation_operation"],
                "ensure_remote_daemon"
            );
            barrier.wait();
            holder
                .join()
                .expect("mutation owner thread")
                .expect("mutation succeeds");
            record_admitted_job("runner-a", "job-a")
                .expect("admission succeeds after reservation release");
        });
    }

    #[test]
    fn a_second_mutation_is_refused_while_the_first_owns_the_runner() {
        crate::test_support::with_isolated_home(|_| {
            let error = with_admission_fence("runner-a", "ensure_remote_daemon", |_| {
                with_admission_fence("runner-a", "recover_missing_lease_state", |_| Ok(()))
            })
            .expect_err("nested mutation is refused");
            assert_eq!(
                error.details["reservation_operation"],
                "ensure_remote_daemon"
            );
        });
    }

    #[test]
    fn dead_reservation_is_reclaimed_before_admission() {
        crate::test_support::with_isolated_home(|_| {
            let mut owner = std::process::Command::new("sh")
                .args(["-c", "sleep 60"])
                .spawn()
                .expect("start reservation owner");
            let identity = process_start_identity(owner.id())
                .expect("inspect child")
                .expect("child is live");
            owner.kill().expect("stop reservation owner");
            owner.wait().expect("reap reservation owner");
            write_durable_json(
                &admission_reservation_path("runner-a").expect("reservation path"),
                &reservation("recover_missing_lease_state", owner.id(), identity),
            )
            .expect("write dead reservation");

            record_admitted_job("runner-a", "job-a").expect("dead owner reservation is reclaimed");

            assert!(!admission_reservation_path("runner-a")
                .expect("reservation path")
                .exists());
        });
    }

    #[test]
    fn admitted_jobs_fence_mutation_until_each_is_observed_terminal() {
        crate::test_support::with_isolated_home(|_| {
            assert_eq!(observed_fence("runner-a"), None);
            record_admitted_job("runner-a", "job-a").expect("admit A");
            record_admitted_job("runner-a", "job-b").expect("admit B");
            record_admitted_job("runner-a", "job-b").expect("re-admitting B is idempotent");
            assert_eq!(observed_fence("runner-a"), Some(2));

            assert!(settle_admitted_job("runner-a", "job-a").expect("settle A"));
            assert!(
                !settle_admitted_job("runner-a", "job-a").expect("settle A again"),
                "settlement is idempotent"
            );
            assert!(!settle_admitted_job("runner-a", "job-unknown").expect("unknown job"));
            assert_eq!(observed_fence("runner-a"), Some(1));

            assert!(settle_admitted_job("runner-a", "job-b").expect("settle B"));
            assert_eq!(observed_fence("runner-a"), None);
        });
    }

    #[test]
    fn an_idle_observation_releases_only_jobs_admitted_before_it_started() {
        crate::test_support::with_isolated_home(|_| {
            record_admitted_job("runner-a", "job-before").expect("admit before");
            std::thread::sleep(std::time::Duration::from_millis(5));
            let observed_since_ms = Utc::now().timestamp_millis();
            std::thread::sleep(std::time::Duration::from_millis(5));
            record_admitted_job("runner-a", "job-during").expect("admit during");

            with_runner_registry_lock("runner-a", || {
                release_admitted_jobs_observed_idle_locked("runner-a", observed_since_ms)
            })
            .expect("release idle observation");

            assert_eq!(observed_fence("runner-a"), Some(1));
            assert!(settle_admitted_job("runner-a", "job-during").expect("kept job"));
        });
    }

    #[test]
    fn admitted_jobs_are_scoped_to_their_runner() {
        crate::test_support::with_isolated_home(|_| {
            record_admitted_job("runner-a", "job-a").expect("admit on A");
            assert_eq!(observed_fence("runner-b"), None);
            assert_eq!(observed_fence("runner-a"), Some(1));
        });
    }

    #[test]
    fn a_terminal_cancel_response_settles_the_admitted_job() {
        crate::test_support::with_isolated_home(|_| {
            let job_id = uuid::Uuid::new_v4();
            record_admitted_job("runner-a", &job_id.to_string()).expect("admit queued job");
            assert_eq!(observed_fence("runner-a"), Some(1));

            let cancelled_job = homeboy_core::api_jobs::Job {
                id: job_id,
                operation: "runner.exec".to_string(),
                status: homeboy_core::api_jobs::JobStatus::Cancelled,
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: None,
                finished_at_ms: Some(2),
                event_count: 1,
                source_snapshot: None,
                path_materialization_plan: None,
                stale_reason: None,
                daemon_lease_id: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
                runner_job_projection: None,
            };
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fixture");
            let address = listener.local_addr().expect("fixture address");
            let server = std::thread::spawn(move || {
                use std::io::{Read, Write};
                let (mut stream, _) = listener.accept().expect("cancel request");
                let mut bytes = [0; 4096];
                let count = stream.read(&mut bytes).expect("read cancel request");
                let request = String::from_utf8_lossy(&bytes[..count]);
                assert!(
                    request.starts_with(&format!("POST /jobs/{job_id}/cancel ")),
                    "{request}"
                );
                let body = serde_json::json!({
                    "success": true,
                    "data": {"body": {
                        "job": serde_json::to_value(&cancelled_job).expect("cancel job"),
                        "events": [],
                    }}
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("cancel response");
            });
            let mut session = crate::test_support::direct_ssh_session("lease-cancel");
            session.runner_id = "runner-a".to_string();
            session.local_url = Some(format!("http://{address}"));

            let (job, _events) =
                crate::runner_job_cancel_for_session(&session, &job_id.to_string(), "fixture")
                    .expect("cancel queued job");
            assert_eq!(job.status, homeboy_core::api_jobs::JobStatus::Cancelled);
            server.join().expect("fixture server");

            assert_eq!(
                observed_fence("runner-a"),
                None,
                "the settled job releases the admission fence"
            );
        });
    }
}
