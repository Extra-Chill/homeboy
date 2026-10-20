//! Durable progress writer and canonical reader migration (#13697).

use super::*;
use crate::orchestration::{
    action_event_idempotency_key, prepare_control_plane_event_append, LifecycleStoreLookup,
    OrchestrationService,
};
use base64::Engine;
use homeboy_control_plane_contract::{
    ControlPlaneErrorClass, ControlPlaneEventAppendRequest, ControlPlaneEventSource, RunId,
    CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
};

/// Persist a snapshot directly, bypassing the owning writer so no progress
/// events are minted for it.
fn persist_record_without_writer(store: &AgentTaskLifecycleStore, record: &AgentTaskRunRecord) {
    store
        .open_observation_initialized()
        .expect("observation")
        .upsert_imported_run(&homeboy_core::observation::RunRecord {
            id: record.run_id.clone(),
            kind: "agent-task".to_string(),
            component_id: Some(record.plan_id.clone()),
            started_at: record.submitted_at.clone(),
            finished_at: record.lifecycle.execution.finished_at.clone(),
            status: if record.state().is_terminal() {
                "pass".to_string()
            } else {
                "running".to_string()
            },
            command: Some("homeboy agent-task".to_string()),
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: json!({
                "schema": "homeboy/agent-task-observation-record/v1",
                "agent_task_run": record,
            }),
        })
        .expect("historical run");
}

fn append_ledger_event(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    request: ControlPlaneEventAppendRequest,
) {
    let run = RunId::new(&record.run_id).expect("run");
    let prepared = prepare_control_plane_event_append(&run, record, &request).expect("prepare");
    store
        .open_observation_initialized()
        .expect("observation")
        .append_control_plane_event(
            &run,
            &prepared.request,
            &prepared.idempotency_digest,
            &prepared.request_digest,
        )
        .expect("append");
}

