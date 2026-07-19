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
fn remote_runner_submission_lookup_is_non_mutating() {
    let store = JobStore::default();
    let missing = store.lookup_remote_runner_submission("missing-key");
    assert!(matches!(missing, RemoteRunnerSubmissionLookup::Absent));

    let mut request = remote_runner_request("homeboy-lab", None);
    request.metadata = Some(json!({ "submission_key": "lookup-key" }));
    let accepted = store
        .submit_remote_runner_job(request)
        .expect("accept runner submission");

    let lookup = store.lookup_remote_runner_submission("lookup-key");
    assert!(matches!(
        lookup,
        RemoteRunnerSubmissionLookup::Accepted { job } if job.id == accepted.id
    ));
    assert_eq!(store.events(accepted.id).expect("events").len(), 1);
}

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
fn terminal_linked_reconciliation_preserves_live_jobs_and_is_idempotent() {
    let store = JobStore::default().with_daemon_lease("lease-live".to_string());
    let first = store.create("runner.exec");
    store.start(first.id).expect("start first terminal handoff");
    let second = store.create("runner.exec");
    store
        .start(second.id)
        .expect("start second terminal handoff");
    let live = store.create("runner.exec");
    store.start(live.id).expect("start live job");

    let mut first_pass = store
        .reconcile_terminal_linked_daemon_jobs_with_resolver(|stored| match stored.job.id {
            id if id == first.id => Some(RecoveredTerminalJob::test_result(
                JobStatus::Cancelled,
                "run-cancelled",
                json!({ "status": "cancelled" }),
                Vec::new(),
            )),
            id if id == second.id => Some(RecoveredTerminalJob::test_result(
                JobStatus::Failed,
                "run-failed",
                json!({ "status": "failed" }),
                Vec::new(),
            )),
            _ => None,
        })
        .expect("reconcile terminal handoffs");
    first_pass.sort();
    let mut expected = vec![first.id, second.id];
    expected.sort();
    assert_eq!(first_pass, expected);
    assert_eq!(
        store.get(first.id).expect("first").status,
        JobStatus::Cancelled
    );
    assert_eq!(
        store.get(second.id).expect("second").status,
        JobStatus::Failed
    );
    assert_eq!(store.get(live.id).expect("live").status, JobStatus::Running);

    let repeated = store
        .reconcile_terminal_linked_daemon_jobs_with_resolver(|_| None)
        .expect("repeat reconciliation");
    assert!(repeated.is_empty());
    assert_eq!(
        store.get(live.id).expect("live remains protected").status,
        JobStatus::Running
    );
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
fn exact_daemon_loss_recovery_requires_the_complete_pidless_active_set_and_persists_evidence() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let first = store.create("runner.exec");
    store.start(first.id).expect("first starts");
    let second = store.create("runner.exec");
    store.start(second.id).expect("second starts");

    for ids in [&[first.id][..], &[first.id, Uuid::new_v4()][..]] {
        let error = store
            .reconcile_exact_daemon_loss_jobs("lease-dead", ids, 4242)
            .expect_err("omitted or unknown active jobs are refused");
        assert!(error.message.contains("exact active durable-job set"));
    }
    assert_eq!(
        store.get(first.id).expect("first").status,
        JobStatus::Running
    );

    let diagnostics = store
        .reconcile_exact_daemon_loss_jobs("lease-dead", &[first.id, second.id], 4242)
        .expect("the exact complete set reconciles");
    assert_eq!(
        diagnostics.matching_job_ids,
        [first.id, second.id]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    for id in [first.id, second.id] {
        assert_eq!(
            store.get(id).expect("terminal job").status,
            JobStatus::Failed
        );
        assert!(store
            .events(id)
            .expect("durable events")
            .iter()
            .any(|event| {
                event.data.as_ref().is_some_and(|data| {
                    data["reason"] == "operator_confirmed_daemon_loss_after_unexpected_termination"
                        && data["daemon_lease_id"] == "lease-dead"
                        && data["daemon_pid"] == 4242
                        && data["operator_confirmed_workload_processes_absent"] == true
                })
            }));
    }
    assert!(store
        .reconcile_exact_daemon_loss_jobs("lease-dead", &[first.id, second.id], 4242)
        .expect_err("replay refuses because the named jobs are no longer active")
        .message
        .contains("exact active durable-job set"));
}

#[test]
fn exact_daemon_loss_recovery_refuses_persisted_child_process_evidence() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, u32::MAX);

    let error = store
        .reconcile_exact_daemon_loss_jobs("lease-dead", &[job.id], 4242)
        .expect_err("live-process evidence contradicts no-PID recovery");

    assert!(error.message.contains("child-process evidence"));
    assert_eq!(
        store.get(job.id).expect("protected job").status,
        JobStatus::Running
    );
}

