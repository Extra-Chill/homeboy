#![allow(dead_code)]

mod persistence;
mod remote_runner;
mod store;
mod summary;
mod types;

pub use remote_runner::{
    JobArtifactMetadata, RemoteRunnerJobClaim, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
pub use store::{JobHandle, JobRunner, JobStore};
pub use summary::active_runner_job_run_summary;
pub use types::{
    ActiveRunnerJobRunSummary, ActiveRunnerJobSummary, Job, JobEvent, JobEventKind, JobStatus,
    RunnerJobLifecycleOwner, RunnerJobSource,
};

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::persistence::recovered_terminal_from_result;
    use super::*;
    use crate::core::source_snapshot::SourceSnapshot;
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
            Err(crate::core::error::Error::validation_invalid_argument(
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
            Some("daemon restarted before the job reached a terminal status")
        );
        assert!(stale.finished_at_ms.is_some());

        let events = reopened.events(job.id).expect("events persist");
        assert!(events.iter().any(|event| {
            event.kind == JobEventKind::Error
                && event
                    .data
                    .as_ref()
                    .is_some_and(|data| data["reason"] == json!("stale_after_daemon_restart"))
        }));
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
                .is_some_and(|data| data["reason"] == json!("stale_after_daemon_restart"))
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

        let reopened =
            JobStore::open_with_event_retention(&path, 3).expect("durable store reopens");
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
    fn remote_runner_job_claim_returns_oldest_matching_job() {
        let store = JobStore::default();
        let other = store
            .submit_remote_runner_job(remote_runner_request("other-lab", Some("extrachill")))
            .expect("other runner job queues");
        let first = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
            .expect("first runner job queues");
        let second = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
            .expect("second runner job queues");

        let claim = store
            .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
            .expect("claim succeeds")
            .expect("matching job is claimed");

        assert_eq!(claim.job.id, first.id);
        assert_eq!(claim.request.runner_id, "homeboy-lab");
        assert_eq!(claim.job.status, JobStatus::Running);
        assert_eq!(
            claim.job.claimed_by_runner_id.as_deref(),
            Some("homeboy-lab")
        );
        assert!(claim.job.claim_id.is_some());
        assert!(claim.job.claim_expires_at_ms.is_some());
        assert_eq!(
            store.get(other.id).expect("other job").status,
            JobStatus::Queued
        );
        assert_eq!(
            store.get(second.id).expect("second job").status,
            JobStatus::Queued
        );
        let active_jobs = store.active_runner_jobs();
        let active = active_jobs
            .iter()
            .find(|job| job.job_id == first.id.to_string())
            .expect("claimed job is active");
        assert_eq!(active.claimed_by_runner_id.as_deref(), Some("homeboy-lab"));
        assert_eq!(active.claim_id, claim.job.claim_id);
        assert_eq!(active.claimed_at_ms, claim.job.claimed_at_ms);
        assert_eq!(active.claim_expires_at_ms, claim.job.claim_expires_at_ms);
        assert!(active.claim_expires_in_ms.is_some());
        assert!(active.heartbeat_age_ms <= active.elapsed_ms);
    }

    #[test]
    fn durable_remote_runner_restart_failure_moves_to_stale_runner_jobs() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("jobs.json");
        let store = JobStore::open(&path).expect("durable store opens");
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
            .expect("remote runner job queues");
        store
            .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
            .expect("claim succeeds")
            .expect("job claimed");

        let reopened = JobStore::open(&path).expect("durable store reopens");

        assert!(
            reopened.active_runner_jobs().is_empty(),
            "abandoned jobs must not remain active after daemon restart"
        );
        let stale_jobs = reopened.stale_runner_jobs();
        assert_eq!(stale_jobs.len(), 1);
        assert_eq!(stale_jobs[0].job_id, job.id.to_string());
        assert_eq!(stale_jobs[0].runner_id, "homeboy-lab");
        assert_eq!(stale_jobs[0].status, JobStatus::Failed);
        assert_eq!(
            stale_jobs[0].lifecycle_state.as_deref(),
            Some("abandoned_after_daemon_restart")
        );
        assert_eq!(stale_jobs[0].retryable, Some(true));
        assert_eq!(
            stale_jobs[0].stale_reason.as_deref(),
            Some("daemon restarted before the job reached a terminal status")
        );
    }

    #[test]
    fn remote_runner_job_claim_respects_concurrency_limit() {
        let store = JobStore::default();
        let first = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
            .expect("first runner job queues");
        let second = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("events")))
            .expect("second runner job queues");

        let first_claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, Some(1))
            .expect("claim succeeds")
            .expect("first job is claimed");
        assert_eq!(first_claim.job.id, first.id);
        let first_claim_id = first_claim.job.claim_id.as_deref().expect("claim id");

        let saturated_claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, Some(1))
            .expect("claim request succeeds");
        assert!(saturated_claim.is_none());
        assert_eq!(
            store.get(second.id).expect("second job").status,
            JobStatus::Queued
        );

        store
            .finish_remote_runner_job(
                first.id,
                "homeboy-lab",
                first_claim_id,
                RemoteRunnerJobResult {
                    exit_code: 0,
                    stdout: None,
                    stderr: None,
                    patch: None,
                    mutation_artifacts: None,
                    data: None,
                    observation_run_ids: Vec::new(),
                    artifacts: Vec::new(),
                    artifact_refs: Vec::new(),
                    metrics: None,
                    capture: None,
                },
            )
            .expect("first job completes");

        let second_claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, Some(1))
            .expect("claim succeeds")
            .expect("second job is claimed after capacity frees");
        assert_eq!(second_claim.job.id, second.id);
    }

    #[test]
    fn remote_runner_job_claim_can_be_filtered_by_project() {
        let store = JobStore::default();
        let wire = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("wire")))
            .expect("wire job queues");
        let events = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("events")))
            .expect("events job queues");

        let claim = store
            .claim_remote_runner_job("homeboy-lab", Some("events"), 30_000, None)
            .expect("claim succeeds")
            .expect("events job is claimed");

        assert_eq!(claim.job.id, events.id);
        assert_eq!(
            store.get(wire.id).expect("wire job").status,
            JobStatus::Queued
        );
    }

    #[test]
    fn remote_runner_job_result_records_terminal_state_and_artifacts() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");
        let claim_id = claim.job.claim_id.expect("claim id");
        store
            .append_remote_runner_event(
                job.id,
                "homeboy-lab",
                &claim_id,
                JobEventKind::Stdout,
                Some("running tests".to_string()),
                None,
            )
            .expect("runner appends stdout");

        let completed = store
            .finish_remote_runner_job(
                job.id,
                "homeboy-lab",
                &claim_id,
                RemoteRunnerJobResult {
                    exit_code: 0,
                    stdout: Some("ok".to_string()),
                    stderr: None,
                    patch: None,
                    mutation_artifacts: None,
                    data: Some(json!({ "summary": "passed" })),
                    observation_run_ids: Vec::new(),
                    artifacts: vec![JobArtifactMetadata {
                        id: "report".to_string(),
                        name: Some("report.json".to_string()),
                        path: Some("/srv/homeboy/report.json".to_string()),
                        url: None,
                        mime: Some("application/json".to_string()),
                        size_bytes: Some(42),
                        sha256: Some("abc123".to_string()),
                        content_base64: None,
                        metadata: Some(json!({ "kind": "test_report" })),
                    }],
                    artifact_refs: Vec::new(),
                    metrics: None,
                    capture: None,
                },
            )
            .expect("runner completes job");

        assert_eq!(completed.status, JobStatus::Succeeded);
        assert_eq!(completed.artifacts.len(), 1);
        assert_eq!(completed.artifacts[0].id, "report");

        let events = store.events(job.id).expect("events are readable");
        assert!(events
            .iter()
            .any(|event| event.kind == JobEventKind::Stdout));
        assert!(events
            .iter()
            .any(|event| event.kind == JobEventKind::Result));
        assert_eq!(
            events.last().unwrap().data,
            Some(json!({ "status": "succeeded" }))
        );
    }

    #[test]
    fn remote_runner_job_failed_result_records_error_and_terminal_state() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");
        let claim_id = claim.job.claim_id.expect("claim id");

        let failed = store
            .finish_remote_runner_job(
                job.id,
                "homeboy-lab",
                &claim_id,
                RemoteRunnerJobResult {
                    exit_code: 1,
                    stdout: None,
                    stderr: Some("nope".to_string()),
                    patch: None,
                    mutation_artifacts: None,
                    data: None,
                    observation_run_ids: Vec::new(),
                    artifacts: Vec::new(),
                    artifact_refs: Vec::new(),
                    metrics: None,
                    capture: None,
                },
            )
            .expect("runner fails job");

        assert_eq!(failed.status, JobStatus::Failed);
        let events = store.events(job.id).expect("events are readable");
        assert!(events.iter().any(|event| {
            event.kind == JobEventKind::Error
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("code 1"))
        }));
    }

    #[test]
    fn remote_runner_job_writes_require_matching_claim_id() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");
        let claim_id = claim.job.claim_id.expect("claim id");

        let wrong_claim_event = store.append_remote_runner_event(
            job.id,
            "homeboy-lab",
            "wrong-claim",
            JobEventKind::Progress,
            Some("late progress".to_string()),
            None,
        );
        assert!(wrong_claim_event.is_err());

        store
            .append_remote_runner_event(
                job.id,
                "homeboy-lab",
                &claim_id,
                JobEventKind::Progress,
                Some("valid progress".to_string()),
                None,
            )
            .expect("matching claim appends progress");
        let events = store.events(job.id).expect("events are readable");
        assert!(!events
            .iter()
            .any(|event| event.message.as_deref() == Some("late progress")));
        assert!(events
            .iter()
            .any(|event| event.message.as_deref() == Some("valid progress")));
    }

    #[test]
    fn remote_runner_job_writes_reject_expired_claims() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");
        let claim_id = claim.job.claim_id.expect("claim id");
        {
            let mut inner = store.inner.lock().expect("job store mutex poisoned");
            inner
                .jobs
                .get_mut(&job.id)
                .expect("job exists")
                .job
                .claim_expires_at_ms = Some(super::persistence::timestamp_ms().saturating_sub(1));
        }

        let expired_event = store.append_remote_runner_event(
            job.id,
            "homeboy-lab",
            &claim_id,
            JobEventKind::Progress,
            Some("expired progress".to_string()),
            None,
        );
        assert!(expired_event.is_err());

        let events = store.events(job.id).expect("events are readable");
        assert!(!events
            .iter()
            .any(|event| event.message.as_deref() == Some("expired progress")));
    }

    #[test]
    fn remote_runner_claim_heartbeat_extends_matching_unexpired_claim() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");
        let claim_id = claim.job.claim_id.expect("claim id");
        let previous_expiry = claim.job.claim_expires_at_ms.expect("claim expiry");

        let renewed = store
            .renew_remote_runner_claim(job.id, "homeboy-lab", &claim_id, 60_000)
            .expect("matching claim renews");

        assert_eq!(renewed.id, job.id);
        assert_eq!(renewed.claim_id.as_deref(), Some(claim_id.as_str()));
        assert!(renewed.claim_expires_at_ms.expect("renewed expiry") > previous_expiry);
    }

    #[test]
    fn remote_runner_claim_heartbeat_rejects_wrong_or_expired_claim() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");
        let claim_id = claim.job.claim_id.expect("claim id");

        let wrong_claim = store.renew_remote_runner_claim(job.id, "homeboy-lab", "wrong", 30_000);
        assert!(wrong_claim.is_err());

        {
            let mut inner = store.inner.lock().expect("job store mutex poisoned");
            inner
                .jobs
                .get_mut(&job.id)
                .expect("job exists")
                .job
                .claim_expires_at_ms = Some(super::persistence::timestamp_ms().saturating_sub(1));
        }

        let expired = store.renew_remote_runner_claim(job.id, "homeboy-lab", &claim_id, 30_000);
        assert!(expired.is_err());
    }

    #[test]
    fn cancelled_remote_runner_job_cannot_be_claimed() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        store.cancel(job.id, "user requested").expect("job cancels");

        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim request succeeds");

        assert!(claim.is_none());
    }

    #[test]
    fn running_remote_runner_job_can_be_cancelled_by_broker() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        store
            .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
            .expect("claim succeeds")
            .expect("job is claimed");

        let cancelled = store
            .cancel_remote_runner_job(job.id, "user requested")
            .expect("remote runner job cancels");

        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(store.events(job.id).expect("events").iter().any(|event| {
            event.kind == JobEventKind::Status && event.message.as_deref() == Some("user requested")
        }));
    }

    #[test]
    fn expired_remote_runner_claims_are_reconciled_as_failed() {
        let store = JobStore::default();
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
            .expect("remote runner job queues");
        let claim = store
            .claim_remote_runner_job("homeboy-lab", None, 1, None)
            .expect("claim succeeds")
            .expect("job is claimed");

        let reconciled = store
            .reconcile_expired_remote_runner_claims(
                claim.job.claim_expires_at_ms.expect("claim expiry") + 1,
            )
            .expect("claims reconcile");

        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].id, job.id);
        assert_eq!(reconciled[0].status, JobStatus::Failed);
        assert!(store.events(job.id).expect("events").iter().any(|event| {
            event.kind == JobEventKind::Error
                && event.message.as_deref() == Some("remote runner claim expired")
        }));
    }

    #[test]
    fn remote_runner_jobs_persist_request_and_claim_state() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("jobs.json");
        let store = JobStore::open(&path).expect("durable store opens");
        let job = store
            .submit_remote_runner_job(remote_runner_request("homeboy-lab", Some("extrachill")))
            .expect("remote runner job queues");

        let reopened = JobStore::open(&path).expect("durable store reopens");
        let claim = reopened
            .claim_remote_runner_job("homeboy-lab", Some("extrachill"), 30_000, None)
            .expect("claim succeeds")
            .expect("persisted job is claimed");

        assert_eq!(claim.job.id, job.id);
        assert_eq!(claim.request.command, vec!["homeboy", "test"]);
        assert_eq!(claim.request.project_id.as_deref(), Some("extrachill"));
    }

    fn remote_runner_request(runner_id: &str, project_id: Option<&str>) -> RemoteRunnerJobRequest {
        RemoteRunnerJobRequest {
            runner_id: runner_id.to_string(),
            project_id: project_id.map(str::to_string),
            operation: "runner.exec".to_string(),
            command: vec!["homeboy".to_string(), "test".to_string()],
            cwd: Some("/srv/extrachill".to_string()),
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            capture_patch: true,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                runner_id,
                "/srv/extrachill",
                Some("/srv"),
            )),
            require_paths: Vec::new(),
            runner_workload: None,
            metadata: Some(json!({ "submitted_by": "controller" })),
        }
    }
}