fn action_append_request(
    operation_key: &str,
    kind: &str,
    data: Value,
) -> ControlPlaneEventAppendRequest {
    ControlPlaneEventAppendRequest {
        schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
        idempotency_key: action_event_idempotency_key(operation_key, kind),
        actor: "test".to_string(),
        kind: kind.to_string(),
        source: ControlPlaneEventSource {
            component: "control-plane".to_string(),
            instance: None,
        },
        occurred_at: Some("2026-07-24T00:00:01Z".to_string()),
        task: None,
        attempt: None,
        execution: None,
        data,
        artifacts: Vec::new(),
        evidence: Vec::new(),
    }
}

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
            .map(|event| event.data["state"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["queued", "running"]
    );
    assert_eq!(events[1].sequence, 2);
    assert_eq!(events[1].data["state"], "running");
    assert!(events[1].data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running")));

    let logs = logs_in_store(&lifecycle_store, run_id).expect("canonical logs");
    assert!(logs.events.iter().any(|event| event.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running"))));
}

/// A provider that outlives the observation reconciler's ownerless threshold
/// must still record its terminal state. The reconciler used to stamp the live
/// row `stale`, after which the terminal-row guard silently refused the
/// lifecycle's `running` writes, leaving the provider ledger `running` and
/// publication refused for a missing succeeded provider runtime.
#[test]
fn provider_terminal_survives_observation_reconcile_of_a_long_run() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-provider-long-run";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");

    let observation = lifecycle_store
        .open_observation_initialized()
        .expect("observation");
    let mut row = observation
        .get_run(run_id)
        .expect("row")
        .expect("row exists");
    row.started_at = (chrono::Utc::now() - chrono::Duration::minutes(45)).to_rfc3339();
    homeboy_core::observation::runs_service::reconcile_orphaned_running_runs(
        &observation,
        vec![row],
        false,
        |_| true,
    )
    .expect("observation reconcile");

    record_provider_execution_terminal_in_store(
        &lifecycle_store,
        run_id,
        &plan.tasks[0].task_id,
        1,
        "succeeded",
    )
    .expect("terminal");

    let record = lifecycle_store.read_record(run_id).expect("record");
    assert_eq!(
        record.metadata["provider_executions"][0]["state"],
        "succeeded"
    );
}

/// #15978 recurrence: a store shared with an older binary whose generic
/// reconciler predates the agent-task skip still stamps a live agent-task row
/// `stale` at 30 minutes. The owner's later `running` writes must supersede
/// that foreign verdict rather than being dropped by the terminal-row guard.
#[test]
fn provider_terminal_survives_a_foreign_stale_stamp_on_a_live_run() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-provider-foreign-stale";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");

    // Exactly what a pre-#15979 reconciler writes: status `stale` plus its
    // marker, leaving the typed record untouched.
    let observation = lifecycle_store
        .open_observation_initialized()
        .expect("observation");
    let mut metadata = observation
        .get_run(run_id)
        .expect("row")
        .expect("row exists")
        .metadata_json;
    metadata["homeboy_reconciled"] = json!({
        "status": "stale",
        "reason": "owner_metadata_missing",
        "owner_pid": null,
    });
    observation
        .finish_run(
            run_id,
            homeboy_core::observation::RunStatus::Stale,
            Some(metadata),
        )
        .expect("foreign stale stamp");

    record_provider_execution_terminal_in_store(
        &lifecycle_store,
        run_id,
        &plan.tasks[0].task_id,
        1,
        "succeeded",
    )
    .expect("terminal");

    let record = lifecycle_store.read_record(run_id).expect("record");
    assert_eq!(
        record.metadata["provider_executions"][0]["state"],
        "succeeded"
    );
    let row = observation
        .get_run(run_id)
        .expect("row")
        .expect("row exists");
    assert_eq!(row.status, "running");
    assert!(row.finished_at.is_none());
    assert!(row.metadata_json.get("homeboy_reconciled").is_none());
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
    assert_eq!(first.events.len(), 2);
    assert_eq!(first.events[0].data["state"], "queued");
    assert_eq!(first.events[1].data["state"], "running");
    let cursor = first.next_cursor.clone();

    cancel_run_in_store(&lifecycle_store, run_id, Some("durable progress cancel"))
        .expect("cancelled");

    let resumed = service
        .events(&run, cursor.as_ref())
        .expect("resume after cancel");
    // Cancellation now durably records its own provenance event (actor,
    // cause, recovery action) alongside the task's terminal state transition
    // (#14925), so resuming past the running cursor sees both.
    assert_eq!(resumed.events.len(), 2);
    assert_eq!(resumed.events[0].kind, "task.state_changed");
    assert_eq!(resumed.events[0].data["state"], "cancelled");
    assert_eq!(resumed.events[0].sequence, 3);
    assert_eq!(resumed.events[1].kind, "run.cancelled");
    assert_eq!(resumed.events[1].data["state"], "cancelled");
    assert_eq!(resumed.events[1].sequence, 4);

    let replayed = service
        .events(&run, cursor.as_ref())
        .expect("cursor replay");
    assert_eq!(replayed.events.len(), 2);
    assert_eq!(replayed.events[0].sequence, 3);
    assert_eq!(replayed.events[1].sequence, 4);
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
fn live_promotion_progress_is_available_to_status_and_logs_before_completion() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-live-promotion";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");

    record_promotion_progress_in_store(
        &lifecycle_store,
        run_id,
        "gate",
        Some("gate-2"),
        Some("gate elapsed=12s last-progress=5s"),
        None,
    )
    .expect("persisted live promotion progress");

    let record = status_in_store(&lifecycle_store, run_id).expect("status record");
    assert_eq!(record.metadata["promotion_progress"]["phase"], "gate");
    assert_eq!(record.metadata["promotion_progress"]["gate"], "gate-2");
    assert_eq!(record.metadata["promotion_progress"]["active"], true);

    let logs = logs_in_store(&lifecycle_store, run_id).expect("live promotion logs");
    assert!(logs.events.iter().any(|event| {
        event.kind == "gate.heartbeat"
            && event.data["gate"] == "gate-2"
            && event.data["heartbeat_at"].is_string()
    }));
    let event_count = durable_events(&lifecycle_store, run_id).len();
    let _ = logs_in_store(&lifecycle_store, run_id).expect("read-only log replay");
    assert_eq!(durable_events(&lifecycle_store, run_id).len(), event_count);
}

