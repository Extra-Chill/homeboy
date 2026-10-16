#![cfg(test)]

use std::sync::{Arc, Barrier};

use serde_json::json;

use super::*;
use crate::observation::{ArtifactRecord, RunRecord};

fn direct_local_runner(runner_id: &str) -> LocalRunnerJob {
    LocalRunnerJob {
        runner_id: runner_id.to_string(),
        command: vec!["homeboy".to_string(), "test".to_string()],
        cwd: Some("/srv/extrachill".to_string()),
        lifecycle: None,
        workspace_owner_lease: None,
    }
}

fn staged_runner_request(runner_id: &str) -> RemoteRunnerJobRequest {
    let mut request = remote_runner_request(runner_id, None);
    request.operation = "runner_staged_execution".to_string();
    request.metadata = Some(json!({
        "staged_source_artifact": { "id": "sealed:test" },
        "staged_workspace_materialization": { "workspace": "test" },
    }));
    request
}

#[test]
fn exact_staged_adoption_retains_uuid_and_replays() {
    let store = JobStore::default();
    let original = store
        .submit_runner_api_fixture(staged_runner_request("homeboy-lab"))
        .expect("queue original staged job");
    let expected = store
        .queued_staged_execution(original.id, "homeboy-lab")
        .expect("read authoritative staged envelope")
        .expect("queued staged envelope");
    assert_eq!(
        store
            .select_staged_direct_execution(original.id, "homeboy-lab")
            .expect("select direct execution"),
        Some(expected.clone())
    );
    let (adopted, created) = store
        .adopt_queued_staged_execution_for_local_runner(
            original.id,
            &expected,
            direct_local_runner("homeboy-lab"),
            None,
            json!({ "runner_id": "homeboy-lab" }),
            None,
        )
        .expect("adopt exact staged job");
    assert!(created);
    assert_eq!(adopted.id, original.id);
    assert!(store
        .queued_staged_execution(original.id, "homeboy-lab")
        .expect("inspect adopted job")
        .is_none());

    let replay = store
        .adopt_queued_staged_execution_for_local_runner(
            original.id,
            &expected,
            direct_local_runner("homeboy-lab"),
            None,
            json!({ "runner_id": "homeboy-lab" }),
            None,
        )
        .expect("replay adoption");
    assert_eq!(replay.0.id, original.id);
    assert!(!replay.1);
}

#[test]
fn concurrent_exact_staged_adoption_creates_one_local_executor() {
    let store = JobStore::default();
    let job = store
        .submit_runner_api_fixture(staged_runner_request("homeboy-lab"))
        .expect("queue staged job");
    let expected = store
        .queued_staged_execution(job.id, "homeboy-lab")
        .expect("read authoritative staged envelope")
        .expect("queued staged envelope");
    store
        .select_staged_direct_execution(job.id, "homeboy-lab")
        .expect("select direct execution");
    let barrier = Arc::new(Barrier::new(2));
    let first_store = store.clone();
    let first_expected = expected.clone();
    let first_barrier = Arc::clone(&barrier);
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_store
            .adopt_queued_staged_execution_for_local_runner(
                job.id,
                &first_expected,
                direct_local_runner("homeboy-lab"),
                None,
                json!({}),
                None,
            )
            .expect("first adoption")
            .1
    });
    barrier.wait();
    let second = store
        .adopt_queued_staged_execution_for_local_runner(
            job.id,
            &expected,
            direct_local_runner("homeboy-lab"),
            None,
            json!({}),
            None,
        )
        .expect("second adoption")
        .1;
    assert_ne!(first.join().expect("join adoption"), second);
}

#[test]
fn selected_staged_execution_survives_restart() {
    let directory = tempfile::tempdir().expect("temporary durable store");
    let path = directory.path().join(crate::paths::OBSERVATION_DB_FILENAME);
    let store = JobStore::open_without_reconciliation(&path).expect("durable store");
    let job = store
        .submit_runner_api_fixture(staged_runner_request("homeboy-lab"))
        .expect("queue staged job");
    let selected = store
        .select_staged_direct_execution(job.id, "homeboy-lab")
        .expect("select direct execution")
        .expect("selected envelope");
    drop(store);

    let restarted = JobStore::open_without_reconciliation(&path).expect("restart store");
    assert_eq!(
        restarted
            .select_staged_direct_execution(job.id, "homeboy-lab")
            .expect("replay selection"),
        Some(selected)
    );
}

