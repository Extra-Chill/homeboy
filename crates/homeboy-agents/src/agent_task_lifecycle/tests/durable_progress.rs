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
use homeboy_core::api_jobs::Job;

fn persist_unmarked_record(store: &AgentTaskLifecycleStore, record: &AgentTaskRunRecord) {
    let mut record = record.clone();
    if let Some(metadata) = record.metadata.as_object_mut() {
        metadata.remove(super::super::durable_progress::DURABLE_EVENT_HISTORY_METADATA_KEY);
    }
    store
        .open_observation_initialized()
        .expect("observation")
        .upsert_imported_run(&homeboy_core::observation::RunRecord {
            id: record.run_id.clone(),
            kind: "agent-task".to_string(),
            component_id: Some(record.plan_id.clone()),
            started_at: record.submitted_at.clone(),
            finished_at: record.lifecycle.execution.finished_at.clone(),
            status: if record.state.is_terminal() {
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
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.events[0].kind, "task.state_changed");
    assert_eq!(resumed.events[0].data["state"], "cancelled");
    assert_eq!(resumed.events[0].sequence, 3);

    let replayed = service
        .events(&run, cursor.as_ref())
        .expect("cursor replay");
    assert_eq!(replayed.events.len(), 1);
    assert_eq!(replayed.events[0].sequence, 3);
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
    persist_unmarked_record(store, &record);
    let record = store.read_record(run_id).expect("unmarked record");
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

struct ProviderMustNotRun;

impl RunnerContinuationProvider for ProviderMustNotRun {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
        panic!("history migration must not invoke the runner provider");
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        panic!("history migration must not invoke the runner provider");
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> Result<i32> {
        panic!("history migration must not invoke the runner provider");
    }

    fn submit_runner_api_request(
        &self,
        _runner_id: &str,
        _submission: RunnerContinuationSubmission,
    ) -> Result<Job> {
        panic!("history migration must not invoke the runner provider");
    }
}

fn seed_historical_terminal_fixture(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> AgentTaskRunRecord {
    let mut record = seed_historical_fixture(store, run_id);
    set_run_state(&mut record, AgentTaskRunState::Succeeded);
    record.artifact_refs = vec![AgentTaskArtifactRef {
        task_id: "task-a".to_string(),
        kind: "patch".to_string(),
        uri: "file:///tmp/historical.patch".to_string(),
        role: Some("review".to_string()),
        label: Some("historical patch".to_string()),
        semantic_key: None,
        size_bytes: Some(32),
    }];
    persist_unmarked_record(store, &record);
    store
        .read_record(run_id)
        .expect("terminal historical record")
}

#[test]
fn explicit_history_migration_backfills_terminal_run_without_provider_or_state_change() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let _provider = RunnerContinuationTestGuard::install(Box::new(ProviderMustNotRun));
    let run_id = "explicit-history-migration";
    let before = seed_historical_terminal_fixture(&lifecycle_store, run_id);
    assert_eq!(before.state, AgentTaskRunState::Succeeded);
    assert!(!super::super::durable_progress::has_durable_event_history(
        &before
    ));

    let pre_logs = logs_in_store(&lifecycle_store, run_id).expect("pre-migration logs");
    let pre_events = durable_events(&lifecycle_store, run_id);
    assert_eq!(pre_logs.events, pre_events);
    assert!(
        !pre_events
            .iter()
            .any(|event| event.data["state"] == "running"),
        "reads must not migrate provider progress"
    );
    assert_eq!(
        durable_events(&lifecycle_store, run_id).len(),
        pre_events.len(),
        "reads must not mint ledger events"
    );

    let first = migrate_durable_event_history_in_store(&lifecycle_store, run_id)
        .expect("first explicit migration");
    assert_eq!(first.state, AgentTaskRunState::Succeeded);
    assert_eq!(
        first.durable_event_history,
        crate::orchestration::EVENT_STREAM_DURABLE_PROGRESS
    );
    let after = lifecycle_store
        .read_record(run_id)
        .expect("migrated record");
    assert_eq!(after.state, before.state);
    assert_eq!(after.workspace_owner_lease, before.workspace_owner_lease);
    assert_eq!(after.workspace_claim, before.workspace_claim);
    assert_eq!(after.artifact_refs, before.artifact_refs);
    assert_eq!(
        after.metadata["provider_executions"],
        before.metadata["provider_executions"]
    );
    assert_eq!(
        after.metadata["cook_operation_claims"],
        before.metadata["cook_operation_claims"]
    );
    assert!(super::super::durable_progress::has_durable_event_history(
        &after
    ));

    let first_events = durable_events(&lifecycle_store, run_id);
    let logs = logs_in_store(&lifecycle_store, run_id).expect("migrated logs");
    assert_eq!(logs.events, first_events);
    assert!(first_events
        .iter()
        .any(|event| event.data["state"] == "queued"));
    assert!(first_events.iter().any(|event| event.data["message"]
        .as_str()
        .is_some_and(|message| message.contains("provider execution running"))));
    assert!(first_events
        .iter()
        .any(|event| event.kind == "run.progress" && event.data["message"] == "external note"));
    assert!(first_events.iter().any(|event| {
        event.kind == "action.accepted" && event.data.get("acknowledgement").is_none()
    }));
    assert!(first_events
        .iter()
        .any(|event| event.kind == "action.succeeded"
            && event.data["acknowledgement"] == "receipted"));
    assert!(
        !first_events
            .iter()
            .any(|event| event.data.get("backend").is_some()
                || event.data.get("started_at").is_some()),
        "event payloads do not recover provider backend/started_at strings"
    );
    assert_eq!(
        after.metadata["provider_executions"][0]["backend"],
        "opencode"
    );
    assert_eq!(
        after.metadata["provider_executions"][0]["started_at"],
        "2026-07-24T00:00:00Z"
    );
    let artifacts =
        artifacts_in_store(&lifecycle_store, run_id).expect("artifacts remain readable");
    assert_eq!(artifacts.run_id, run_id);
    assert_eq!(after.artifact_refs[0].uri, "file:///tmp/historical.patch");

    let second = migrate_durable_event_history_in_store(&lifecycle_store, run_id)
        .expect("second explicit migration");
    assert_eq!(second, first);
    assert_eq!(durable_events(&lifecycle_store, run_id), first_events);
    let logs = logs_in_store(&lifecycle_store, run_id).expect("replay logs");
    assert_eq!(logs.events, first_events);
}