#[test]
fn promotion_heartbeat_truncates_large_gate_output_before_event_append() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-large-promotion-output";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");

    record_promotion_progress_in_store(
        &lifecycle_store,
        run_id,
        "gate",
        Some("gate-1"),
        Some("gate heartbeat"),
        Some(&"x".repeat(128 * 1024)),
    )
    .expect("large heartbeat remains persistable");

    let logs = logs_in_store(&lifecycle_store, run_id).expect("heartbeat logs");
    let event = logs
        .events
        .iter()
        .find(|event| event.kind == "gate.heartbeat")
        .expect("heartbeat event");
    let output = event.data["output_tail"]
        .as_str()
        .expect("bounded output tail");
    assert!(output.starts_with("[output truncated]\n"));
    assert!(output.len() <= 8 * 1024 + "[output truncated]\n".len());
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
    let progress = events
        .iter()
        .find(|event| event.kind == "runner.progress")
        .expect("runner progress");
    assert!(progress.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider started")));
    assert_eq!(
        progress.data["transport"]["provider"],
        "openai/gpt-5.6-terra"
    );

    let before = durable_events(&lifecycle_store, &record.run_id).len();
    record.metadata["phase"] = json!("awaiting_runner_synchronization");
    record.provider_handles.clear();
    record.tasks.clear();
    lifecycle_store
        .write_record(&record)
        .expect("later mutation with the same runner event identity");
    assert_eq!(
        durable_events(&lifecycle_store, &record.run_id).len(),
        before,
        "mutable phase, handles, and tasks must not conflict with the first receipt"
    );
}

#[test]
fn oversize_runner_transport_appends_a_bounded_projection() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "durable-oversize-runner-transport",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    // A staged execution adoption carries the whole source package manifest,
    // which grows with the repository past the control-plane event bound.
    let entries: Vec<Value> = (0..2_000)
        .map(|index| json!({ "path": format!("cli/src/module-{index:04}.ts"), "sha256": "a".repeat(64) }))
        .collect();
    let mut snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    snapshot.events = vec![homeboy_core::api_jobs::JobEvent {
        sequence: 1,
        job_id: snapshot.job.id,
        kind: homeboy_core::api_jobs::JobEventKind::Progress,
        timestamp_ms: 42,
        message: Some("queued staged execution adopted for direct daemon execution".to_string()),
        data: Some(json!({
            "phase": "staged_execution_adopted",
            "staged_execution_envelope": { "metadata": { "staged_source_artifact": { "package": { "entries": entries } } } },
        })),
    }];

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("oversize runner event still reconciles");
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("replayed oversize runner event keeps the same receipt");

    let events = durable_events(&lifecycle_store, &record.run_id);
    let progress: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "runner.progress")
        .collect();
    assert_eq!(progress.len(), 1, "the bounded projection appends once");
    let progress = progress[0];
    assert_eq!(progress.data["phase"], "staged_execution_adopted");
    let transport = &progress.data["transport"];
    assert!(transport["omitted"].is_string());
    assert!(transport["bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 64 * 1024));
    assert_eq!(transport["sha256"].as_str().map(str::len), Some(64));
    assert_eq!(
        transport["fields"],
        json!(["phase", "staged_execution_envelope"])
    );
    assert!(serde_json::to_vec(&progress.data).expect("data").len() < 16 * 1024);
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
    assert!(events.iter().any(|event| event.kind == "runner.progress"
        && event.data["message"]
            .as_str()
            .is_some_and(|message| message.contains("provider started"))));
    assert!(!events.iter().any(|event| event.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("mutated progress"))));
    let persisted = lifecycle_store
        .read_record(&record.run_id)
        .expect("rolled back record");
    assert_eq!(
        persisted.metadata["runner_job_events"][0]["message"],
        "provider started"
    );
}

