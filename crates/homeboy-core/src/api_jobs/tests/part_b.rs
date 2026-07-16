#![cfg(test)]

use std::collections::HashMap;
use std::fs;

use serde_json::json;

use super::persistence::recovered_terminal_from_result;
use super::store::{LinkedDurableRunResolution, RecoveredTerminalJob};
use super::*;
use crate::secret_env_plan::SecretEnvPlan;
use crate::source_snapshot::SourceSnapshot;
use uuid::Uuid;

#[test]
fn confirmed_recovery_fails_closed_for_unresolved_linked_durable_run() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let mut request = remote_runner_request("homeboy-lab", None);
    request.lifecycle = Some(RunnerJobLifecycleMetadata {
        source: Some("runner-daemon".to_string()),
        kind: Some("runner.exec".to_string()),
        durable_run_id: Some("missing-linked-run".to_string()),
        active_child_count: None,
        active_cell_count: None,
    });
    let job = store
        .submit_remote_runner_job(request)
        .expect("queue linked job");

    let error = store
        .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs("lease-dead", &[job.id])
        .expect_err("unresolved linked run blocks confirmation");

    assert!(error.message.contains("cannot be safely resolved"));
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Queued);
}

#[test]
fn terminal_linked_run_is_authoritative_for_confirmed_recovery() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path)
        .expect("store")
        .with_daemon_lease("lease-dead".to_string());
    let selected = store.create("runner.exec");
    store.start(selected.id).expect("start selected");
    let before = std::fs::read(&path).expect("store bytes");

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs_and_linked_resolver(
            "lease-dead",
            &[],
            |_| false,
            |_| {
                LinkedDurableRunResolution::Terminal(RecoveredTerminalJob::test_result(
                    JobStatus::Succeeded,
                    "run-terminal",
                    json!({ "status": "succeeded" }),
                    Vec::new(),
                ))
            },
        )
        .expect("linked terminal evidence is reconciled authoritatively");

    assert_eq!(diagnostics.terminalized_count(), 1);
    assert_eq!(
        store.get(selected.id).expect("job").status,
        JobStatus::Succeeded
    );
    assert_ne!(std::fs::read(&path).expect("store bytes"), before);
}

#[test]
fn reused_pid_with_a_different_start_identity_is_terminalized() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, std::process::id());

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |_| false)
        .expect("reconcile reused PID");

    assert!(diagnostics.protected_job_ids.is_empty());
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Failed);
}

#[test]
fn dead_child_recovers_linked_terminal_result_and_artifacts() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, 4242);
    store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
            "lease-dead",
            |_| false,
            |_| {
                Some(RecoveredTerminalJob::test_result(
                    JobStatus::Succeeded,
                    "run-success",
                    json!({ "aggregate": { "status": "succeeded" } }),
                    vec![JobArtifactMetadata {
                        id: "artifact-1".to_string(),
                        name: None,
                        path: None,
                        url: Some("artifact://1".to_string()),
                        mime: None,
                        size_bytes: None,
                        sha256: None,
                        content_base64: None,
                        metadata: None,
                    }],
                ))
            },
        )
        .expect("reconcile");
    let recovered = store.get(job.id).expect("job");
    assert_eq!(recovered.status, JobStatus::Succeeded);
    assert_eq!(recovered.artifacts[0].id, "artifact-1");
    assert!(store
        .events(job.id)
        .expect("events")
        .iter()
        .any(|event| event
            .data
            .as_ref()
            .is_some_and(
                |data| data["child_terminal_result"]["aggregate"]["status"] == "succeeded"
            )));
}

#[test]
fn dead_child_recovers_linked_terminal_failure_and_live_child_skips_resolver() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, 4242);
    store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
            "lease-dead",
            |_| true,
            |_| panic!("live child must win"),
        )
        .expect("live child deferred");
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
    store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
            "lease-dead",
            |_| false,
            |_| {
                Some(RecoveredTerminalJob::test_result(
                    JobStatus::Failed,
                    "run-failure",
                    json!({ "provider": "failed" }),
                    Vec::new(),
                ))
            },
        )
        .expect("reconcile failure");
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Failed);
    assert!(store.get(job.id).expect("job").stale_reason.is_none());
}