#[test]
fn new_daemon_lease_recovers_adopted_pre_worker_job_once() {
    let directory = tempfile::tempdir().expect("temporary durable store");
    let path = directory.path().join(crate::paths::OBSERVATION_DB_FILENAME);
    let old = JobStore::open_without_reconciliation(&path)
        .expect("durable store")
        .with_daemon_lease("old-daemon".to_string());
    let job = old
        .submit_runner_api_fixture(staged_runner_request("homeboy-lab"))
        .expect("queue staged job");
    let expected = old
        .select_staged_direct_execution(job.id, "homeboy-lab")
        .expect("select staged job")
        .expect("selected envelope");
    old.adopt_queued_staged_execution_for_local_runner(
        job.id,
        &expected,
        direct_local_runner("homeboy-lab"),
        Some(crate::source_snapshot::existing_remote(
            "homeboy-lab",
            "/prepared/workspace",
            Some("/prepared"),
        )),
        json!({}),
        None,
    )
    .expect("durably adopt without spawning worker");
    let old_worker_store = old.clone();
    drop(old);

    let recovered_store = JobStore::open_without_reconciliation(&path)
        .expect("reopen store")
        .with_daemon_lease("new-daemon".to_string());
    assert_eq!(
        recovered_store
            .staged_direct_execution_envelope(job.id, "homeboy-lab")
            .expect("read retained sealed envelope"),
        Some(expected.clone())
    );
    let recovered = recovered_store
        .recover_proven_dead_adopted_staged_direct_execution(job.id, "homeboy-lab", "old-daemon")
        .expect("recover abandoned handoff")
        .expect("new generation owns recovery");
    assert_eq!(recovered.id, job.id);
    assert_eq!(
        recovered
            .source_snapshot
            .as_ref()
            .expect("prepared snapshot")
            .remote_path,
        Some("/prepared/workspace".to_string())
    );
    assert!(recovered_store
        .recover_proven_dead_adopted_staged_direct_execution(job.id, "homeboy-lab", "old-daemon",)
        .expect("same generation observes recovery")
        .is_none());
    assert!(old_worker_store
        .reserve_local_child_with_runner_capacity(job.id, "homeboy-lab", 1)
        .is_err());

    let request = LocalRunnerJobRequest {
        operation: "runner.exec".to_string(),
        source_snapshot: recovered.source_snapshot.clone(),
        metadata: None,
        path_materialization_plan: None,
        local_runner: Some(direct_local_runner("homeboy-lab")),
        admission_idempotency_key: None,
    };
    let runner = recovered_store
        .try_run_capacity_queued_local_child_background_with_existing_job(
            request,
            1,
            Some((recovered, true)),
            |handle| {
                handle.start_with_reserved_child_identity(
                    std::process::id(),
                    None,
                    LocalChildStartDiscriminator::Unsupported {
                        evidence: "state-level recovery fixture".to_string(),
                    },
                )?;
                Ok(json!({ "recovered": true }))
            },
        )
        .expect("run recovered job");
    runner.0.handle.join().expect("join recovered worker");
    assert_eq!(
        recovered_store.get(job.id).expect("recovered job").status,
        JobStatus::Succeeded
    );
}

#[test]
fn active_runner_jobs_include_daemon_local_jobs_counted_by_freshness() {
    let store = JobStore::default();
    let job = store.create("runner.exec");

    let active = store.active_runner_jobs();

    assert_eq!(active.len(), 1);
    assert_eq!(active[0].job_id, job.id.to_string());
    assert_eq!(active[0].source, "daemon");
}