#[test]
fn submit_appends_queued_progress_once() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-submit-queued";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    lifecycle_store
        .write_record(&lifecycle_store.read_record(run_id).expect("record"))
        .expect("replayed submit write");
    let events = durable_events(&lifecycle_store, run_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["state"], "queued");
    assert_eq!(events[0].data["message"], "task submitted");
    assert_eq!(events[0].data["task_id"], "task-a");
    assert_eq!(
        events[0].task.as_ref().map(|task| task.as_str()),
        Some("task-a")
    );
}

#[test]
fn provider_success_and_enrichment_keep_stable_receipts() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-provider-success";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.metadata["provider_executions"][0]["owner_pid"] = json!(0);
        record.metadata["provider_executions"][0]["launch_context"] = json!({"ok": true});
    })
    .expect("enriched running execution");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.metadata["provider_executions"][0]["state"] = json!("succeeded");
        record.metadata["provider_executions"][0]["finished_at"] = json!("2026-07-24T00:01:00Z");
    })
    .expect("succeeded");
    let states: Vec<_> = durable_events(&lifecycle_store, run_id)
        .into_iter()
        .filter_map(|event| event.data["state"].as_str().map(str::to_string))
        .collect();
    assert_eq!(states, ["queued", "running", "succeeded"]);
    assert!(durable_events(&lifecycle_store, run_id)
        .iter()
        .filter(|event| event.kind == "task.state_changed")
        .all(|event| event.data["task_id"] == "task-a"
            && event.task.as_ref().map(|task| task.as_str()) == Some("task-a")));
}

#[test]
fn aggregate_progress_appends_once() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let aggregate = succeeded_aggregate(&plan);
    record_completed_run_in_store(
        &lifecycle_store,
        &plan,
        &aggregate,
        Some("durable-aggregate"),
    )
    .expect("completed");
    let events = durable_events(&lifecycle_store, "durable-aggregate");
    assert!(events
        .iter()
        .any(|event| event.data["state"] == "queued" && event.data["message"] == "task submitted"));
    assert!(events
        .iter()
        .any(|event| event.data["state"] == "succeeded" && event.data["message"] == "ok"));
}

#[test]
fn aggregate_repeated_same_state_messages_keep_distinct_receipts() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.events = vec![
        AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Running,
            attempt: 1,
            message: Some("first".to_string()),
        },
        AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Running,
            attempt: 1,
            message: Some("second".to_string()),
        },
        AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Succeeded,
            attempt: 1,
            message: Some("ok".to_string()),
        },
    ];
    record_completed_run_in_store(
        &lifecycle_store,
        &plan,
        &aggregate,
        Some("durable-aggregate-repeat"),
    )
    .expect("completed");
    let running: Vec<_> = durable_events(&lifecycle_store, "durable-aggregate-repeat")
        .into_iter()
        .filter(|event| event.data["state"] == "running")
        .collect();
    assert_eq!(running.len(), 2);
    assert_eq!(running[0].data["message"], "first");
    assert_eq!(running[1].data["message"], "second");
    assert_eq!(running[0].data["task_id"], "task-a");
    assert_ne!(running[0].sequence, running[1].sequence);
}

#[test]
fn imported_aggregate_does_not_invent_submit_for_foreign_tasks() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "durable-imported-tasks", |_| Ok(json!({})))
        .expect("submitted");
    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.outcomes[0].task_id = "task-b".to_string();
    aggregate.events = vec![AgentTaskProgressEvent {
        task_id: "task-b".to_string(),
        state: AgentTaskState::Succeeded,
        attempt: 1,
        message: Some("imported".to_string()),
    }];
    let mut record = lifecycle_store
        .read_record("durable-imported-tasks")
        .expect("record");
    lifecycle_store
        .write_aggregate_and_record(&record, &aggregate)
        .expect("imported aggregate");
    record = lifecycle_store
        .read_record("durable-imported-tasks")
        .expect("updated");
    lifecycle_store
        .write_record(&record)
        .expect("later record write");
    let events = durable_events(&lifecycle_store, "durable-imported-tasks");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.data["message"] == "task submitted")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .find(|event| event.data["message"] == "task submitted")
            .and_then(|event| event.data["task_id"].as_str()),
        Some("task-a")
    );
    assert!(events
        .iter()
        .any(|event| event.data["task_id"] == "task-b" && event.data["message"] == "imported"));
    assert!(!events.iter().any(
        |event| event.data["message"] == "task submitted" && event.data["task_id"] == "task-b"
    ));
}