#[test]
fn dead_lease_reconciliation_refuses_ambiguous_child_state_without_mutation() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");
    let event_count = store.events(job.id).expect("events").len();

    let error = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |_| false)
        .expect_err("missing child evidence must fail closed");

    assert!(error.message.contains(&job.id.to_string()));
    assert!(error
        .message
        .contains("authoritative terminal result or recorded child PID"));
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
    assert_eq!(store.events(job.id).expect("events").len(), event_count);
}

#[test]
fn dead_lease_reconciliation_requires_exact_confirmation_for_untracked_children() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let confirmed = store.create("runner.exec");
    store.start(confirmed.id).expect("start confirmed job");
    let unconfirmed = store.create("runner.exec");
    store.start(unconfirmed.id).expect("start unconfirmed job");
    let unrelated = Uuid::new_v4();

    let error = store
        .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs("lease-dead", &[confirmed.id])
        .expect_err("every unresolved no-PID job needs exact confirmation");
    assert!(error.message.contains(&unconfirmed.id.to_string()));
    assert_eq!(
        store.get(confirmed.id).expect("confirmed job").status,
        JobStatus::Running
    );

    let error = store
        .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs(
            "lease-dead",
            &[confirmed.id, unrelated],
        )
        .expect_err("unknown confirmation must fail closed");
    assert!(error.message.contains(&unrelated.to_string()));
    assert_eq!(
        store.get(confirmed.id).expect("confirmed job").status,
        JobStatus::Running
    );

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs(
            "lease-dead",
            &[confirmed.id, unconfirmed.id],
        )
        .expect("exact confirmations terminalize only the confirmed no-PID jobs");
    assert_eq!(diagnostics.terminalized_count(), 2);
    for job_id in [confirmed.id, unconfirmed.id] {
        assert_eq!(store.get(job_id).expect("job").status, JobStatus::Failed);
        assert!(store.events(job_id).expect("events").iter().any(|event| {
            event.data.as_ref().is_some_and(|data| {
                data["reason"]
                    == json!("operator_confirmed_untracked_child_dead_after_dead_daemon_lease")
                    && data["operator_confirmation"] == json!(true)
            })
        }));
    }
}

#[test]
fn dead_lease_reconciliation_is_idempotent_after_terminalizing_a_dead_child() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, 4242);

    let first = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |_| false)
        .expect("first reconciliation");
    let event_count = store.events(job.id).expect("events").len();
    let second = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |_| false)
        .expect("repeated reconciliation");

    assert_eq!(first.terminalized_count(), 1);
    assert_eq!(second.matching_count(), 0);
    assert_eq!(second.terminalized_count(), 0);
    assert_eq!(store.events(job.id).expect("events").len(), event_count);
}

#[test]
fn leaseless_recovery_terminalizes_one_historical_lease_and_retains_evidence() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let leased = JobStore::open_without_reconciliation(&path)
        .expect("open leased store")
        .with_daemon_lease("lease-owned".to_string());
    let succeeded = leased.create("runner.exec");
    leased.start(succeeded.id).expect("start succeeded job");
    leased
        .append_event(
            succeeded.id,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 0 })),
        )
        .expect("record result");
    let unfinished = leased.create("runner.exec");
    record_test_local_child(&leased, unfinished.id, u32::MAX);

    let recovery = JobStore::open_without_reconciliation(&path).expect("open recovery");
    let diagnostics = recovery
        .reconcile_leaseless_orphan_jobs()
        .expect("reconcile legacy jobs");
    assert_eq!(diagnostics.reconciled_count(), 2);
    assert!(diagnostics.reconciled_job_ids.contains(&succeeded.id));
    assert!(diagnostics.reconciled_job_ids.contains(&unfinished.id));
    assert_eq!(
        recovery.get(succeeded.id).expect("succeeded").status,
        JobStatus::Succeeded
    );
    assert_eq!(
        recovery.get(unfinished.id).expect("unfinished").status,
        JobStatus::Failed
    );
    assert!(recovery
        .events(unfinished.id)
        .expect("events")
        .iter()
        .any(|event| event
            .data
            .as_ref()
            .is_some_and(|data| data["reason"] == json!("dead_daemon_lease"))));
}

