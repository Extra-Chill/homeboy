//! Writer-side durable progress: owning lifecycle writes append to the ledger
//! before CLI/review readers switch off synthetic reconstruction.

use super::*;
use crate::orchestration::{LifecycleStoreLookup, OrchestrationService};
use homeboy_control_plane_contract::RunId;

fn durable_events(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Vec<homeboy_control_plane_contract::ControlPlaneEvent> {
    let run = RunId::new(run_id).expect("run");
    store
        .open_observation_readonly()
        .expect("readonly observations")
        .control_plane_event_stream(&run)
        .expect("read durable stream")
        .expect("run exists")
}

#[test]
fn reserve_provider_execution_appends_running_progress_once() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-provider-running";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");

    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("replayed reservation");

    let events = durable_events(&lifecycle_store, run_id);
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["task.state_changed"]
    );
    assert_eq!(events[0].sequence, 1);
    assert_eq!(events[0].data["state"], "running");
    assert!(events[0].data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running")));

    let logs = logs_in_store(&lifecycle_store, run_id).expect("synthetic logs unchanged");
    assert!(logs.events.iter().any(|event| event.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running"))));
}

#[test]
fn cancel_appends_terminal_provider_progress_and_resumes_from_cursor() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-provider-cancelled";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");

    let service = OrchestrationService::new(LifecycleStoreLookup::new(
        AgentTaskLifecycleStore::new(context.path_roots()),
    ));
    let run = RunId::new(run_id).expect("run");
    let first = service.events(&run, None).expect("running page");
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].data["state"], "running");
    let cursor = first.next_cursor.clone();

    cancel_run_in_store(&lifecycle_store, run_id, Some("durable progress cancel"))
        .expect("cancelled");

    let resumed = service
        .events(&run, cursor.as_ref())
        .expect("resume after cancel");
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.events[0].kind, "task.state_changed");
    assert_eq!(resumed.events[0].data["state"], "cancelled");
    assert_eq!(resumed.events[0].sequence, 2);

    let replayed = service
        .events(&run, cursor.as_ref())
        .expect("cursor replay");
    assert_eq!(replayed.events.len(), 1);
    assert_eq!(replayed.events[0].sequence, 2);
}

#[test]
fn logs_read_does_not_append_historical_metadata_progress() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "historical-metadata-only";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.metadata["provider_executions"] = json!([{
            "key": "task-a:1",
            "task_id": "task-a",
            "attempt": 1,
            "backend": "opencode",
            "state": "running",
            "started_at": "2026-07-24T00:00:00Z"
        }]);
    })
    .expect("rewrote historical metadata");

    let after_write = durable_events(&lifecycle_store, run_id).len();
    assert!(after_write > 0, "record writes mint owned progress");
    let logs = logs_in_store(&lifecycle_store, run_id).expect("synthetic historical logs");
    assert!(logs.events.iter().any(|event| event.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running"))));
    assert_eq!(
        durable_events(&lifecycle_store, run_id).len(),
        after_write,
        "logs reads must not mint ledger events"
    );
}

#[test]
fn live_runner_snapshot_appends_runner_progress_once() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "durable-live-runner",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events = vec![homeboy_core::api_jobs::JobEvent {
        sequence: 1,
        job_id: snapshot.job.id,
        kind: homeboy_core::api_jobs::JobEventKind::Progress,
        timestamp_ms: 42,
        message: Some("provider started".to_string()),
        data: Some(json!({
            "provider": "openai/gpt-5.6-terra",
            "phase": "implementing",
            "activity": "editing lifecycle projection"
        })),
    }];

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("live reconciliation");
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("replayed live reconciliation");

    let events = durable_events(&lifecycle_store, &record.run_id);
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["runner.progress"]
    );
    assert_eq!(events[0].sequence, 1);
    assert!(events[0].data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider started")));
    assert_eq!(
        events[0].data["transport"]["provider"],
        "openai/gpt-5.6-terra"
    );

    record.metadata["phase"] = json!("awaiting_runner_synchronization");
    record.provider_handles.clear();
    record.tasks.clear();
    lifecycle_store
        .write_record(&record)
        .expect("later mutation with the same runner event identity");
    assert_eq!(
        durable_events(&lifecycle_store, &record.run_id).len(),
        1,
        "mutable phase, handles, and tasks must not conflict with the first receipt"
    );
}

#[test]
fn conflicting_runner_event_digest_does_not_skip_silently() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "durable-runner-conflict",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events = vec![homeboy_core::api_jobs::JobEvent {
        sequence: 1,
        job_id: snapshot.job.id,
        kind: homeboy_core::api_jobs::JobEventKind::Progress,
        timestamp_ms: 42,
        message: Some("provider started".to_string()),
        data: Some(json!({ "phase": "implementing" })),
    }];
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("live reconciliation");

    snapshot.events[0].message = Some("mutated progress".to_string());
    let error = reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect_err("different digest must not skip");
    assert!(error.message.contains("idempotency"));

    let events = durable_events(&lifecycle_store, &record.run_id);
    assert_eq!(events.len(), 1);
    assert!(events[0].data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider started")));
    let persisted = lifecycle_store
        .read_record(&record.run_id)
        .expect("rolled back record");
    assert_eq!(
        persisted.metadata["runner_job_events"][0]["message"],
        "provider started"
    );
}
