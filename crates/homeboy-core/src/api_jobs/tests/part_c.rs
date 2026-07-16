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
fn remote_runner_job_submit_derives_implicit_command_secret_names() {
    // This test exercises runner-augmented secret-env derivation, which lives
    // behind the RunnerJobPreparationProvider hook. Register the runner provider
    // (normally done at CLI startup) so the augmentation runs.
    crate::runner::register_runner_job_preparation_provider();
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
    let plan = crate::plan::HomeboyPlan::builder_for_description(
        crate::plan::PlanKind::LabOffload,
        "test",
    )
    .build();
    let command_contract = crate::runner::LabOffloadCommand {
        command: crate::lab_contract::LabCommandContract::portable(
            "tunnel preview-client start",
            None,
            false,
            &[],
        ),
        required_extensions: Vec::new(),
        required_capabilities: Vec::new(),
        workload: None,
    };
    request.runner_workload = Some(crate::runner::workload::build_runner_workload(
        crate::runner::workload::RunnerWorkloadBuildInput {
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

    let runner_job = crate::runner::RunnerJob::from(&summary);
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
fn dead_daemon_recovery_preserves_remote_work_and_uses_broker_claim_reconciliation() {
    let store = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let queued = store
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
        .expect("remote job queues");
    let claimed = store
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
        .expect("second remote job queues");
    let claim = store
        .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
        .expect("claim succeeds")
        .expect("queued remote work is claimed");
    assert_eq!(claim.job.id, queued.id);
    let claim_id = claim.job.claim_id.clone().expect("claim id");

    let diagnostics = store
        .reconcile_dead_daemon_lease_jobs("lease-dead")
        .expect("remote jobs never block daemon replacement");
    assert_eq!(
        diagnostics.preserved_remote_job_ids,
        vec![claimed.id, queued.id]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        store.get(claimed.id).expect("claimed job").status,
        JobStatus::Queued
    );
    assert_eq!(
        store.get(queued.id).expect("running job").status,
        JobStatus::Running
    );

    let completed = store
        .finish_remote_runner_job(
            queued.id,
            "homeboy-lab",
            &claim_id,
            RemoteRunnerJobResult {
                exit_code: 0,
                stdout: Some("completed after replacement".to_string()),
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
        .expect("worker completion remains accepted after replacement");
    assert_eq!(completed.status, JobStatus::Succeeded);

    let expired = store
        .submit_remote_runner_job(remote_runner_request("homeboy-lab", None))
        .expect("expired fixture queues");
    let expired_claim = store
        .claim_remote_runner_job("homeboy-lab", None, 30_000, None)
        .expect("expired fixture claims")
        .expect("expired fixture claim exists");
    assert_eq!(expired_claim.job.id, claimed.id);
    {
        let mut inner = store.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .get_mut(&expired_claim.job.id)
            .expect("claimed job")
            .job
            .claim_expires_at_ms = Some(super::persistence::timestamp_ms().saturating_sub(1));
    }
    store
        .reconcile_dead_daemon_lease_jobs("lease-dead")
        .expect("expired remote claim uses broker reconciliation");
    assert_eq!(
        store.get(expired_claim.job.id).expect("expired job").status,
        JobStatus::Failed
    );
    assert!(store
        .events(expired_claim.job.id)
        .expect("events")
        .iter()
        .any(|event| event.message.as_deref() == Some("remote runner claim expired")));
    assert_eq!(
        store.get(expired.id).expect("later queued job").status,
        JobStatus::Queued
    );
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
    // Exercises runner-augmented secret-env derivation via the
    // RunnerJobPreparationProvider hook; register the runner provider (normally
    // done at CLI startup) so the augmentation runs.
    crate::runner::register_runner_job_preparation_provider();
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request.command = vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()];
    request
        .env
        .insert("PUBLIC_VALUE".to_string(), "visible".to_string());
    request
        .env
        .insert("TOKEN_A".to_string(), "secret".to_string());
    request.env.insert(
        homeboy_runner_contract::RUNNER_PLACEMENT_RESOLVED_ENV.to_string(),
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
        .contains_key(homeboy_runner_contract::RUNNER_PLACEMENT_RESOLVED_ENV));
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
        .contains_key(homeboy_runner_contract::RUNNER_PLACEMENT_RESOLVED_ENV));
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
