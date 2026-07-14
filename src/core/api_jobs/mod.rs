mod persistence;
mod remote_runner;
mod store;
mod summary;
mod types;

pub use remote_runner::{
    JobArtifactMetadata, RemoteRunnerJobClaim, RemoteRunnerJobRequest, RemoteRunnerJobResult,
    RunnerJobLifecycleMetadata,
};
pub use store::{JobHandle, JobRunner, JobStore};
pub use summary::active_runner_job_run_summary;
pub use types::{
    ActiveRunnerJobRunSummary, ActiveRunnerJobSummary, DaemonLeaseJobDiagnostics, Job,
    JobClaimMetadata, JobEvent, JobEventKind, JobStatus, LeaselessOrphanAffectedJob,
    LeaselessOrphanJobDiagnostics, RunnerJobLifecycleOwner, RunnerJobSource,
};

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::persistence::recovered_terminal_from_result;
    use super::store::RecoveredTerminalJob;
    use super::*;
    use crate::core::secret_env_plan::SecretEnvPlan;
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
            Some("control plane lost before the job reached a terminal status")
        );
        assert!(stale.finished_at_ms.is_some());

        let events = reopened.events(job.id).expect("events persist");
        assert!(events.iter().any(|event| {
            event.kind == JobEventKind::Error
                && event.data.as_ref().is_some_and(|data| {
                    data["reason"] == json!("orphaned_after_control_plane_loss")
                        && data["classification"]["kind"]
                            == json!("orphaned_after_control_plane_loss")
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
        matching
            .start(unfinished_job.id)
            .expect("start unfinished job");
        matching
            .append_event(
                unfinished_job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": "heartbeat", "process": { "root_pid": u32::MAX } })),
            )
            .expect("record dead child heartbeat");

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
        store.start(job.id).expect("start job");
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": "heartbeat", "process": { "root_pid": 4242 } })),
            )
            .expect("record child heartbeat");

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
        store.start(job.id).expect("start job");
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": "heartbeat", "process": { "root_pid": 4242 } })),
            )
            .expect("record child heartbeat");

        let diagnostics = store
            .reconcile_dead_daemon_lease_jobs_with_child_liveness("lease-dead", |_| false)
            .expect("reconcile dead lease");

        assert!(diagnostics.protected_job_ids.is_empty());
        assert_eq!(diagnostics.terminalized_count(), 1);
        assert_eq!(store.get(job.id).expect("job").status, JobStatus::Failed);
    }

    #[test]
    fn dead_child_recovers_linked_terminal_result_and_artifacts() {
        let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("start job");
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "process": { "root_pid": 4242 } })),
            )
            .expect("heartbeat");
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
        store.start(job.id).expect("start job");
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "process": { "root_pid": 4242 } })),
            )
            .expect("heartbeat");
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
    fn dead_lease_reconciliation_is_idempotent_after_terminalizing_a_dead_child() {
        let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("start job");
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": "heartbeat", "process": { "root_pid": 4242 } })),
            )
            .expect("record child heartbeat");

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
        let legacy = JobStore::open_without_reconciliation(&path).expect("open legacy store");
        let succeeded = legacy.create("runner.exec");
        legacy.start(succeeded.id).expect("start succeeded job");
        legacy
            .append_event(
                succeeded.id,
                JobEventKind::Result,
                None,
                Some(json!({ "exit_code": 0 })),
            )
            .expect("record result");
        let unfinished = legacy.create("runner.exec");
        legacy.start(unfinished.id).expect("start unfinished job");

        let recovery = JobStore::open_without_reconciliation(&path).expect("open recovery");
        let diagnostics = recovery
            .reconcile_leaseless_orphan_jobs()
            .expect("reconcile legacy jobs");
        assert_eq!(diagnostics.reconciled_count(), 2);
        assert!(diagnostics.reconciled_job_ids.contains(&succeeded.id));
        assert!(diagnostics.reconciled_job_ids.contains(&unfinished.id));
        assert_eq!(
            recovery.get(succeeded.id).expect("succeeded").status,
            JobStatus::Failed
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
                .is_some_and(|data| data["reason"] == json!("leaseless_orphan_reconciliation"))));

        let leased = JobStore::open_without_reconciliation(&path)
            .expect("open leased store")
            .with_daemon_lease("lease-owned".to_string());
        let leased_job = leased.create("runner.exec");
        leased.start(leased_job.id).expect("start leased job");
        let diagnostics = JobStore::open_without_reconciliation(&path)
            .expect("open refusing recovery")
            .reconcile_leaseless_orphan_jobs()
            .expect("historical lease has no current owner");
        assert_eq!(diagnostics.historical_lease_ids, vec!["lease-owned"]);
        assert_eq!(diagnostics.affected_jobs.len(), 1);
        assert_eq!(
            diagnostics.affected_jobs[0]
                .original_daemon_lease_id
                .as_deref(),
            Some("lease-owned")
        );
        let recovered = JobStore::open_without_reconciliation(&path).expect("reopen recovered");
        assert_eq!(
            recovered.get(leased_job.id).expect("leased job").status,
            JobStatus::Failed
        );
        assert!(recovered
            .events(leased_job.id)
            .expect("events")
            .iter()
            .any(|event| event
                .data
                .as_ref()
                .is_some_and(|data| data["original_daemon_lease_id"] == json!("lease-owned"))));
    }

    #[test]
    fn leaseless_recovery_enumerates_and_terminalizes_multiple_historical_leases() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("jobs.json");
        let first = JobStore::open_without_reconciliation(&path)
            .expect("open first")
            .with_daemon_lease("lease-first".to_string());
        let first_job = first.create("runner.exec");
        first.start(first_job.id).expect("start first");
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
        second.start(second_job.id).expect("start second");

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
            event.kind == JobEventKind::Stdout
                && event.message.as_deref() == Some("running wp test")
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

    #[test]
    fn remote_runner_job_submit_derives_implicit_command_secret_names() {
        let store = JobStore::default();
        let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
        request.command = vec![
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--ingress".to_string(),
            "https://preview-broker.example.test".to_string(),
            "--public-host".to_string(),
            "preview.example.test".to_string(),
            "--local-origin".to_string(),
            "http://127.0.0.1:8888".to_string(),
        ];
        let plan = crate::core::plan::HomeboyPlan::builder_for_description(
            crate::core::plan::PlanKind::LabOffload,
            "test",
        )
        .build();
        let command_contract = crate::core::runner::LabOffloadCommand {
            command: crate::command_contract::LabCommandContract::portable(
                "tunnel preview-client start",
                None,
                false,
                &[],
            ),
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            workload: None,
        };
        request.runner_workload = Some(crate::core::runner::workload::build_runner_workload(
            crate::core::runner::workload::RunnerWorkloadBuildInput {
                plan: &plan,
                command: &command_contract,
                capture_patch: request.capture_patch,
                mutation_flag: None,
                allow_dirty_lab_workspace: false,
                runner_id: "homeboy-lab",
                runner_mode: "reverse_broker",
                assignment_source: "broker",
                status: "queued",
                remote_workspace: request.cwd.as_deref(),
                fallback_reason: None,
                workspace_mapping_ref: None,
                proof_id: None,
            },
        ));

        let job = store
            .submit_remote_runner_job(request)
            .expect("remote runner job derives implicit secret names before validation");

        let inner = store.inner.lock().expect("job store mutex poisoned");
        let stored = inner.jobs.get(&job.id).expect("job stored");
        let remote_runner = stored.remote_runner.as_ref().expect("remote runner job");
        assert_eq!(
            remote_runner.request.secret_env_names,
            vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]
        );
        assert_eq!(
            remote_runner
                .execution_request
                .as_ref()
                .expect("execution request")
                .secret_env_names,
            vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]
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
        assert_eq!(
            active.claim.claimed_by_runner_id.as_deref(),
            Some("homeboy-lab")
        );
        assert_eq!(active.claim.claim_id, claim.job.claim_id);
        assert_eq!(active.claim.claimed_at_ms, claim.job.claimed_at_ms);
        assert_eq!(
            active.claim.claim_expires_at_ms,
            claim.job.claim_expires_at_ms
        );
        assert!(active.claim_expires_in_ms.is_some());
        assert!(active.heartbeat_age_ms <= active.elapsed_ms);
    }

    #[test]
    fn active_runner_job_summary_prefers_typed_lifecycle_metadata() {
        let store = JobStore::default();
        let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
        request.lifecycle = Some(RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("lab_agent_task".to_string()),
            durable_run_id: Some("agent-task-run-123".to_string()),
            active_child_count: Some(2),
            active_cell_count: Some(7),
        });
        request.metadata = Some(json!({
            "source": "legacy-source",
            "kind": "legacy-kind",
            "durable_run_id": "legacy-run",
            "active_child_count": 99,
            "active_cell_count": 100,
        }));
        let job = store
            .submit_remote_runner_job(request)
            .expect("remote runner job queues");

        let summary = store
            .active_runner_jobs()
            .into_iter()
            .find(|summary| summary.job_id == job.id.to_string())
            .expect("active job summary");

        assert_eq!(summary.source, "reverse-broker");
        assert_eq!(summary.kind, "lab_agent_task");
        assert_eq!(
            summary.durable_run_id.as_deref(),
            Some("agent-task-run-123")
        );
        assert_eq!(summary.active_child_count, Some(2));
        assert_eq!(summary.active_cell_count, Some(7));
        assert_eq!(
            summary
                .lifecycle
                .as_ref()
                .and_then(|lifecycle| lifecycle.durable_run_id.as_deref()),
            Some("agent-task-run-123")
        );

        let runner_job = crate::core::runner::RunnerJob::from(&summary);
        assert_eq!(runner_job.source, "reverse-broker");
        assert_eq!(
            runner_job
                .lifecycle
                .as_ref()
                .and_then(|lifecycle| lifecycle.durable_run_id.as_deref()),
            Some("agent-task-run-123")
        );
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
            Some("orphaned_after_control_plane_loss")
        );
        assert_eq!(stale_jobs[0].retryable, Some(true));
        assert_eq!(
            stale_jobs[0].stale_reason.as_deref(),
            Some("control plane lost before the job reached a terminal status")
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

    #[test]
    fn remote_runner_request_compiles_canonical_execution_envelope() {
        let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
        request.command = vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()];
        request
            .env
            .insert("PUBLIC_VALUE".to_string(), "visible".to_string());
        request
            .env
            .insert("TOKEN_A".to_string(), "secret".to_string());
        request.env.insert(
            crate::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV.to_string(),
            "1".to_string(),
        );
        request.secret_env_names = vec!["TOKEN_A".to_string()];
        request.secret_env_plan = SecretEnvPlan::from_secret_env_names(["TOKEN_B".to_string()]);
        request.require_paths = vec!["/srv/extrachill/cache".to_string()];
        request.lifecycle = Some(RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: Some("run-123".to_string()),
            active_child_count: Some(2),
            active_cell_count: Some(3),
        });

        let envelope = request.execution_envelope();
        let dispatch = envelope.dispatch.expect("dispatch payload");

        assert_eq!(envelope.source.kind, "remote_runner_job_request");
        assert_eq!(dispatch.runner_id, "homeboy-lab");
        assert_eq!(dispatch.project_id.as_deref(), Some("extrachill"));
        assert_eq!(dispatch.command, vec!["sh", "-c", "printf ok"]);
        assert_eq!(dispatch.cwd.as_deref(), Some("/srv/extrachill"));
        assert_eq!(dispatch.env["PUBLIC_VALUE"], "visible");
        assert!(!dispatch
            .env
            .contains_key(crate::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV));
        assert_eq!(dispatch.require_paths, vec!["/srv/extrachill/cache"]);
        assert!(dispatch.source_snapshot.is_some());
        assert_eq!(
            envelope
                .secret_env
                .expect("secret env plan")
                .secret_env_names(),
            vec!["TOKEN_A".to_string(), "TOKEN_B".to_string()]
        );
        let lifecycle = envelope.lifecycle.expect("lifecycle");
        assert_eq!(lifecycle.source.as_deref(), Some("reverse-broker"));
        assert_eq!(lifecycle.kind.as_deref(), Some("runner.exec"));
        assert_eq!(lifecycle.durable_run_id.as_deref(), Some("run-123"));
        assert_eq!(lifecycle.active_child_count, Some(2));
        assert_eq!(lifecycle.active_cell_count, Some(3));
        assert_eq!(envelope.result_refs.run_id.as_deref(), Some("run-123"));
        assert!(!request
            .public_metadata()
            .env
            .contains_key(crate::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV));
    }

    #[test]
    fn remote_runner_job_enqueue_persists_run_ref_metadata() {
        let store = JobStore::default();
        let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
        request.lifecycle = Some(RunnerJobLifecycleMetadata {
            source: Some("runner-daemon".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: Some("agent-task-run-123".to_string()),
            active_child_count: None,
            active_cell_count: None,
        });

        let job = store.submit_remote_runner_job(request).expect("job queued");
        let events = store.events(job.id).expect("job events");

        assert!(events.iter().any(|event| {
            event.data.as_ref().is_some_and(|data| {
                data["durable_run_id"] == "agent-task-run-123"
                    && data["agent_task_run_id"] == "agent-task-run-123"
            })
        }));
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
            secret_env_plan: Default::default(),
            env_materialization: None,
            capture_patch: true,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                runner_id,
                "/srv/extrachill",
                Some("/srv"),
            )),
            path_materialization_plan: None,
            require_paths: Vec::new(),
            runner_workload: None,
            lifecycle: None,
            metadata: Some(json!({ "submitted_by": "controller" })),
        }
    }
}