#[test]
fn queued_identity_survives_submitted_at_revision() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-age-queued";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.submitted_at = "2020-01-01T00:00:00Z".to_string();
    })
    .expect("aged submitted_at");
    let events = durable_events(&lifecycle_store, run_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["message"], "task submitted");
}

#[test]
fn provider_state_survives_model_correction() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-provider-model";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.metadata["provider_executions"] = json!([{
            "task_id": "task-a",
            "attempt": 1,
            "state": "succeeded",
            "model": null
        }]);
    })
    .expect("terminal without model");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.metadata["provider_executions"][0]["model"] = json!("openai/gpt-5.6-terra");
    })
    .expect("model correction");
    let succeeded: Vec<_> = durable_events(&lifecycle_store, run_id)
        .into_iter()
        .filter(|event| event.data["state"] == "succeeded")
        .collect();
    assert_eq!(succeeded.len(), 1);
    assert_eq!(succeeded[0].data["message"], "provider execution succeeded");
}

#[test]
fn aggregate_replacement_replays_reordered_same_content() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let mut first = succeeded_aggregate(&plan);
    first.events = vec![
        AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Running,
            attempt: 1,
            message: Some("alpha".to_string()),
        },
        AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Running,
            attempt: 1,
            message: Some("beta".to_string()),
        },
    ];
    record_completed_run_in_store(&lifecycle_store, &plan, &first, Some("durable-reorder"))
        .expect("first aggregate");
    let mut reordered = first.clone();
    reordered.events.reverse();
    let record = lifecycle_store
        .read_record("durable-reorder")
        .expect("record");
    lifecycle_store
        .write_aggregate_and_record(&record, &reordered)
        .expect("reordered aggregate replay");
    let running: Vec<_> = durable_events(&lifecycle_store, "durable-reorder")
        .into_iter()
        .filter(|event| event.data["state"] == "running")
        .map(|event| event.data["message"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(running, ["alpha", "beta"]);
}

#[test]
fn cli_and_service_events_agree_on_the_canonical_ledger() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "durable-reader-agreement";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");

    let logs = logs_in_store(&lifecycle_store, run_id).expect("logs");
    let events =
        control_plane_events_in_store(&lifecycle_store, run_id, None).expect("service events");
    assert_eq!(logs.events, events.events);
    assert_eq!(logs.events, durable_events(&lifecycle_store, run_id));
    assert_eq!(
        logs.events
            .iter()
            .map(|event| event.data["state"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["queued", "running"]
    );
}

fn seed_historical_fixture(store: &AgentTaskLifecycleStore, run_id: &str) -> AgentTaskRunRecord {
    store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    let mut record = store.read_record(run_id).expect("record");
    record.metadata["provider_executions"] = json!([{
        "key": "task-a:1",
        "task_id": "task-a",
        "attempt": 1,
        "backend": "opencode",
        "state": "running",
        "started_at": "2026-07-24T00:00:00Z"
    }]);
    record.metadata["cook_operation_claims"] = json!([
        {
            "operation_key": "control-plane-action:cancel:receipted",
            "leased_at": "2026-07-24T00:00:01Z",
            "completed_at": "2026-07-24T00:00:02Z",
            "intent": { "action": "cancel" },
            "result": { "outcome": "succeeded" }
        },
        {
            "operation_key": "control-plane-action:cancel:unreceipted",
            "leased_at": "2026-07-24T00:00:03Z",
            "completed_at": "2026-07-24T00:00:04Z",
            "intent": { "action": "cancel" },
            "result": { "outcome": "succeeded" }
        }
    ]);
    persist_record_without_writer(store, &record);
    let record = store.read_record(run_id).expect("persisted record");
    append_ledger_event(
        store,
        &record,
        action_append_request(
            "control-plane-action:cancel:receipted",
            "action.accepted",
            json!({
                "operation_key": "control-plane-action:cancel:receipted",
                "acknowledgement": "receipted"
            }),
        ),
    );
    append_ledger_event(
        store,
        &record,
        action_append_request(
            "control-plane-action:cancel:receipted",
            "action.succeeded",
            json!({
                "outcome": "succeeded",
                "acknowledgement": "receipted"
            }),
        ),
    );
    append_ledger_event(
        store,
        &record,
        ControlPlaneEventAppendRequest {
            schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
            idempotency_key: "external-progress-1".to_string(),
            actor: "broker:test".to_string(),
            kind: "run.progress".to_string(),
            source: ControlPlaneEventSource {
                component: "external".to_string(),
                instance: None,
            },
            occurred_at: Some("2026-07-24T00:00:05Z".to_string()),
            task: None,
            attempt: None,
            execution: None,
            data: json!({ "message": "external note" }),
            artifacts: Vec::new(),
            evidence: Vec::new(),
        },
    );
    record
}

#[test]
fn owning_write_migrates_historical_progress_twice_without_read_writes() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "owning-write-migration";
    seed_historical_fixture(&lifecycle_store, run_id);

    let before = durable_events(&lifecycle_store, run_id);
    let logs = logs_in_store(&lifecycle_store, run_id).expect("pre-migration logs");
    let service = control_plane_events_in_store(&lifecycle_store, run_id, None)
        .expect("pre-migration service");
    assert_eq!(logs.events, service.events);
    assert_eq!(logs.events, before);
    assert!(
        !before.iter().any(|event| event.data["state"] == "running"),
        "reads must not migrate provider progress"
    );
    assert_eq!(
        durable_events(&lifecycle_store, run_id).len(),
        before.len(),
        "reads must not mint ledger events"
    );

    lifecycle_store
        .write_record(&lifecycle_store.read_record(run_id).expect("record"))
        .expect("first owning migration");
    let first = durable_events(&lifecycle_store, run_id);
    lifecycle_store
        .write_record(&lifecycle_store.read_record(run_id).expect("record"))
        .expect("second owning migration");
    let second = durable_events(&lifecycle_store, run_id);
    assert_eq!(first, second, "migration is idempotent");
    let logs = logs_in_store(&lifecycle_store, run_id).expect("migrated logs");
    let service =
        control_plane_events_in_store(&lifecycle_store, run_id, None).expect("migrated service");
    assert_eq!(logs.events, service.events);
    assert_eq!(logs.events, first);
    assert!(first.iter().any(|event| event.data["state"] == "queued"));
    assert!(first.iter().any(|event| event.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running"))));
    assert!(first
        .iter()
        .any(|event| event.kind == "run.progress" && event.data["message"] == "external note"));
    assert!(first.iter().any(|event| {
        event.kind == "action.accepted" && event.data.get("acknowledgement").is_none()
    }));
    assert!(first
        .iter()
        .any(|event| event.kind == "action.succeeded"
            && event.data["acknowledgement"] == "receipted"));
}

#[test]
fn obsolete_event_cursors_expire_with_reset_guidance() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "obsolete-cursor";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    let numeric = homeboy_control_plane_contract::EventCursor::new("1").expect("numeric");
    let error = control_plane_events_in_store(&lifecycle_store, run_id, Some(&numeric))
        .expect_err("numeric cursor");
    assert_eq!(error.class, ControlPlaneErrorClass::CursorExpired);
    assert!(error.message.contains("retry without a cursor to reset"));
    let v1_bytes = serde_json::to_vec(&json!({
        "schema": "homeboy/control-plane-event-cursor/v1",
        "run_id": run_id,
        "sequence": 1,
    }))
    .expect("v1 payload");
    let v1 = homeboy_control_plane_contract::EventCursor::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v1_bytes),
    )
    .expect("v1 cursor");
    let error =
        control_plane_events_in_store(&lifecycle_store, run_id, Some(&v1)).expect_err("v1 cursor");
    assert_eq!(error.class, ControlPlaneErrorClass::CursorExpired);
    assert!(error.message.contains("retry without a cursor to reset"));
}