#[test]
fn exact_daemon_loss_recovery_accepts_a_reservation_without_process_identity() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    store
        .reserve_local_child_at(job.id, 100)
        .expect("pre-spawn reservation persists");

    store
        .reconcile_exact_daemon_loss_jobs("lease-dead", &[job.id], 4242)
        .expect("a reservation without process identity remains a PID-less job");

    assert_eq!(
        store.get(job.id).expect("terminal job").status,
        JobStatus::Failed
    );
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
fn terminal_job_retention_compacts_existing_store_without_losing_active_recovery_evidence() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("durable store opens");
    let queued = store.create("queued");
    let running = store.create("running");
    store.start(running.id).expect("running job starts");
    let terminal = (0..4)
        .map(|_| {
            let job = store.create("terminal");
            store.start(job.id).expect("terminal job starts");
            store
                .complete(job.id, None)
                .expect("terminal job completes");
            job.id
        })
        .collect::<Vec<_>>();
    {
        let mut inner = store.inner.lock().expect("job store mutex poisoned");
        for (index, job_id) in terminal.iter().enumerate() {
            let stored = inner.jobs.get_mut(job_id).expect("terminal job exists");
            let timestamp = (index as u64 + 1) * 100;
            stored.job.created_at_ms = timestamp;
            stored.job.updated_at_ms = timestamp;
            stored.job.finished_at_ms = Some(timestamp);
        }
    }
    store.persist().expect("seed store persists");

    let compacted = JobStore::open_without_reconciliation_with_retention(&path, 3, 2)
        .expect("existing store compacts on open");
    assert!(compacted.get(queued.id).is_ok());
    assert_eq!(
        compacted.get(running.id).expect("running job").status,
        JobStatus::Running
    );
    assert!(compacted.get(terminal[0]).is_err());
    assert!(compacted.get(terminal[1]).is_err());
    assert!(compacted.get(terminal[2]).is_ok());
    assert!(compacted.get(terminal[3]).is_ok());

    let persisted = fs::read_to_string(&path).expect("compacted store persists");
    assert_eq!(persisted.matches("\"operation\": \"terminal\"").count(), 2);
    assert!(persisted.contains("\"removed_terminal_jobs\": 2"));
    assert!(persisted.contains("\"active_jobs\": 2"));

    let restarted = JobStore::open_without_reconciliation_with_retention(&path, 3, 2)
        .expect("compacted store restarts");
    assert_eq!(restarted.list().len(), 4);
    for _ in 0..5 {
        let job = restarted.create("terminal");
        restarted.start(job.id).expect("new terminal job starts");
        restarted
            .complete(job.id, None)
            .expect("new terminal job completes");
    }
    assert_eq!(restarted.list().len(), 4, "terminal history stays bounded");
    assert!(restarted.get(queued.id).is_ok());
    assert_eq!(
        restarted.get(running.id).expect("running job").status,
        JobStatus::Running
    );
}

