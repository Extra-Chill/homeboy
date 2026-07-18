#![cfg(test)]

use std::collections::HashMap;
use std::fs;

use serde_json::json;

use super::persistence::recovered_terminal_from_result;
use super::store::{LinkedDurableRunResolution, RecoveredTerminalJob};
use super::*;
use crate::secret_env_plan::SecretEnvPlan;
use crate::source_snapshot::SourceSnapshot;
use crate::Error;
use uuid::Uuid;

#[test]
fn test_create() {
    let store = JobStore::default();
    let job = store.create("audit");

    assert_eq!(job.operation, "audit");
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.event_count, 1);
    assert!(job.source_snapshot.is_none());
}

#[test]
fn active_count_reads_durable_jobs_without_reconciling_them() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("open store");
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");

    assert_eq!(
        JobStore::active_count_at_path(&path).expect("active count"),
        1
    );
    assert_eq!(
        store.get(job.id).expect("job remains present").status,
        JobStatus::Running,
        "status inspection must not reconcile an in-flight daemon job"
    );
}

#[test]
fn expired_local_child_reservation_releases_capacity_and_persists_retryable_failure() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("open store");
    let job = store.create("runner.exec");
    let reserved_at = 10_000;
    store
        .reserve_local_child_at(job.id, reserved_at)
        .expect("persist reservation");

    assert!(store
        .reconcile_expired_local_child_reservations_at(reserved_at + 59_999)
        .expect("reservation remains leased")
        .is_empty());
    assert_eq!(
        JobStore::active_count_at_path(&path).expect("active capacity before expiry"),
        1
    );

    assert_eq!(
        store
            .reconcile_expired_local_child_reservations_at(reserved_at + 60_000)
            .expect("expire reservation"),
        vec![job.id]
    );
    assert_eq!(
        store.get(job.id).expect("terminal job").status,
        JobStatus::Failed
    );
    assert_eq!(
        JobStore::active_count_at_path(&path).expect("expired capacity released"),
        0
    );
    let events = JobStore::open_without_reconciliation(&path)
        .expect("reopen durable terminal result")
        .events(job.id)
        .expect("events");
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Result
            && event.data.as_ref().is_some_and(|data| {
                data["reason"] == "local_child_reservation_expired" && data["retryable"] == true
            })
    }));
}

#[test]
fn pid_bound_local_child_is_not_expired_by_its_former_reservation_deadline() {
    let store = JobStore::default();
    let job = store.create("runner.exec");
    store
        .reserve_local_child_at(job.id, 10_000)
        .expect("reserve child");
    store
        .start_with_reserved_child_identity(
            job.id,
            std::process::id(),
            None,
            super::store::LocalChildStartDiscriminator::Unsupported {
                evidence: "test owns a live current-process identity".to_string(),
            },
        )
        .expect("atomically bind live child identity");

    assert!(store
        .reconcile_expired_local_child_reservations_at(1_000_000)
        .expect("do not expire spawned child")
        .is_empty());
    assert_eq!(
        store.get(job.id).expect("live job").status,
        JobStatus::Running
    );
}

#[test]
fn local_child_worker_records_start_before_persisting_child_identity() {
    let store = JobStore::default();
    let runner = store
        .run_local_child_background_with_source_snapshot_metadata_and_path_materialization_plan(
            "runner.exec",
            None,
            None,
            None,
            move |job| {
                job.start_with_reserved_child_identity(
                    std::process::id(),
                    None,
                    super::store::LocalChildStartDiscriminator::Unsupported {
                        evidence: "test child identity".to_string(),
                    },
                )?;
                Ok(serde_json::json!({}))
            },
        );
    runner.handle.join().expect("worker exits");

    let events = store.events(runner.job_id).expect("events");
    let phases = events
        .iter()
        .filter_map(|event| event.data.as_ref()?.get("phase")?.as_str())
        .collect::<Vec<_>>();
    let reserved = phases
        .iter()
        .position(|phase| *phase == "child_reserved")
        .expect("reservation event");
    let worker_started = phases
        .iter()
        .position(|phase| *phase == "local_child_worker_started")
        .expect("worker start event");
    let spawned = phases
        .iter()
        .position(|phase| *phase == "spawned")
        .expect("spawn event");
    assert!(reserved < worker_started && worker_started < spawned);
}