#[test]
fn leaseless_recovery_enumerates_and_terminalizes_multiple_historical_leases() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let first = JobStore::open_without_reconciliation(&path)
        .expect("open first")
        .with_daemon_lease("lease-first".to_string());
    let first_job = first.create("runner.exec");
    record_test_local_child(&first, first_job.id, u32::MAX);
    first
        .append_event(
            first_job.id,
            JobEventKind::Stdout,
            Some("first output".to_string()),
            None,
        )
        .expect("first output");
    let second = JobStore::open_without_reconciliation(&path)
        .expect("open second")
        .with_daemon_lease("lease-second".to_string());
    let second_job = second.create("runner.exec");
    record_test_local_child(&second, second_job.id, u32::MAX);

    let recovery = JobStore::open_without_reconciliation(&path).expect("open recovery");
    let diagnostics = recovery
        .reconcile_leaseless_orphan_jobs()
        .expect("reconcile");

    assert_eq!(
        diagnostics.historical_lease_ids,
        vec!["lease-first", "lease-second"]
    );
    assert_eq!(diagnostics.affected_jobs.len(), 2);
    assert_eq!(
        recovery.get(first_job.id).expect("first job").status,
        JobStatus::Failed
    );
    assert_eq!(
        recovery.get(second_job.id).expect("second job").status,
        JobStatus::Failed
    );
    assert!(recovery
        .events(first_job.id)
        .expect("first events")
        .iter()
        .any(|event| event.message.as_deref() == Some("first output")));
}

#[test]
fn leaseless_recovery_reconciles_proven_dead_unowned_before_historical_lease_once() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let unowned = JobStore::open_without_reconciliation(&path).expect("open unowned store");
    let unowned_job = unowned.create("runner.exec");
    record_test_local_child(&unowned, unowned_job.id, u32::MAX);
    let leased = JobStore::open_without_reconciliation(&path)
        .expect("open leased store")
        .with_daemon_lease("lease-historical".to_string());
    let leased_job = leased.create("runner.exec");
    record_test_local_child(&leased, leased_job.id, u32::MAX);

    let recovery = JobStore::open_without_reconciliation(&path).expect("open recovery");
    let first = recovery
        .reconcile_leaseless_orphan_jobs()
        .expect("proven-dead mixed store reconciles");
    let unowned_events = recovery
        .events(unowned_job.id)
        .expect("unowned events")
        .len();
    let leased_events = recovery.events(leased_job.id).expect("leased events").len();
    let replay = recovery
        .reconcile_leaseless_orphan_jobs()
        .expect("replay is idempotent");

    assert_eq!(first.reconciled_count(), 2);
    assert_eq!(replay.reconciled_count(), 0);
    assert_eq!(
        recovery.get(unowned_job.id).expect("unowned").status,
        JobStatus::Failed
    );
    assert_eq!(
        recovery.get(leased_job.id).expect("leased").status,
        JobStatus::Failed
    );
    assert_eq!(
        recovery
            .events(unowned_job.id)
            .expect("unowned events")
            .len(),
        unowned_events
    );
    assert_eq!(
        recovery.events(leased_job.id).expect("leased events").len(),
        leased_events
    );
}

#[test]
fn leaseless_recovery_fails_before_historical_mutation_when_unowned_work_is_ambiguous() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let unowned = JobStore::open_without_reconciliation(&path).expect("open unowned store");
    let unowned_job = unowned.create("runner.exec");
    unowned.start(unowned_job.id).expect("start unowned job");
    let leased = JobStore::open_without_reconciliation(&path)
        .expect("open leased store")
        .with_daemon_lease("lease-historical".to_string());
    let leased_job = leased.create("runner.exec");
    record_test_local_child(&leased, leased_job.id, u32::MAX);
    let leased_events = leased.events(leased_job.id).expect("leased events").len();

    let recovery = JobStore::open_without_reconciliation(&path).expect("open recovery");
    let error = recovery
        .reconcile_leaseless_orphan_jobs()
        .expect_err("ambiguous unowned work fails closed");

    assert!(error.message.contains("no authoritative terminal result"));
    assert_eq!(
        recovery.get(unowned_job.id).expect("unowned").status,
        JobStatus::Running
    );
    assert_eq!(
        recovery.get(leased_job.id).expect("leased").status,
        JobStatus::Running
    );
    assert_eq!(
        recovery.events(leased_job.id).expect("leased events").len(),
        leased_events
    );
}