#[test]
fn legacy_compaction_evidence_without_retained_bytes_reopens() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("durable store opens");
    let job = store.create("terminal");
    store.start(job.id).expect("terminal job starts");
    store
        .complete(job.id, None)
        .expect("terminal job completes");

    let mut durable: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("durable store is readable"))
            .expect("durable store is valid JSON");
    durable["compaction"] = json!({
        "timestamp_ms": 100,
        "removed_terminal_jobs": 1,
        "retained_terminal_jobs": 1,
        "active_jobs": 0
    });
    fs::write(
        &path,
        serde_json::to_string_pretty(&durable).expect("legacy store serializes"),
    )
    .expect("legacy store persists");

    let reopened = JobStore::open_without_reconciliation(&path)
        .expect("legacy compaction evidence remains readable");
    assert_eq!(
        reopened.get(job.id).expect("job survives").status,
        JobStatus::Succeeded
    );
    assert_eq!(
        reopened
            .inner
            .lock()
            .expect("job store mutex")
            .compaction
            .as_ref()
            .expect("compaction evidence survives")
            .retained_terminal_bytes,
        0
    );
}

#[test]
fn terminal_payload_budget_bounds_high_event_history_before_child_reservation_persists() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("durable store opens");
    let queued = store.create("queued");
    let running = store.create("running");
    record_test_local_child(&store, running.id, std::process::id());

    let terminal = (0..8)
        .map(|index| {
            let job = store.create("terminal");
            store.start(job.id).expect("terminal job starts");
            store
                .complete(
                    job.id,
                    Some(json!({ "index": index, "output": "x".repeat(20_000) })),
                )
                .expect("terminal job completes");
            job.id
        })
        .collect::<Vec<_>>();

    let compacted = JobStore::open_without_reconciliation_with_retention_and_terminal_byte_limit(
        &path, 10, 8, 70_000,
    )
    .expect("high-payload history compacts on open");
    assert!(
        compacted.get(queued.id).is_ok(),
        "queued work survives compaction"
    );
    assert_eq!(
        compacted
            .get(running.id)
            .expect("running job survives")
            .status,
        JobStatus::Running
    );
    assert!(compacted
        .events(running.id)
        .expect("recovery events")
        .iter()
        .any(|event| event
            .data
            .as_ref()
            .is_some_and(|data| data["phase"] == "spawned")));
    assert!(
        compacted.get(terminal[0]).is_err(),
        "oldest terminal evicts first"
    );
    assert!(compacted
        .get(*terminal.last().expect("terminal jobs"))
        .is_ok());
    assert!(
        fs::metadata(&path).expect("compacted store metadata").len() < 80_000,
        "the persisted store remains bounded despite high-payload terminal events"
    );

    let reserved = compacted.create("new-child");
    record_test_local_child(&compacted, reserved.id, std::process::id());
    assert!(compacted
        .events(reserved.id)
        .expect("spawn events")
        .iter()
        .any(|event| event
            .data
            .as_ref()
            .is_some_and(|data| data["phase"] == "spawned")));
    assert!(
        fs::metadata(&path)
            .expect("reserved child store metadata")
            .len()
            < 80_000,
        "reservation and PID persistence do not rewrite historical payloads"
    );
}