#[test]
fn local_child_worker_persists_typed_setup_failure_before_child_identity() {
    let store = JobStore::default();
    let runner = store
        .run_local_child_background_with_source_snapshot_metadata_and_path_materialization_plan(
            "runner.exec",
            None,
            None,
            None,
            move |_job| Err::<serde_json::Value, _>(Error::internal_unexpected("setup failed")),
        );
    runner.handle.join().expect("worker exits");

    let events = store.events(runner.job_id).expect("events");
    let worker_started = events
        .iter()
        .position(|event| {
            event
                .data
                .as_ref()
                .is_some_and(|data| data["phase"] == "local_child_worker_started")
        })
        .expect("worker start event");
    let setup_failure = events
        .iter()
        .position(|event| {
            event.data.as_ref().is_some_and(|data| {
                data["phase"] == "local_child_worker_failed_before_child_identity"
                    && data["error"] == "setup failed"
            })
        })
        .expect("typed setup failure event");
    assert!(worker_started < setup_failure);
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Error
            && event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("setup failed"))
    }));
    assert_eq!(
        store.get(runner.job_id).expect("job").status,
        JobStatus::Failed
    );
}

#[test]
fn test_create_with_source_snapshot() {
    let store = JobStore::default();
    let snapshot =
        SourceSnapshot::existing_remote("lab", "/srv/homeboy/repo", Some("/srv/homeboy"));
    let job = store.create_with_source_snapshot("runner.exec", Some(snapshot.clone()));

    assert_eq!(job.source_snapshot, Some(snapshot.clone()));
    assert_eq!(
        store.get(job.id).expect("job").source_snapshot,
        Some(snapshot)
    );
}

#[test]
fn test_get() {
    let store = JobStore::default();
    let job = store.create("audit");

    assert_eq!(store.get(job.id).expect("job is readable").id, job.id);
}

#[test]
fn test_list() {
    let store = JobStore::default();
    let first = store.create("audit");
    let second = store.create("lint");

    let mut jobs = store.list();
    jobs.sort_by(|a, b| a.operation.cmp(&b.operation));
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].id, first.id);
    assert_eq!(jobs[1].id, second.id);
}

#[test]
fn local_runner_jobs_retain_durable_identity_in_active_projection() {
    let store = JobStore::default();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let runner = store
        .run_local_child_background_with_source_snapshot_metadata_path_materialization_and_local_runner(
            "runner.exec",
            None,
            None,
            None,
            Some(super::store::LocalRunnerJob {
                runner_id: "homeboy-lab".to_string(),
                command: vec!["homeboy".to_string(), "agent-task".to_string(), "run-plan".to_string()],
                cwd: Some("/runner/worktree".to_string()),
                lifecycle: Some(RunnerJobLifecycleMetadata {
                    source: Some("runner-daemon".to_string()),
                    kind: Some("agent-task-run-plan".to_string()),
                    durable_run_id: Some("agent-task-durable-run".to_string()),
                    active_child_count: None,
                    active_cell_count: None,
                }),
            }),
            move |_job| {
                let _ = wait.recv();
                Ok(serde_json::json!({}))
            },
        );

    let active = store.active_runner_jobs();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].job_id, runner.job_id.to_string());
    assert_eq!(active[0].runner_id, "homeboy-lab");
    assert_eq!(
        active[0].durable_run_id.as_deref(),
        Some("agent-task-durable-run")
    );
    assert_eq!(active[0].kind, "agent-task-run-plan");
    release.send(()).expect("release runner job");
    runner.handle.join().expect("runner job exits");
}

