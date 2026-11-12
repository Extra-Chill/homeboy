//! A drain queues new admissions for the replacement and names the protected
//! jobs that still hold it open (#17707).

use super::controller_job_driver::{
    ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use super::lifecycle_plan::{Plan, DRAIN_HOLDER_NO_PROGRESS_MS};
use super::tests::write_test_lease;
use super::*;
use crate::test_support::with_isolated_home;
use std::sync::atomic::AtomicUsize;

const JOB_TYPE: &str = "test.17707-drain-queue";
static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);

/// Records each execution; `run_id` in the request links a durable run.
struct DrainQueueDriver;

impl ControllerJobDriver for DrainQueueDriver {
    fn job_type(&self) -> &'static str {
        JOB_TYPE
    }
    fn version(&self) -> u32 {
        1
    }
    fn public_request(&self, request: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(request.clone())
    }
    fn public_progress(&self, progress: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(progress.clone())
    }
    fn public_result(&self, result: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(result.clone())
    }
    fn public_error(&self, error: &Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: error.to_string(),
            data: json!({}),
        }
    }
    fn validate_secret_references(&self, _: &serde_json::Value) -> Result<()> {
        Ok(())
    }
    fn linked_durable_run_id(&self, request: &serde_json::Value) -> Option<String> {
        request["run_id"].as_str().map(str::to_owned)
    }
    fn execute(&self, _: serde_json::Value, _: ControllerJobHandle) -> Result<serde_json::Value> {
        EXECUTIONS.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
    fn cancel(&self, _: &serde_json::Value) -> Result<()> {
        Ok(())
    }
}

fn register_driver() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(Arc::new(DrainQueueDriver))
            .expect("register drain-queue driver");
    });
}

fn lease_store(lease_id: &str) -> JobStore {
    JobStore::open_without_reconciliation(paths::observation_db().expect("jobs path"))
        .expect("durable store")
        .with_daemon_scope(daemon_scope_for_state_path(&state_path().expect("state")))
        .with_daemon_lease(lease_id.to_string())
}

fn payload(response: &HttpResponse) -> &serde_json::Value {
    &response.body["body"]
}

#[test]
fn admission_during_drain_queues_for_the_replacement_daemon_to_admit() {
    with_isolated_home(|_| {
        register_driver();
        write_test_lease("drain-lease");
        let store = lease_store("drain-lease");
        drain_then_replace("drain-lease").expect("drain");

        let response = route_with_job_store_and_body(
            "POST",
            "/controller/jobs",
            Some(json!({
                "type": JOB_TYPE,
                "version": 1,
                "idempotency_key": "drain-queued",
                "request": {},
            })),
            &store,
        );
        assert_eq!(response.status_code, 200, "{}", response.body);
        let admitted = payload(&response);
        assert_eq!(
            admitted["admission"]["status"],
            "pending_resource_admission"
        );
        assert_eq!(admitted["admission"]["reason"], "daemon_draining");
        let job_id = Uuid::parse_str(admitted["job"]["id"].as_str().unwrap()).unwrap();
        // The draining lease never starts it, and it does not hold the drain.
        let started =
            route_with_job_store("POST", &format!("/controller/jobs/{job_id}/start"), &store);
        assert_eq!(payload(&started)["job"]["status"], "queued");
        assert_eq!(daemon_freshness_report(&store).unwrap().active_jobs, 0);
        assert!(store.has_pending_admissions());
        assert_eq!(EXECUTIONS.load(Ordering::SeqCst), 0);

        // The replacement's startup admission starts the queued job.
        write_test_lease("replacement-lease");
        let replacement = lease_store("replacement-lease");
        recover_controller_jobs(&replacement);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !replacement.get(job_id).unwrap().status.is_terminal() {
            assert!(Instant::now() < deadline, "queued job was never admitted");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            replacement.get(job_id).unwrap().status,
            JobStatus::Succeeded
        );
        assert_eq!(EXECUTIONS.load(Ordering::SeqCst), 1);
        assert!(!replacement.has_pending_admissions());
    });
}

#[test]
fn reservation_holds_admission_without_failing_and_release_resumes() {
    with_isolated_home(|_| {
        register_driver();
        write_test_lease("reservation-lease");
        let store = lease_store("reservation-lease");
        let reservation =
            reservation::update(Some(json!({"quiet_below": 0, "window": 1}))).unwrap();
        let request = ControllerJobRequest {
            job_type: JOB_TYPE.into(),
            version: 1,
            idempotency_key: "reserved-job".into(),
            active_idempotency_key: None,
            request: json!({}),
        };
        let admitted = admit_controller_job(request, &store).unwrap();
        let id = admitted.job.id;
        assert!(admitted
            .pending_admission(&store)
            .unwrap()
            .contains(reservation["id"].as_str().unwrap()));
        assert_eq!(
            start_controller_job(id, &store).unwrap().status,
            JobStatus::Queued
        );
        reservation::update(Some(json!({"release": reservation["id"]}))).unwrap();
        recover_controller_jobs(&store);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !store.get(id).unwrap().status.is_terminal() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(store.get(id).unwrap().status, JobStatus::Succeeded);
    });
}

#[test]
fn daemon_plan_during_a_drain_lists_the_holder_jobs() {
    with_isolated_home(|_| {
        register_driver();
        let path = state_path().expect("state path");
        let _lock = acquire_daemon_owner_lock_at(&path, Duration::ZERO).expect("serve lock");
        write_test_lease("holder-lease");
        let store = lease_store("holder-lease");
        let request = ControllerJobRequest {
            job_type: JOB_TYPE.to_string(),
            version: 1,
            idempotency_key: "continuation".to_string(),
            active_idempotency_key: None,
            request: json!({ "run_id": "run-17707" }),
        };
        let crate::api_jobs::ControllerJobSubmissionOutcome::Submitted(job_id) = store
            .admit_controller_job(
                format!("controller.{JOB_TYPE}"),
                request.idempotency_key.clone(),
                controller_job_state(&request).unwrap(),
            )
            .unwrap()
        else {
            panic!("fresh continuation was replayed");
        };
        store.start_controller_execution(job_id).unwrap();
        drain_then_replace("holder-lease").expect("drain");

        let status = read_status().expect("status");
        let Plan::Draining { reason, holders } = recovery_actions::plan_recovery(&status).lifecycle
        else {
            panic!("a drain plans its holders, not a bare wait");
        };
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].job_id, job_id);
        assert_eq!(holders[0].operation, format!("controller.{JOB_TYPE}"));
        assert_eq!(holders[0].owning_run_id.as_deref(), Some("run-17707"));
        assert!(!holders[0].blocker);
        assert!(reason.contains(&job_id.to_string()), "{reason}");
        assert_eq!(
            status.admission_report()["drain_holders"][0]["job_id"],
            job_id.to_string()
        );

        // Past the no-progress bound the continuation is the drain blocker.
        let later = crate::api_jobs::timestamp_ms() + DRAIN_HOLDER_NO_PROGRESS_MS;
        let stalled = &lifecycle_plan::drain_holders(&status, later)[0];
        assert!(stalled.blocker);
        assert_eq!(
            stalled.recovery_command.as_deref(),
            Some("homeboy runs cancel run-17707")
        );
    });
}
