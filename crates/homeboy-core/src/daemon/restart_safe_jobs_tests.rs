//! A stale daemon is replaceable when every active job survives the handoff
//! (#17666): driver-recovery jobs resume in the successor, live children do not.

use super::*;
use crate::api_jobs::{ControllerJobState, ControllerJobSubmissionOutcome};
use crate::test_support::with_isolated_home;

const JOB_TYPE: &str = "test.restart-safety";

/// Driver-owned work: a request naming `live_pid` is supervised by that live
/// process; any other request has no external execution left.
struct RestartSafetyDriver;

impl controller_job_driver::ControllerJobDriver for RestartSafetyDriver {
    fn job_type(&self) -> &'static str {
        JOB_TYPE
    }
    fn version(&self) -> u32 {
        1
    }
    fn public_request(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(value.clone())
    }
    fn public_progress(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(value.clone())
    }
    fn public_result(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(value.clone())
    }
    fn public_error(&self, _: &Error) -> controller_job_driver::ControllerJobPublicError {
        controller_job_driver::ControllerJobPublicError {
            message: "fixture".to_string(),
            data: json!({}),
        }
    }
    fn validate_secret_references(&self, _: &serde_json::Value) -> Result<()> {
        Ok(())
    }
    fn execution_owner(
        &self,
        request: &serde_json::Value,
        _checkpoint: Option<&serde_json::Value>,
    ) -> Result<Option<controller_job_driver::ControllerJobExecutionOwner>> {
        use controller_job_driver::ControllerJobExecutionOwner as Owner;
        let Some(pid) = request["live_pid"].as_u64().map(|pid| pid as u32) else {
            return Ok(Some(Owner::None));
        };
        Ok(Some(
            match crate::process::process_start_identity(pid).ok().flatten() {
                Some(identity) => Owner::supervised(pid, identity),
                None => Owner::Unavailable,
            },
        ))
    }
    fn execute(
        &self,
        _: serde_json::Value,
        _: controller_job_driver::ControllerJobHandle,
    ) -> Result<serde_json::Value> {
        panic!("restart-safety fixture must not execute")
    }
    fn cancel(&self, _: &serde_json::Value) -> Result<()> {
        panic!("restart-safety fixture must not cancel")
    }
}

/// Admit one running controller job of this daemon's lease.
fn running_job(store: &JobStore, request: serde_json::Value) -> Uuid {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(std::sync::Arc::new(
            RestartSafetyDriver,
        ))
        .expect("register restart-safety driver");
    });
    let outcome = store
        .admit_controller_job(
            format!("controller.{JOB_TYPE}"),
            Uuid::new_v4().to_string(),
            ControllerJobState {
                job_type: JOB_TYPE.to_string(),
                version: 1,
                request: request.clone(),
                public_request: request,
                request_digest: Uuid::new_v4().to_string(),
                active_idempotency_key: None,
                linked_durable_run_id: None,
                checkpoint: Some(json!({ "phase": "prepared" })),
                cancellation_requested: false,
                cancellation_reason: None,
                execution_claim_id: None,
                recovery_attempted: false,
                parent_job_id: None,
                pending_admission_reason: None,
            },
        )
        .expect("admit fixture job");
    let ControllerJobSubmissionOutcome::Submitted(job_id) = outcome else {
        panic!("unique fixture job was replayed");
    };
    store
        .start_controller_execution(job_id)
        .expect("claim execution");
    job_id
}

/// A live, token-owned daemon whose build is older than this binary, with the
/// given running jobs; returns its `daemon status`.
fn stale_daemon_status(requests: &[serde_json::Value]) -> DaemonStatus {
    let path = state_path().expect("state path");
    let _lock = acquire_daemon_owner_lock_at(&path, Duration::ZERO).expect("serve lock");
    let mut lease = write_state("127.0.0.1:31017".parse().unwrap(), None).expect("lease");
    lease.startup_token = "restart-safety-token".to_string();
    lease.build_identity.version = "0.0.1".to_string();
    lease.build_identity.display = "homeboy 0.0.1".to_string();
    write_lease(&path, &lease).expect("stale lease");
    let store = JobStore::open_without_reconciliation(paths::observation_db().expect("jobs path"))
        .expect("durable store")
        .with_daemon_scope(daemon_scope_for_state_path(&path))
        .with_daemon_lease(lease.lease_id.clone());
    for request in requests {
        running_job(&store, request.clone());
    }
    read_status().expect("status")
}

#[test]
fn a_stale_daemon_with_only_driver_recovery_jobs_is_restartable() {
    with_isolated_home(|_| {
        let status = stale_daemon_status(&[json!({}), json!({})]);

        assert!(!status.fresh);
        assert!(status
            .active_job_recovery_evidence
            .iter()
            .all(|evidence| evidence.disposition
                == crate::api_jobs::DaemonActiveJobRecoveryDisposition::DriverRecovery));
        assert_eq!(status.active_job_recovery_evidence.len(), 2);
        assert_eq!(status.freshness.active_jobs, 0);
        assert!(status.freshness.restartable);
        assert_eq!(
            status
                .freshness
                .repair_plan
                .iter()
                .map(|step| step.code.as_str())
                .collect::<Vec<_>>(),
            vec![recovery_actions::DAEMON_ENSURE_RUNNING]
        );
        assert!(!status.replacement_blocked, "{status:?}");
        assert!(recovery_actions::authorizes_automatic_idle_restart(&status));
    });
}

#[test]
fn a_stale_daemon_with_a_protected_live_job_stays_blocked() {
    with_isolated_home(|_| {
        let status = stale_daemon_status(&[json!({}), json!({ "live_pid": std::process::id() })]);

        assert!(status.active_job_recovery_evidence.iter().any(|evidence| {
            evidence.disposition
                == crate::api_jobs::DaemonActiveJobRecoveryDisposition::ProtectedLive
        }));
        assert_eq!(status.freshness.active_jobs, 1);
        assert!(!status.freshness.restartable);
        assert!(status.freshness.repair_plan.is_empty());
        assert!(status.replacement_blocked);
        assert!(!recovery_actions::authorizes_automatic_idle_restart(
            &status
        ));
    });
}