#[test]
fn test_start() {
    let store = JobStore::default();
    let job = store.create("audit");

    let running = store.start(job.id).expect("job starts");
    assert_eq!(running.status, JobStatus::Running);
    assert!(running.started_at_ms.is_some());
}

#[test]
fn test_append_event() {
    let store = JobStore::default();
    let job = store.create("audit");
    store.start(job.id).expect("job starts");

    let event = store
        .append_event(
            job.id,
            JobEventKind::Stdout,
            Some("running audit".to_string()),
            None,
        )
        .expect("stdout event appends");

    assert_eq!(event.kind, JobEventKind::Stdout);
    assert_eq!(event.message.as_deref(), Some("running audit"));
}

#[test]
fn test_complete() {
    let store = JobStore::default();
    let job = store.create("audit");
    store.start(job.id).expect("job starts");

    let completed = store
        .complete(job.id, Some(json!({ "findings": 0 })))
        .expect("job completes");
    assert_eq!(completed.status, JobStatus::Succeeded);
    assert!(completed.finished_at_ms.is_some());
}

#[test]
fn test_fail() {
    let store = JobStore::default();
    let job = store.create("lint");
    store.start(job.id).expect("job starts");

    let failed = store.fail(job.id, "lint failed").expect("job fails");
    assert_eq!(failed.status, JobStatus::Failed);
    assert!(store
        .events(job.id)
        .expect("events are readable")
        .iter()
        .any(|event| event.kind == JobEventKind::Error));
}

#[test]
fn test_cancel() {
    let store = JobStore::default();
    let job = store.create("bench");

    let cancelled = store.cancel(job.id, "user requested").expect("job cancels");
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.started_at_ms.is_none());
    assert!(cancelled.finished_at_ms.is_some());
}

#[test]
fn test_job_id() {
    let store = JobStore::default();
    let runner = store.run_background("test", |job| Ok(job.job_id().to_string()));

    runner.handle.join().expect("worker thread exits cleanly");
    assert_eq!(
        store
            .events(runner.job_id)
            .expect("events are readable")
            .iter()
            .find(|event| event.kind == JobEventKind::Result)
            .and_then(|event| event.data.as_ref()),
        Some(&json!(runner.job_id.to_string()))
    );
}

#[test]
fn test_stdout() {
    let store = JobStore::default();
    let runner = store.run_background("test", |job| {
        job.stdout("stdout line")?;
        Ok(json!(true))
    });

    runner.handle.join().expect("worker thread exits cleanly");
    assert!(store
        .events(runner.job_id)
        .expect("events are readable")
        .iter()
        .any(|event| event.kind == JobEventKind::Stdout));
}

#[test]
fn test_stderr() {
    let store = JobStore::default();
    let runner = store.run_background("test", |job| {
        job.stderr("stderr line")?;
        Ok(json!(true))
    });

    runner.handle.join().expect("worker thread exits cleanly");
    assert!(store
        .events(runner.job_id)
        .expect("events are readable")
        .iter()
        .any(|event| event.kind == JobEventKind::Stderr));
}

#[test]
fn test_progress() {
    let store = JobStore::default();
    let runner = store.run_background("test", |job| {
        job.progress(json!({ "current": 1, "total": 1 }))?;
        Ok(json!(true))
    });

    runner.handle.join().expect("worker thread exits cleanly");
    assert!(store
        .events(runner.job_id)
        .expect("events are readable")
        .iter()
        .any(|event| event.kind == JobEventKind::Progress));
}