#[test]
fn leaseless_recovery_preserves_queued_remote_jobs_and_expires_claims_through_broker() {
    let queued = JobStore::default().with_daemon_lease("lease-remote".to_string());
    let queued_job = queued
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
        .expect("queue remote job");

    let preserved = queued
        .reconcile_leaseless_orphan_jobs()
        .expect("queued remote job is broker-owned");
    assert_eq!(preserved.preserved_remote_job_ids, vec![queued_job.id]);
    assert!(preserved.protected_job_ids.is_empty());
    assert_eq!(
        queued.get(queued_job.id).expect("job").status,
        JobStatus::Queued
    );

    let expired = JobStore::default().with_daemon_lease("lease-expired".to_string());
    let expired_job = expired
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
        .expect("queue remote job");
    expired
        .claim_remote_runner_job("homeboy-lab", None, 1, None)
        .expect("claim remote job")
        .expect("claim exists");
    std::thread::sleep(std::time::Duration::from_millis(5));

    let diagnostics = expired
        .reconcile_leaseless_orphan_jobs()
        .expect("expired claim reconciles through broker lifecycle");
    assert_eq!(diagnostics.reconciled_count(), 0);
    assert_eq!(
        expired.get(expired_job.id).expect("job").status,
        JobStatus::Failed
    );
    assert!(expired
        .events(expired_job.id)
        .expect("events")
        .iter()
        .any(|event| event.message.as_deref() == Some("remote runner claim expired")));
}

#[test]
fn daemon_job_acceptance_stamps_current_lease_and_old_job_json_remains_readable() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path)
        .expect("open daemon store")
        .with_daemon_lease("lease-current".to_string());
    let job = store.create("runner.exec");

    assert_eq!(job.daemon_lease_id.as_deref(), Some("lease-current"));
    let mut legacy = serde_json::to_value(&job).expect("serialize job");
    legacy
        .as_object_mut()
        .expect("job object")
        .remove("daemon_lease_id");
    let decoded: Job = serde_json::from_value(legacy).expect("read old public job schema");
    assert!(decoded.daemon_lease_id.is_none());
}

#[test]
fn durable_store_stale_restart_classification_preserves_last_child_evidence() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
        .expect("remote runner job queues");
    let claim = store
        .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
        .expect("claim succeeds")
        .expect("job claimed");
    let claim_id = claim.job.claim_id.expect("claim id");
    store
        .append_remote_runner_event(
            job.id,
            "homeboy-lab",
            &claim_id,
            JobEventKind::Stdout,
            Some("running wp test".to_string()),
            Some(json!({ "tail": "last child output" })),
        )
        .expect("runner appends child output");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let stale = reopened.get(job.id).expect("job persists");
    assert_eq!(stale.status, JobStatus::Failed);

    let events = reopened.events(job.id).expect("events persist");
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Stdout && event.message.as_deref() == Some("running wp test")
    }));
    let classification = events
        .iter()
        .find_map(|event| {
            (event.kind == JobEventKind::Error)
                .then(|| event.data.as_ref()?.get("classification"))?
        })
        .expect("stale classification");

    assert_eq!(
        classification["kind"],
        json!("orphaned_after_control_plane_loss")
    );
    assert_eq!(classification["recoverable"], json!(false));
    assert_eq!(
        classification["child"]["terminal_result_recorded"],
        json!(false)
    );
    assert_eq!(classification["child"]["output_observed"], json!(true));
    assert_eq!(
        classification["child"]["last_known_event"]["kind"],
        json!("stdout")
    );
    assert_eq!(
        classification["child"]["last_known_event"]["message"],
        json!("running wp test")
    );
    assert_eq!(
        classification["remote_runner"]["runner_id"],
        json!("homeboy-lab")
    );
    assert_eq!(
        classification["remote_runner"]["claimed_by_runner_id"],
        json!("homeboy-lab")
    );
}

