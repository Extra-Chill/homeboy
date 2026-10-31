//! Several controllers' daemons share one HOME-wide ledger. Each daemon's
//! startup sweeps, recovery, deliveries, and stop/idle counts act only on its
//! own daemon scope; scope-less (pre-scope) records belong to none (#16104).
use super::*;
use crate::test_support::with_isolated_home;

fn scope(name: &str) -> DaemonScope {
    DaemonScope::from_state_dir(Path::new(&format!("/state/runner-daemons/runner/{name}")))
}

fn daemon(path: &Path, name: &str) -> JobStore {
    JobStore::open_without_reconciliation(path)
        .expect("open ledger")
        .with_daemon_lease(format!("lease-{name}-{}", Uuid::new_v4()))
        .with_daemon_scope(scope(name))
}

/// A record written before daemon scopes existed.
fn legacy(path: &Path) -> JobStore {
    JobStore::open_without_reconciliation(path).expect("open ledger")
}

fn ledger(home: &tempfile::TempDir) -> PathBuf {
    home.path().join(crate::paths::OBSERVATION_DB_FILENAME)
}

#[test]
fn startup_admission_expiry_fails_only_its_own_scope() {
    with_isolated_home(|home| {
        let path = ledger(home);
        let now = timestamp_ms();
        let admit = |store: JobStore| {
            store
                .create_admission_at(serde_json::json!({}), now, None)
                .expect("reserve admission")
                .job
                .id
        };
        let own = admit(daemon(&path, "a"));
        let other = admit(daemon(&path, "b"));
        let scopeless = admit(legacy(&path));

        let restarted = daemon(&path, "a");
        let expired = restarted
            .reconcile_expired_admissions_at(now + ADMISSION_RESERVATION_LEASE_MS + 1)
            .expect("reconcile admissions");

        assert_eq!(expired, vec![own]);
        assert_eq!(restarted.get(own).unwrap().status, JobStatus::Failed);
        for untouched in [other, scopeless] {
            assert_eq!(restarted.get(untouched).unwrap().status, JobStatus::Queued);
        }
    });
}

fn controller_state() -> ControllerJobState {
    ControllerJobState {
        job_type: "test.daemon-scope".to_string(),
        version: 1,
        request: serde_json::json!({}),
        public_request: serde_json::json!({}),
        request_digest: format!("digest-{}", Uuid::new_v4()),
        active_idempotency_key: None,
        linked_durable_run_id: None,
        checkpoint: None,
        cancellation_requested: false,
        cancellation_reason: None,
        execution_claim_id: None,
        recovery_attempted: false,
        parent_job_id: None,
    }
}

fn admit_controller(store: &JobStore) -> Uuid {
    match store
        .admit_controller_job(
            "controller.test.daemon-scope".to_string(),
            Uuid::new_v4().to_string(),
            controller_state(),
        )
        .expect("admit controller job")
    {
        ControllerJobSubmissionOutcome::Submitted(job_id) => job_id,
        ControllerJobSubmissionOutcome::Existing(_) => panic!("unique submission replayed"),
    }
}

#[test]
fn controller_recovery_never_resumes_another_scopes_job() {
    with_isolated_home(|home| {
        let path = ledger(home);
        let own = admit_controller(&daemon(&path, "a"));
        let other = admit_controller(&daemon(&path, "b"));
        let scopeless = admit_controller(&legacy(&path));

        let restarted = daemon(&path, "a");
        let recoverable = restarted
            .active_controller_jobs()
            .into_iter()
            .map(|(job_id, _)| job_id)
            .collect::<Vec<_>>();
        assert_eq!(recoverable, vec![own]);

        crate::daemon::recover_controller_jobs(&restarted);
        for untouched in [other, scopeless] {
            assert_eq!(restarted.get(untouched).unwrap().status, JobStatus::Queued);
            assert!(restarted
                .controller_job_state(untouched)
                .unwrap()
                .execution_claim_id
                .is_none());
        }
    });
}

#[test]
fn stop_and_idle_counts_and_deliveries_see_only_their_own_scope() {
    with_isolated_home(|home| {
        let path = ledger(home);
        let own = daemon(&path, "a");
        let queued = own.create("runner.exec").id;
        let running = own.create("runner.exec").id;
        own.start(running).expect("start own job");
        daemon(&path, "b").create("runner.exec");
        legacy(&path).create("runner.exec");
        // Payloads another scope's daemon would deliver: deserializing them
        // here would fail, so seeing them at all is a scope violation.
        let connection = crate::api_jobs::persistence::open_ledger(&path).expect("open ledger");
        for payload in [
            serde_json::json!({ "daemon_scope": scope("b").as_str() }).to_string(),
            serde_json::json!({ "daemon_scope": scope("b").as_str() }).to_string(),
            "{}".to_string(),
        ] {
            connection
                .execute(
                    "INSERT INTO step_terminal_deliveries(job_id, payload) VALUES (?1, ?2)",
                    rusqlite::params![Uuid::new_v4().to_string(), payload],
                )
                .expect("seed delivery");
        }

        let restarted = daemon(&path, "a");
        let mut expected = vec![queued, running];
        expected.sort();
        assert_eq!(restarted.active_job_ids(), expected);
        assert!(restarted
            .pending_step_terminal_deliveries()
            .expect("own deliveries")
            .is_empty());
        restarted
            .deliver_pending_step_terminals()
            .expect("no foreign delivery attempted");
        assert_eq!(
            JobStore::active_count_at_path(&path, Some(&scope("a"))).unwrap(),
            2
        );
        assert_eq!(
            JobStore::active_count_at_path(&path, Some(&scope("b"))).unwrap(),
            3
        );
        assert_eq!(JobStore::active_count_at_path(&path, None).unwrap(), 2);
    });
}