#[test]
fn job_lifecycle_records_status_events_in_order() {
    let store = JobStore::default();
    let job = store.create("audit");

    store.start(job.id).expect("job starts");
    store
        .append_event(
            job.id,
            JobEventKind::Stdout,
            Some("running audit".to_string()),
            None,
        )
        .expect("stdout event appends");
    store
        .append_event(
            job.id,
            JobEventKind::Progress,
            None,
            Some(json!({ "current": 1, "total": 2 })),
        )
        .expect("progress event appends");

    store
        .complete(job.id, Some(json!({ "findings": 0 })))
        .expect("job completes");

    let events = store.events(job.id).expect("events are readable");
    let kinds: Vec<JobEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            JobEventKind::Status,
            JobEventKind::Status,
            JobEventKind::Stdout,
            JobEventKind::Progress,
            JobEventKind::Result,
            JobEventKind::Status,
        ]
    );
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert_eq!(
        events.last().unwrap().data,
        Some(json!({ "status": "succeeded" }))
    );
}

#[test]
fn invalid_status_transitions_are_rejected() {
    let store = JobStore::default();
    let job = store.create("lint");

    let err = store
        .complete(job.id, None)
        .expect_err("queued job cannot complete before running");
    assert!(err.to_string().contains("Queued to Succeeded"));
    assert_eq!(
        store.events(job.id).expect("events are readable").len(),
        1,
        "failed transition must not append result or status events"
    );

    store.start(job.id).expect("job starts");
    store.fail(job.id, "lint failed").expect("job fails");

    let err = store
        .cancel(job.id, "too late")
        .expect_err("terminal job cannot be cancelled");
    assert!(err.to_string().contains("Failed to Cancelled"));

    let err = store
        .append_event(
            job.id,
            JobEventKind::Stdout,
            Some("too late".to_string()),
            None,
        )
        .expect_err("terminal job cannot receive more output");
    assert!(err.to_string().contains("terminal job"));
}

#[test]
fn background_runner_captures_result_and_handle_events() {
    let store = JobStore::default();
    let runner = store.run_background("rig-check", |job| {
        job.stdout("checking services")?;
        job.progress(json!({ "checked": 1, "total": 1 }))?;
        Ok(json!({ "ok": true, "job_id": job.job_id().to_string() }))
    });

    runner.handle.join().expect("worker thread exits cleanly");

    let job = store.get(runner.job_id).expect("job is readable");
    assert_eq!(job.status, JobStatus::Succeeded);

    let events = store.events(runner.job_id).expect("events are readable");
    assert_eq!(events[0].kind, JobEventKind::Status);
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Stdout));
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Progress));
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Result));
    assert_eq!(
        events.last().unwrap().data,
        Some(json!({ "status": "succeeded" }))
    );
}

#[test]
fn background_runner_captures_errors_as_failed_jobs() {
    let store = JobStore::default();
    let runner = store.run_background::<serde_json::Value, _>("test", |_job| {
        Err(crate::error::Error::validation_invalid_argument(
            "fixture", "boom", None, None,
        ))
    });

    runner.handle.join().expect("worker thread exits cleanly");

    let job = store.get(runner.job_id).expect("job is readable");
    assert_eq!(job.status, JobStatus::Failed);

    let events = store.events(runner.job_id).expect("events are readable");
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Error
            && event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("boom"))
    }));
    assert_eq!(
        events.last().unwrap().data,
        Some(json!({ "status": "failed" }))
    );
}

#[test]
fn test_open() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store.create("bench");

    store.start(job.id).expect("job starts");
    store
        .append_event(job.id, JobEventKind::Stdout, Some("done".to_string()), None)
        .expect("stdout event appends");
    store
        .complete(job.id, Some(json!({ "ok": true })))
        .expect("job completes");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let persisted = reopened.get(job.id).expect("job persists");
    assert_eq!(persisted.status, JobStatus::Succeeded);
    assert!(persisted.finished_at_ms.is_some());

    let events = reopened.events(job.id).expect("events persist");
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Stdout));
    assert!(events
        .iter()
        .any(|event| event.kind == JobEventKind::Result));
}

#[test]
fn open_quarantines_corrupt_durable_store() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    fs::write(&path, b"{").expect("corrupt store");

    let store = JobStore::open(&path).expect("corrupt store is quarantined");
    assert!(store.list().is_empty());
    assert!(path.exists());

    let quarantined = fs::read_dir(temp.path())
        .expect("read temp dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("jobs.json.corrupt-"))
        .collect::<Vec<_>>();
    assert_eq!(quarantined.len(), 1);
}