#[test]
fn oversized_terminal_result_remains_observable_when_it_exceeds_the_byte_budget() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let terminal_job_retention_bytes = 64;
    let store = JobStore::open_without_reconciliation_with_retention_and_terminal_byte_limit(
        &path,
        10,
        10,
        terminal_job_retention_bytes,
    )
    .expect("durable store opens");
    let job = store.create("oversized-result");
    store.start(job.id).expect("job starts");

    let completed = store
        .complete(job.id, Some(json!({ "output": "x".repeat(10_000) })))
        .expect("terminal transition remains observable");

    assert_eq!(completed.status, JobStatus::Succeeded);
    assert_eq!(
        store
            .get(job.id)
            .expect("terminal job remains readable")
            .status,
        JobStatus::Succeeded
    );
    let events = store
        .events(job.id)
        .expect("terminal events remain readable");
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Result));
    assert!(events.iter().any(|event| event.kind == JobEventKind::Status
        && event
            .data
            .as_ref()
            .is_some_and(|data| data["status"] == "succeeded")));

    let evidence = store
        .inner
        .lock()
        .expect("job store mutex")
        .compaction
        .clone()
        .expect("compaction evidence records the oversized terminal job");
    assert_eq!(evidence.removed_terminal_jobs, 0);
    assert_eq!(evidence.retained_terminal_jobs, 1);
    assert!(
        evidence.retained_terminal_bytes > terminal_job_retention_bytes,
        "the single retained terminal record may exceed the byte budget"
    );
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
fn remote_runner_job_rejects_inline_secret_env_before_durable_persistence() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request.env.insert(
        "RUNNER_SECRET_TOKEN".to_string(),
        "inline-secret".to_string(),
    );
    request
        .env
        .insert("PUBLIC_FLAG".to_string(), "1".to_string());
    request.secret_env_names = vec!["RUNNER_SECRET_TOKEN".to_string()];

    let error = store
        .submit_remote_runner_job(request)
        .expect_err("inline secret must be rejected before durable persistence");

    assert_eq!(error.code, crate::ErrorCode::ValidationInvalidArgument);
    assert_eq!(
        error.details["id"],
        serde_json::json!("durable_reverse_runner_inline_secret_env")
    );
    assert!(error
        .message
        .contains("cannot accept inline secret env values"));
    assert!(error.details["tried"]
        .as_array()
        .expect("actionable alternatives")
        .iter()
        .any(|value| value
            .as_str()
            .is_some_and(|value| value.contains("runner secret_env"))));
    assert!(
        store
            .inner
            .lock()
            .expect("job store mutex poisoned")
            .jobs
            .is_empty(),
        "rejected inline secret must not create a durable job"
    );
}

#[test]
fn remote_runner_submission_key_replays_one_redacted_durable_job() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store");
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request.secret_env_names = vec!["RUNNER_SECRET_TOKEN".to_string()];
    request.metadata = Some(json!({ "submission_key": "agent-task:crash-window" }));

    // Models POST-before-ack-persist: the recovery POST carries the same key.
    let first = store
        .submit_remote_runner_job(request.clone())
        .expect("first submit");
    let replay = store
        .submit_remote_runner_job(request)
        .expect("replayed submit");
    assert_eq!(first.id, replay.id);
    assert_eq!(
        store
            .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, Some(1))
            .expect("claim")
            .expect("one claim")
            .job
            .id,
        first.id
    );
    assert!(store
        .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, Some(1))
        .expect("duplicate wake")
        .is_none());

    let persisted = fs::read_to_string(path).expect("read durable store");
    assert!(!persisted.contains("never-persist"));
    assert!(persisted.contains("RUNNER_SECRET_TOKEN"));
}

#[test]
fn remote_runner_submission_key_survives_restart_and_rejects_conflicts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store");
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request.metadata = Some(json!({
        "submission_key": "agent-task:v1:restart",
        "transport": "initial-controller-post",
        "evidence": { "attempt": 1 },
    }));
    let accepted = store
        .submit_remote_runner_job(request.clone())
        .expect("first submit");
    drop(store);

    let restarted = JobStore::open_without_reconciliation(&path).expect("restart store");
    let mut replay_request = request.clone();
    replay_request.metadata = Some(json!({
        "submission_key": "agent-task:v1:restart",
        "reconciled_from": "durable_detached_handoff_intent",
        "transport": "controller-recovery",
    }));
    let replay = restarted
        .submit_remote_runner_job(replay_request)
        .expect("replay");
    assert_eq!(replay.id, accepted.id);
    request.command.push("different".to_string());
    let conflict = restarted
        .submit_remote_runner_job(request)
        .expect_err("conflict fails closed");
    assert_eq!(conflict.code, crate::ErrorCode::ValidationInvalidArgument);
    assert_eq!(
        conflict.details["schema"],
        json!("homeboy/remote-runner-submission-conflict/v1")
    );
    assert_eq!(conflict.details["accepted_job_id"], json!(accepted.id));
}