#[test]
fn durable_store_recovers_succeeded_job_from_result_event_after_restart() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store.create("runner.exec");
    store.start(job.id).expect("job starts");
    // Simulate the daemon worker recording its terminal result *before* the
    // status transition, then the daemon process dying in that window.
    store
        .append_event(
            job.id,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 0, "stdout": "ok" })),
        )
        .expect("result event records");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let recovered = reopened.get(job.id).expect("job persists");
    assert_eq!(
        recovered.status,
        JobStatus::Succeeded,
        "a job with a successful recorded result must not be reported as a daemon-restart failure"
    );
    assert!(recovered.stale_reason.is_none());
    assert!(recovered.finished_at_ms.is_some());

    let events = reopened.events(job.id).expect("events persist");
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Status
            && event
                .data
                .as_ref()
                .is_some_and(|data| data["reason"] == json!("recovered_after_daemon_restart"))
    }));
}

#[test]
fn durable_store_recovers_failed_job_from_nonzero_result_after_restart() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store.create("runner.exec");
    store.start(job.id).expect("job starts");
    store
        .append_event(
            job.id,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 2, "stderr": "boom" })),
        )
        .expect("result event records");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let recovered = reopened.get(job.id).expect("job persists");
    assert_eq!(recovered.status, JobStatus::Failed);
    // Recovered (real) failure must not be mislabeled as a daemon-restart stale failure.
    assert!(recovered.stale_reason.is_none());

    let events = reopened.events(job.id).expect("events persist");
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Status
            && event.data.as_ref().is_some_and(|data| {
                data["reason"] == json!("recovered_after_daemon_restart")
                    && data["exit_code"] == json!(2)
            })
    }));
    // It must NOT carry the synthetic stale-after-restart error event.
    assert!(!events.iter().any(|event| {
        event
            .data
            .as_ref()
            .is_some_and(|data| data["reason"] == json!("orphaned_after_control_plane_loss"))
    }));
}

#[test]
fn durable_store_recovers_cancelled_job_from_result_after_restart() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store.create("runner.exec");
    store.start(job.id).expect("job starts");
    store
        .append_event(
            job.id,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 130, "status": "cancelled" })),
        )
        .expect("result event records");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let recovered = reopened.get(job.id).expect("job persists");
    assert_eq!(recovered.status, JobStatus::Cancelled);
    assert!(recovered.stale_reason.is_none());
}

#[test]
fn recovered_terminal_from_result_handles_missing_and_present_results() {
    // No result event -> None (falls through to stale failure).
    assert!(recovered_terminal_from_result(&[]).is_none());

    let zero = vec![JobEvent {
        sequence: 1,
        job_id: Uuid::new_v4(),
        kind: JobEventKind::Result,
        timestamp_ms: 0,
        message: None,
        data: Some(json!({ "exit_code": 0 })),
    }];
    assert_eq!(
        recovered_terminal_from_result(&zero),
        Some((JobStatus::Succeeded, 0))
    );

    let nonzero = vec![JobEvent {
        sequence: 1,
        job_id: Uuid::new_v4(),
        kind: JobEventKind::Result,
        timestamp_ms: 0,
        message: None,
        data: Some(json!({ "exit_code": 7 })),
    }];
    assert_eq!(
        recovered_terminal_from_result(&nonzero),
        Some((JobStatus::Failed, 7))
    );

    // Result event without an exit_code is not authoritative -> None.
    let no_code = vec![JobEvent {
        sequence: 1,
        job_id: Uuid::new_v4(),
        kind: JobEventKind::Result,
        timestamp_ms: 0,
        message: None,
        data: Some(json!({ "stdout": "partial" })),
    }];
    assert!(recovered_terminal_from_result(&no_code).is_none());
}