#[test]
fn durable_store_reconciles_running_jobs_as_stale_after_restart() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store.create("audit");
    store.start(job.id).expect("job starts");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let stale = reopened.get(job.id).expect("job persists");
    assert_eq!(stale.status, JobStatus::Failed);
    assert_eq!(
        stale.stale_reason.as_deref(),
        Some("control plane lost before the job reached a terminal status")
    );
    assert!(stale.finished_at_ms.is_some());

    let events = reopened.events(job.id).expect("events persist");
    assert!(events.iter().any(|event| {
        event.kind == JobEventKind::Error
            && event.data.as_ref().is_some_and(|data| {
                data["reason"] == json!("orphaned_after_control_plane_loss")
                    && data["classification"]["kind"] == json!("orphaned_after_control_plane_loss")
                    && data["classification"]["recoverable"] == json!(false)
                    && data["classification"]["child"]["output_observed"] == json!(false)
            })
    }));
}

#[test]
fn daemon_lease_reconciliation_recovers_results_and_only_fails_unfinished_matching_jobs() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let matching = JobStore::open_without_reconciliation(&path)
        .expect("open matching")
        .with_daemon_lease("lease-dead".to_string());
    let succeeded_job = matching.create("runner.exec");
    matching
        .start(succeeded_job.id)
        .expect("start succeeded job");
    matching
        .append_event(
            succeeded_job.id,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 0 })),
        )
        .expect("record successful result");
    matching
        .append_event(
            succeeded_job.id,
            JobEventKind::Progress,
            None,
            Some(json!({ "phase": "heartbeat", "process": { "root_pid": 4242 } })),
        )
        .expect("record stale child heartbeat after result");
    let failed_result_job = matching.create("runner.exec");
    matching
        .start(failed_result_job.id)
        .expect("start failed-result job");
    matching
        .append_event(
            failed_result_job.id,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 1 })),
        )
        .expect("record failed result");
    let cancelled_job = matching.create("runner.exec");
    matching
        .start(cancelled_job.id)
        .expect("start cancelled job");
    matching
        .append_event(
            cancelled_job.id,
            JobEventKind::Result,
            None,
            Some(json!({ "status": "cancelled", "exit_code": 0 })),
        )
        .expect("record cancelled result");
    let unfinished_job = matching.create("runner.exec");
    record_test_local_child(&matching, unfinished_job.id, u32::MAX);

    let other = JobStore::open_without_reconciliation(&path)
        .expect("open other")
        .with_daemon_lease("lease-other".to_string());
    let other_job = other.create("runner.exec");
    other.start(other_job.id).expect("start other job");

    let store = JobStore::open_without_reconciliation(&path).expect("open recovery store");
    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |pid| pid == 4242)
        .expect("reconcile matching lease");

    assert_eq!(
        diagnostics.matching_job_ids,
        vec![
            succeeded_job.id,
            failed_result_job.id,
            cancelled_job.id,
            unfinished_job.id,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    assert_eq!(diagnostics.other_lease_job_ids, vec![other_job.id]);
    assert!(diagnostics.unowned_job_ids.is_empty());
    assert!(diagnostics.protected_job_ids.is_empty());
    assert_eq!(
        store.get(succeeded_job.id).expect("succeeded job").status,
        JobStatus::Succeeded
    );
    assert_eq!(
        store
            .get(failed_result_job.id)
            .expect("failed-result job")
            .status,
        JobStatus::Failed
    );
    assert!(store
        .get(failed_result_job.id)
        .expect("failed-result job")
        .stale_reason
        .is_none());
    assert_eq!(
        store.get(cancelled_job.id).expect("cancelled job").status,
        JobStatus::Cancelled
    );
    assert_eq!(
        store.get(unfinished_job.id).expect("unfinished job").status,
        JobStatus::Failed
    );
    assert_eq!(
        store.get(other_job.id).expect("other job").status,
        JobStatus::Running
    );
    assert!(store
        .events(unfinished_job.id)
        .expect("matching events")
        .iter()
        .any(|event| {
            event.kind == JobEventKind::Error
                && event.data.as_ref().is_some_and(|data| {
                    data["reason"] == json!("dead_daemon_lease")
                        && data["daemon_lease_id"] == json!("lease-dead")
                        && data["classification"]["kind"]
                            == json!("orphaned_after_control_plane_loss")
                })
        }));
}

#[test]
fn legacy_unowned_active_job_blocks_dead_lease_reconciliation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let legacy = JobStore::open_without_reconciliation(&path).expect("open legacy store");
    let legacy_job = legacy.create("runner.exec");
    legacy.start(legacy_job.id).expect("start legacy job");

    let store = JobStore::open_without_reconciliation(&path).expect("open recovery store");
    let error = store
        .reconcile_dead_daemon_lease_jobs("lease-dead")
        .expect_err("legacy job blocks automatic recovery");

    assert!(error.message.contains("legacy unowned active job"));
    assert!(error.message.contains(&legacy_job.id.to_string()));
    assert_eq!(
        store.get(legacy_job.id).expect("legacy job").status,
        JobStatus::Running
    );
}