#[test]
fn remote_runner_submission_key_concurrent_replay_creates_one_job() {
    let store = JobStore::default();
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request.metadata = Some(json!({ "submission_key": "agent-task:v1:concurrent" }));
    let first_store = store.clone();
    let first_request = request.clone();
    let first = std::thread::spawn(move || first_store.submit_remote_runner_job(first_request));
    let second_store = store.clone();
    let second = std::thread::spawn(move || second_store.submit_remote_runner_job(request));
    let first = first.join().expect("first thread").expect("first submit");
    let second = second
        .join()
        .expect("second thread")
        .expect("second submit");
    assert_eq!(first.id, second.id);
    assert_eq!(store.list().len(), 1);
}

#[test]
fn remote_runner_submission_key_conflicts_on_all_execution_semantic_inputs() {
    let store = JobStore::default();
    let mut base = remote_runner_request("homeboy-lab", Some("extrachill"));
    base.metadata = Some(json!({ "submission_key": "agent-task:v1:semantic-inputs" }));
    store
        .submit_remote_runner_job(base.clone())
        .expect("accept baseline");

    let mut variants = Vec::new();
    let mut changed_env = base.clone();
    changed_env
        .env
        .insert("PUBLIC_FLAG".to_string(), "changed".to_string());
    variants.push(changed_env);
    let mut changed_capture = base.clone();
    changed_capture.capture_patch = !changed_capture.capture_patch;
    variants.push(changed_capture);
    let mut changed_paths = base.clone();
    changed_paths
        .require_paths
        .push("/opt/required".to_string());
    variants.push(changed_paths);
    let mut changed_source = base.clone();
    changed_source.source_snapshot = Some(SourceSnapshot::existing_remote(
        "homeboy-lab",
        "/srv/other-source",
        Some("/srv"),
    ));
    variants.push(changed_source);
    let mut changed_materialization = base;
    changed_materialization.path_materialization_plan =
        Some(crate::runner_execution_envelope::PathMaterializationPlan::new([]));
    variants.push(changed_materialization);

    for request in variants {
        let error = store
            .submit_remote_runner_job(request)
            .expect_err("semantic input reuse fails closed");
        assert_eq!(
            error.details["schema"],
            json!("homeboy/remote-runner-submission-conflict/v1")
        );
    }
}

#[test]
fn compacted_submission_key_expires_without_creating_a_duplicate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_with_retention(&path, 10, 1).expect("durable store");
    let mut requests = Vec::new();
    for key in ["agent-task:v1:compact-one", "agent-task:v1:compact-two"] {
        let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
        request.metadata = Some(json!({ "submission_key": key }));
        let job = store
            .submit_remote_runner_job(request.clone())
            .expect("submit");
        store
            .inner
            .lock()
            .expect("store")
            .jobs
            .get_mut(&job.id)
            .expect("job")
            .job
            .status = JobStatus::Succeeded;
        requests.push((key.to_string(), request));
    }
    store.persist().expect("compact terminal jobs");
    let expired_key = store
        .inner
        .lock()
        .expect("store")
        .expired_submission_keys
        .keys()
        .next()
        .cloned()
        .expect("compacted submission tombstone");
    let request = requests
        .into_iter()
        .find_map(|(key, request)| (key == expired_key).then_some(request))
        .expect("expired request");
    let error = store
        .submit_remote_runner_job(request)
        .expect_err("expired replay is explicit");
    assert_eq!(
        error.details["schema"],
        json!("homeboy/remote-runner-submission-expired/v1")
    );
    assert_eq!(store.list().len(), 1);
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