#[test]
fn test_open_with_event_retention() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_with_event_retention(&path, 3).expect("durable store opens");
    let job = store.create("test");
    store.start(job.id).expect("job starts");

    for index in 0..5 {
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "index": index })),
            )
            .expect("progress event appends");
    }

    let events = store.events(job.id).expect("events are readable");
    assert_eq!(events.len(), 3);
    assert_eq!(store.get(job.id).expect("job persists").event_count, 3);

    let reopened = JobStore::open_with_event_retention(&path, 3).expect("durable store reopens");
    let reopened_events = reopened.events(job.id).expect("events persist");
    assert_eq!(reopened_events.len(), 3);
    assert!(reopened_events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
}

#[test]
fn remote_runner_job_submit_targets_runner_and_project() {
    let store = JobStore::default();
    let job = store
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
        .expect("remote runner job queues");

    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.operation, "runner.exec");
    assert_eq!(job.target_runner_id.as_deref(), Some("homeboy-lab"));
    assert_eq!(job.target_project_id.as_deref(), Some("extrachill"));
    assert_eq!(job.event_count, 1);

    let events = store.events(job.id).expect("events are readable");
    assert_eq!(events[0].kind, JobEventKind::Status);
    assert_eq!(events[0].data, Some(json!({ "status": "queued" })));
}

#[test]
fn remote_runner_job_secret_env_is_execution_only_not_public_or_persisted() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let sentinel = "homeboy-secret-sentinel-do-not-persist";
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request
        .env
        .insert("RUNNER_SECRET_TOKEN".to_string(), sentinel.to_string());
    request
        .env
        .insert("PUBLIC_FLAG".to_string(), "1".to_string());
    request.secret_env_names = vec!["RUNNER_SECRET_TOKEN".to_string()];

    let job = store
        .submit_remote_runner_job(request)
        .expect("remote runner job queues");

    {
        let inner = store.inner.lock().expect("job store mutex poisoned");
        let stored = inner.jobs.get(&job.id).expect("job stored");
        let remote_runner = stored.remote_runner.as_ref().expect("remote runner job");
        assert_eq!(
            remote_runner.request.env.get("RUNNER_SECRET_TOKEN"),
            Some(&"<redacted>".to_string())
        );
        assert_eq!(
            remote_runner
                .execution_request
                .as_ref()
                .expect("execution request")
                .env
                .get("RUNNER_SECRET_TOKEN"),
            Some(&sentinel.to_string())
        );
    }

    let persisted = fs::read_to_string(&path).expect("read durable store");
    assert!(
        !persisted.contains(sentinel),
        "persisted store leaked secret"
    );
    assert!(persisted.contains("<redacted>"));

    let reopened = JobStore::open(&path).expect("durable store reopens");
    assert_eq!(
        reopened.get(job.id).expect("reopened job").status,
        JobStatus::Failed
    );
    assert!(reopened
        .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
        .expect("claim request succeeds")
        .is_none());

    let claim = store
        .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
        .expect("claim succeeds")
        .expect("job is claimed");
    assert_eq!(
        claim.request.env.get("RUNNER_SECRET_TOKEN"),
        Some(&sentinel.to_string())
    );
}

#[test]
fn remote_runner_job_env_is_scoped_to_submitted_job() {
    let store = JobStore::default();
    let mut first = remote_runner_request("homeboy-lab", Some("extrachill"));
    first.env.insert(
        "STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH".to_string(),
        "/tmp/sample-runtime".to_string(),
    );
    let second = remote_runner_request("homeboy-lab", Some("extrachill"));

    store
        .submit_remote_runner_job(first)
        .expect("first job queues");
    store
        .submit_remote_runner_job(second)
        .expect("second job queues");

    let first_claim = store
        .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
        .expect("first claim succeeds")
        .expect("first claim");
    let second_claim = store
        .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
        .expect("second claim succeeds")
        .expect("second claim");

    assert_eq!(
        first_claim
            .request
            .env
            .get("STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH"),
        Some(&"/tmp/sample-runtime".to_string())
    );
    assert!(!second_claim
        .request
        .env
        .contains_key("STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH"));
}