#[test]
fn canonical_already_satisfied_does_not_synthesize_a_claim_succeeded() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "canonical-action-no-duplicate-terminal";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    let mut record = lifecycle_store.read_record(run_id).expect("record");
    append_ledger_event(
        &lifecycle_store,
        &record,
        action_append_request(
            "control-plane-action:reconcile:canonical",
            "action.accepted",
            json!({
                "acknowledgement": "canonical",
                "operation_digest": homeboy_engine_primitives::content_hash::sha256_hex(
                    b"control-plane-action:reconcile:canonical"
                )
            }),
        ),
    );
    append_ledger_event(
        &lifecycle_store,
        &record,
        action_append_request(
            "control-plane-action:reconcile:canonical",
            "action.already_satisfied",
            json!({
                "acknowledgement": "canonical",
                "outcome": "already_satisfied",
                "operation_digest": homeboy_engine_primitives::content_hash::sha256_hex(
                    b"control-plane-action:reconcile:canonical"
                )
            }),
        ),
    );
    record.metadata["cook_operation_claims"] = json!([
        {
            "operation_key": "control-plane-action:reconcile:canonical",
            "leased_at": "2026-07-24T00:00:01Z",
            "completed_at": "2026-07-24T00:00:02Z",
            "intent": { "action": "reconcile" },
            "result": { "outcome": "succeeded" }
        },
        {
            "operation_key": "control-plane-action:cancel:legacy",
            "leased_at": "2026-07-24T00:00:03Z",
            "completed_at": "2026-07-24T00:00:04Z",
            "intent": { "action": "cancel" },
            "result": { "outcome": "succeeded" }
        }
    ]);
    lifecycle_store
        .write_record(&record)
        .expect("backfill mixed claims");
    let actions: Vec<_> = durable_events(&lifecycle_store, run_id)
        .into_iter()
        .filter(|event| event.kind.starts_with("action."))
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        actions
            .iter()
            .filter(|kind| *kind == "action.already_satisfied")
            .count(),
        1
    );
    assert_eq!(
        actions
            .iter()
            .filter(|kind| *kind == "action.succeeded")
            .count(),
        1,
        "legacy unreceipted claim may backfill succeeded; canonical already_satisfied must not"
    );
    assert_eq!(
        actions
            .iter()
            .filter(|kind| *kind == "action.accepted")
            .count(),
        2
    );
    assert!(!actions.iter().any(|kind| kind == "action.failed"));
}

#[test]
fn interrupted_canonical_accepted_does_not_synthesize_a_terminal() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "interrupted-canonical-accepted";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    let mut record = lifecycle_store.read_record(run_id).expect("record");
    append_ledger_event(
        &lifecycle_store,
        &record,
        action_append_request(
            "control-plane-action:reconcile:interrupted",
            "action.accepted",
            json!({
                "acknowledgement": "in-flight",
                "operation_digest": homeboy_engine_primitives::content_hash::sha256_hex(
                    b"control-plane-action:reconcile:interrupted"
                )
            }),
        ),
    );
    record.metadata["cook_operation_claims"] = json!([{
        "operation_key": "control-plane-action:reconcile:interrupted",
        "leased_at": "2026-07-24T00:00:01Z",
        "completed_at": "2026-07-24T00:00:02Z",
        "intent": { "action": "reconcile" },
        "result": { "outcome": "succeeded" }
    }]);
    lifecycle_store
        .write_record(&record)
        .expect("recover interrupted action");
    let actions: Vec<_> = durable_events(&lifecycle_store, run_id)
        .into_iter()
        .filter(|event| event.kind.starts_with("action."))
        .map(|event| event.kind)
        .collect();
    assert_eq!(actions, ["action.accepted"]);
}