#[test]
fn daemon_exec_projection_survives_event_retention_and_separates_run_identity() {
    let store = JobStore::default();
    let job = store.create_with_source_snapshot_and_metadata(
        "runner.exec",
        None,
        Some(json!({
            "runner_job_projection": {
                "runner_id": "homeboy-lab",
                "command": "homeboy agent-task status durable-run-8341",
                "cwd": "/runner/homeboy",
                "source": "runner-daemon",
                "kind": "runner.exec",
                "lifecycle": {
                    "source": "runner-daemon",
                    "kind": "runner.exec",
                    "durable_run_id": "durable-run-8341"
                }
            }
        })),
    );

    let active = store.active_runner_jobs();
    assert_eq!(active[0].runner_id, "homeboy-lab");
    assert_eq!(
        active[0].durable_run_id.as_deref(),
        Some("durable-run-8341")
    );
    assert_eq!(
        active_runner_job_run_summary_if_durable(active[0].clone())
            .expect("durable run projection")
            .id,
        "durable-run-8341"
    );

    store
        .inner
        .lock()
        .expect("job store")
        .jobs
        .get_mut(&job.id)
        .expect("job")
        .events
        .clear();
    let retained = store.active_runner_jobs();
    assert_eq!(retained[0].runner_id, "homeboy-lab");
    assert_eq!(
        retained[0].durable_run_id.as_deref(),
        Some("durable-run-8341")
    );
}

#[test]
fn remote_runner_result_observation_details_are_additive() {
    let legacy: RemoteRunnerJobResult = serde_json::from_value(json!({ "exit_code": 0 }))
        .expect("old result payload remains readable");
    assert!(legacy.observation_run_details.is_empty());

    let run = RunRecord {
        id: "run-1".to_string(),
        kind: "fuzz".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        finished_at: Some("2026-01-01T00:01:00Z".to_string()),
        status: "succeeded".to_string(),
        metadata_json: json!({}),
        ..Default::default()
    };
    let artifact = ArtifactRecord {
        id: "report".to_string(),
        run_id: run.id.clone(),
        kind: "report".to_string(),
        artifact_type: "file".to_string(),
        path: "/runner/report.json".to_string(),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: Some("abc123".to_string()),
        size_bytes: Some(42),
        mime: Some("application/json".to_string()),
        metadata_json: json!({ "complete": true }),
        created_at: "2026-01-01T00:01:00Z".to_string(),
    };
    let result = RemoteRunnerJobResult {
        exit_code: 0,
        stdout: None,
        stderr: None,
        patch: None,
        mutation_artifacts: None,
        data: None,
        observation_run_ids: vec![run.id.clone()],
        observation_run_details: vec![RemoteRunnerObservationRunDetail::v1(run, vec![artifact])],
        observation_run_details_compatibility_degraded: false,
        artifacts: Vec::new(),
        artifact_refs: Vec::new(),
        metrics: None,
        capture: None,
    };
    let serialized = serde_json::to_value(&result).expect("serialize result");
    assert_eq!(
        serialized["observation_run_details"][0]["schema"],
        RemoteRunnerObservationRunDetail::SCHEMA_V1
    );
}

#[test]
fn dead_daemon_recovery_preserves_queued_remote_work() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let first = store
        .submit_runner_api_fixture(remote_runner_request("homeboy-lab", None))
        .expect("remote job queues");
    let second = store
        .submit_runner_api_fixture(remote_runner_request("homeboy-lab", None))
        .expect("second remote job queues");

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs("lease-dead")
        .expect("remote jobs never block daemon replacement");
    assert_eq!(
        diagnostics.preserved_remote_job_ids,
        vec![first.id, second.id]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    for job in [first.id, second.id] {
        assert_eq!(
            store.get(job).expect("preserved job").status,
            JobStatus::Queued
        );
    }
}

#[test]
fn remote_runner_jobs_persist_envelope() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join(crate::paths::OBSERVATION_DB_FILENAME);
    let store = JobStore::open(&path).expect("durable store opens");
    let job = store
        .submit_runner_api_fixture(staged_runner_request("homeboy-lab"))
        .expect("remote runner job queues");

    let reopened = JobStore::open(&path).expect("durable store reopens");
    let envelope = reopened
        .queued_staged_execution(job.id, "homeboy-lab")
        .expect("read persisted envelope")
        .expect("persisted job remains queued");
    let dispatch = envelope.dispatch.as_ref().expect("typed dispatch");
    assert_eq!(dispatch.command, vec!["homeboy", "test"]);
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

    let job = store
        .submit_runner_api_fixture(request)
        .expect("job queued");
    let events = store.events(job.id).expect("job events");

    assert!(events.iter().any(|event| {
        event.data.as_ref().is_some_and(|data| {
            data["durable_run_id"] == "agent-task-run-123"
                && data["agent_task_run_id"] == "agent-task-run-123"
        })
    }));
}