#[test]
fn dead_lease_reconciliation_preserves_a_job_with_a_live_recorded_child() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, 4242);

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |pid| pid == 4242)
        .expect("reconcile dead lease");

    assert_eq!(diagnostics.protected_job_ids, vec![job.id]);
    assert_eq!(diagnostics.terminalized_count(), 0);
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
    assert!(store.events(job.id).expect("events").iter().all(|event| {
        event
            .data
            .as_ref()
            .is_none_or(|data| data["reason"] != json!("dead_daemon_lease"))
    }));
}

#[test]
fn dead_lease_reconciliation_terminalizes_a_job_with_a_dead_recorded_child() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, 4242);

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |_| false)
        .expect("reconcile dead lease");

    assert!(diagnostics.protected_job_ids.is_empty());
    assert_eq!(diagnostics.terminalized_count(), 1);
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Failed);
}

#[test]
fn child_identity_is_durable_before_a_job_becomes_running() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    record_test_local_child(&store, job.id, 4242);

    let running = store.get(job.id).expect("running job");
    assert_eq!(running.status, JobStatus::Running);
    assert!(store.events(job.id).expect("events").iter().any(|event| {
        event.kind == JobEventKind::Progress
            && event.data.as_ref().is_some_and(|data| {
                data["process"]["root_pid"] == 4242
                    && data["process"]["start_discriminator"]["kind"]
                        == if cfg!(target_os = "linux") {
                            "linux_proc_stat_starttime_ticks"
                        } else {
                            "unsupported"
                        }
            })
    }));
}

#[test]
fn missing_child_identity_remains_blocked_without_exact_process_evidence() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    store.start(job.id).expect("start legacy job");

    let error = store
        .reconcile_dead_daemon_lease_jobs("lease-dead")
        .expect_err("missing identity blocks recovery");
    assert!(error.message.contains("no authoritative terminal result"));
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
}

#[test]
fn leaseless_recovery_refuses_missing_typed_child_identity_without_mutation() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = store.create("runner.exec");
    store.start(job.id).expect("start legacy job");
    let event_count = store.events(job.id).expect("events").len();

    let error = store
        .reconcile_leaseless_orphan_jobs()
        .expect_err("missing identity blocks lease-less recovery");

    assert!(error.message.contains("no authoritative terminal result"));
    assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
    assert_eq!(store.events(job.id).expect("events").len(), event_count);
}

#[test]
fn linked_active_or_unresolved_job_blocks_confirmed_recovery_before_persisting() {
    for linked in [
        LinkedDurableRunResolution::Active("run-active".to_string()),
        LinkedDurableRunResolution::Unresolved("run-unresolved".to_string()),
    ] {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("jobs.json");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("store")
            .with_daemon_lease("lease-dead".to_string());
        let selected = store.create("runner.exec");
        store.start(selected.id).expect("start selected");
        let linked_job = store.create("runner.exec");
        store
            .start_with_child_identity(linked_job.id, 4242, "dead-child".to_string())
            .expect("record child");
        let before = std::fs::read(&path).expect("store bytes");

        let result = store
            .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs_and_linked_resolver(
                "lease-dead",
                &[selected.id],
                |_| false,
                |stored| {
                    (stored.job.id == linked_job.id)
                        .then(|| linked.clone())
                        .unwrap_or(LinkedDurableRunResolution::None)
                },
            );

        match linked {
            LinkedDurableRunResolution::Active(_) => assert_eq!(
                result
                    .expect("active returns diagnostics")
                    .protected_job_ids,
                vec![linked_job.id]
            ),
            LinkedDurableRunResolution::Unresolved(_) => assert!(result
                .expect_err("unresolved errors")
                .message
                .contains("cannot be safely resolved")),
            _ => unreachable!(),
        }
        assert_eq!(std::fs::read(&path).expect("store bytes"), before);
    }
}

#[test]
fn confirmed_pidless_job_with_unselected_live_child_returns_diagnostics_without_persisting() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path)
        .expect("store")
        .with_daemon_lease("lease-dead".to_string());
    let confirmed = store.create("runner.exec");
    store.start(confirmed.id).expect("start confirmed job");
    let live_child = store.create("runner.exec");
    store.start(live_child.id).expect("start live child");
    store
        .append_event(
            live_child.id,
            JobEventKind::Progress,
            None,
            Some(json!({ "process": { "root_pid": 4242 } })),
        )
        .expect("record live child");
    let before = std::fs::read(&path).expect("store bytes");

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs_and_linked_resolver(
            "lease-dead",
            &[confirmed.id],
            |pid| pid == 4242,
            |_| LinkedDurableRunResolution::None,
        )
        .expect("live child returns protected diagnostics");

    assert_eq!(diagnostics.protected_job_ids, vec![live_child.id]);
    assert_eq!(std::fs::read(&path).expect("store bytes"), before);
}

#[test]
fn status_evidence_reports_linked_active_and_unresolved_as_blocking() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let active = store.create("runner.exec");
    store.start(active.id).expect("start active");
    let unresolved = store.create("runner.exec");
    store.start(unresolved.id).expect("start unresolved");

    let evidence = store.active_daemon_job_recovery_evidence_with_linked_durable_run_resolver(
        Some("lease-dead"),
        |_| false,
        |stored| match stored.job.id {
            id if id == active.id => LinkedDurableRunResolution::Active("run-active".to_string()),
            id if id == unresolved.id => {
                LinkedDurableRunResolution::Unresolved("run-unresolved".to_string())
            }
            _ => LinkedDurableRunResolution::None,
        },
    );

    assert!(evidence
        .iter()
        .all(|job| job.disposition == DaemonActiveJobRecoveryDisposition::BlockingAmbiguous));
    let active_evidence = evidence
        .iter()
        .find(|job| job.job_id == active.id)
        .expect("active evidence");
    assert_eq!(
        active_evidence.linked_durable_run_id.as_deref(),
        Some("run-active")
    );
    assert_eq!(
        active_evidence.linked_durable_run_state,
        Some(DaemonLinkedDurableRunState::Active)
    );
    let unresolved_evidence = evidence
        .iter()
        .find(|job| job.job_id == unresolved.id)
        .expect("unresolved evidence");
    assert_eq!(
        unresolved_evidence.linked_durable_run_id.as_deref(),
        Some("run-unresolved")
    );
    assert_eq!(
        unresolved_evidence.linked_durable_run_state,
        Some(DaemonLinkedDurableRunState::Unresolved)
    );
}
