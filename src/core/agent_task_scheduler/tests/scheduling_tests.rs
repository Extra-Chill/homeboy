//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::super::*;
use super::fixtures::*;
use crate::core::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskMatrixAggregate, AgentTaskMatrixAxis,
    AGENT_TASK_ARTIFACT_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[test]
fn cancellation_token_callbacks_fire_once_and_after_existing_cancel() {
    let token = AgentTaskCancellationToken::default();
    let callback_count = Arc::new(AtomicUsize::new(0));
    let callback_count_for_token = Arc::clone(&callback_count);
    token.on_cancel(Arc::new(move || {
        callback_count_for_token.fetch_add(1, Ordering::SeqCst);
    }));

    token.cancel();
    token.cancel();

    assert_eq!(callback_count.load(Ordering::SeqCst), 1);

    let immediate_count = Arc::new(AtomicUsize::new(0));
    let immediate_count_for_token = Arc::clone(&immediate_count);
    token.on_cancel(Arc::new(move || {
        immediate_count_for_token.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(immediate_count.load(Ordering::SeqCst), 1);
}

#[test]
fn schedules_tasks_with_bounded_concurrency_and_success_aggregate() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(4);
    plan.options.max_concurrency = 2;

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.queued, 0);
    assert_eq!(aggregate.totals.succeeded, 4);
    assert!(max_seen.load(Ordering::SeqCst) <= 2);
    assert!(aggregate
        .events
        .iter()
        .any(|event| event.state == AgentTaskState::Running));
}

#[test]
fn preserves_partial_failure_evidence() {
    let mut statuses = HashMap::new();
    statuses.insert("task-2".to_string(), AgentTaskOutcomeStatus::Failed);
    let scheduler =
        AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
    let mut plan = plan_with_tasks(3);
    plan.options.max_concurrency = 3;

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
    assert_eq!(aggregate.totals.queued, 0);
    assert_eq!(aggregate.totals.succeeded, 2);
    assert_eq!(aggregate.totals.failed, 1);
    let failed = aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == "task-2")
        .expect("failed task outcome");
    assert_eq!(failed.evidence_refs[0].kind, "log");
}

#[test]
fn failed_single_task_is_not_also_counted_as_queued() {
    let mut statuses = HashMap::new();
    statuses.insert("task-1".to_string(), AgentTaskOutcomeStatus::Failed);
    let scheduler =
        AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));

    let aggregate = scheduler.run(plan_with_tasks(1));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(aggregate.totals.queued, 0);
    assert_eq!(aggregate.queue.queued, 0);
}

#[test]
fn normalizes_slow_task_to_timeout() {
    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(25),
    ));
    let mut plan = plan_with_tasks(1);
    plan.tasks[0].limits.timeout_ms = Some(1);

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.timed_out, 1);
    assert_eq!(
        aggregate.outcomes[0].status,
        AgentTaskOutcomeStatus::Timeout
    );
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::Timeout)
    );
}

#[test]
fn timeout_with_completed_runtime_artifacts_is_discoverable_and_promotable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let artifact_root = temp.path().join("task-1-artifacts");
    fs::create_dir_all(&artifact_root).expect("artifact root");
    let patch_path = artifact_root.join("fix.patch");
    fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
    fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
    let agent_result_path = artifact_root.join("agent-result.json");
    fs::write(
        &agent_result_path,
        serde_json::to_string(&AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("patch ready".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "fix".to_string(),
                kind: "patch".to_string(),
                name: Some("fix.patch".to_string()),
                path: Some(patch_path.display().to_string()),
                url: None,
                mime: Some("text/x-patch".to_string()),
                size_bytes: None,
                sha256: None,
                metadata: json!({ "role": "patch" }),
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "runtime_bundle".to_string(),
                uri: artifact_root.display().to_string(),
                label: Some("runtime bundle".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: json!({}),
        })
        .expect("agent result json"),
    )
    .expect("agent result");

    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(250),
    ));
    let mut plan = plan_with_tasks(1);
    plan.tasks[0].limits.timeout_ms = Some(1);
    plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(aggregate.totals.timed_out, 0);
    assert!(aggregate
        .events
        .iter()
        .any(|event| event.task_id == "task-1" && event.state == AgentTaskState::Succeeded));
    let outcome = &aggregate.outcomes[0];
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome.artifacts.iter().any(|artifact| {
        artifact.kind == "patch" && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
    }));
    assert!(outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "transcript"));
    assert!(outcome
        .evidence_refs
        .iter()
        .any(|evidence| evidence.kind == "agent_result"
            && evidence.uri == agent_result_path.display().to_string()));
    assert!(outcome.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "completed_runtime_late_provider_race"
            && diagnostic.data.get("timeout_kind").and_then(Value::as_str)
                == Some("scheduler_timeout")
            && diagnostic
                .data
                .get("actionable_patch")
                .and_then(Value::as_bool)
                == Some(true)
    }));
}

#[test]
fn runtime_bundle_artifacts_materialize_required_typed_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let bundle = temp.path().join("runtime-123");
    let files = bundle.join("files");
    fs::create_dir_all(&files).expect("runtime files");
    let patch_path = files.join("patch.diff");
    let transcript_path = files.join("transcript.json");
    fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
    fs::write(&transcript_path, "{\"events\":[]}").expect("transcript");

    let scheduler = AgentTaskScheduler::new(RuntimeBundleOutcomeExecutor {
        patch_path: patch_path.clone(),
        transcript_path: transcript_path.clone(),
    });
    let mut plan = plan_with_tasks(1);
    plan.tasks[0].expected_artifacts = vec![
        "patch".to_string(),
        "agent_result".to_string(),
        "transcript".to_string(),
    ];

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 1);
    let outcome = &aggregate.outcomes[0];
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    for name in ["patch", "agent_result", "transcript"] {
        assert!(
            outcome
                .typed_artifacts
                .iter()
                .any(|artifact| artifact.name == name),
            "missing typed artifact {name}"
        );
    }
    let patch = outcome
        .typed_artifacts
        .iter()
        .find(|artifact| artifact.name == "patch")
        .expect("patch typed artifact");
    assert_eq!(
        patch
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.path.as_deref()),
        Some(patch_path.to_str().expect("patch path"))
    );
    assert!(outcome.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "agent_task.required_typed_artifacts_normalized"
    }));
}

#[test]
fn timeout_with_empty_patch_artifacts_and_actionable_false_stays_timed_out() {
    let temp = tempfile::tempdir().expect("tempdir");
    let artifact_root = temp.path().join("task-1-artifacts");
    fs::create_dir_all(&artifact_root).expect("artifact root");
    let patch_path = artifact_root.join("patch.diff");
    let mounted_patch_path = artifact_root.join("mount-5.patch");
    fs::write(&patch_path, "").expect("patch diff");
    fs::write(&mounted_patch_path, "").expect("mounted patch");
    fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
    fs::write(
        artifact_root.join("agent-result.json"),
        serde_json::to_string(&json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-1",
            "status": "succeeded",
            "summary": "runtime produced no actionable patch",
            "actionable": false,
            "artifacts": [
                {
                    "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                    "id": "patch",
                    "kind": "patch",
                    "name": "patch.diff",
                    "path": patch_path.display().to_string(),
                    "mime": "text/x-diff",
                    "metadata": { "role": "patch" }
                },
                {
                    "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                    "id": "mount-5",
                    "kind": "patch",
                    "name": "mount-5.patch",
                    "path": mounted_patch_path.display().to_string(),
                    "mime": "text/x-patch",
                    "metadata": { "role": "patch" }
                }
            ],
            "evidence_refs": [],
            "diagnostics": [],
            "metadata": {}
        }))
        .expect("agent result json"),
    )
    .expect("agent result");

    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(250),
    ));
    let mut plan = plan_with_tasks(1);
    plan.tasks[0].limits.timeout_ms = Some(1);
    plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.succeeded, 0);
    assert_eq!(aggregate.totals.timed_out, 1);
    assert!(aggregate
        .events
        .iter()
        .any(|event| event.task_id == "task-1" && event.state == AgentTaskState::TimedOut));
    let outcome = &aggregate.outcomes[0];
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Timeout)
    );
    assert!(outcome.artifacts.iter().any(|artifact| {
        artifact.kind == "patch" && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
    }));
    assert!(outcome.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "completed_runtime_late_provider_race"
            && diagnostic
                .data
                .get("actionable_patch")
                .and_then(Value::as_bool)
                == Some(false)
    }));
}

#[test]
fn retries_failed_tasks_until_success() {
    let executor = RetryOnceExecutor::default();
    let attempts = Arc::clone(&executor.attempts);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(1);
    plan.options.retry.max_attempts = 2;

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(aggregate.events.iter().any(|event| {
        event.task_id == "task-1" && event.state == AgentTaskState::Queued && event.attempt == 2
    }));
}

#[test]
fn blocks_tasks_over_queue_depth_and_reports_backpressure() {
    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(0),
    ));
    let mut plan = plan_with_tasks(3);
    plan.options.max_queue_depth = Some(2);

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
    assert_eq!(aggregate.totals.succeeded, 2);
    assert_eq!(aggregate.totals.blocked, 1);
    assert_eq!(aggregate.queue.blocked, 1);
    assert_eq!(aggregate.queue.max_queue_depth, Some(2));
    assert!(aggregate
        .queue
        .backpressure
        .iter()
        .any(|status| status.kind == "queue_depth"));
    assert!(aggregate
        .events
        .iter()
        .any(|event| { event.task_id == "task-3" && event.state == AgentTaskState::Blocked }));
}

#[test]
fn applies_per_executor_concurrency_below_global_limit() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(3);
    plan.options.max_concurrency = 3;
    plan.options
        .per_executor_concurrency
        .insert("test".to_string(), 1);

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert!(max_seen.load(Ordering::SeqCst) <= 1);
    assert_eq!(
        aggregate.queue.per_executor_concurrency.get("test"),
        Some(&1)
    );
}

#[test]
fn resource_budget_limits_concurrent_task_cost() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(4);
    plan.options.max_concurrency = 4;
    plan.options.resource_budget.max_active_units = Some(2);
    plan.options.resource_budget.default_task_units = 1;

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 4);
    assert!(max_seen.load(Ordering::SeqCst) <= 2);
    assert_eq!(aggregate.queue.resource_budget.max_active_units, Some(2));
    assert_eq!(aggregate.queue.resource_budget.default_task_units, 1);
}

#[test]
fn resource_budget_blocks_task_that_cannot_fit() {
    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(0),
    ));
    let mut plan = plan_with_tasks(1);
    plan.options.resource_budget.max_active_units = Some(2);
    plan.options.resource_budget.default_task_units = 3;

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.blocked, 1);
    assert_eq!(aggregate.queue.blocked, 1);
    assert!(aggregate
        .queue
        .backpressure
        .iter()
        .any(|status| status.kind == "resource_budget"));
}

#[test]
fn adaptive_concurrency_scales_up_when_runner_slots_are_available() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(4);
    plan.options.max_concurrency = 1;
    plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
        max_concurrency: Some(3),
        runner_capacity: Some(3),
        ..AgentTaskAdaptiveConcurrencyPolicy::default()
    });

    let aggregate = scheduler.run(plan);
    let adaptive = aggregate
        .queue
        .adaptive_concurrency
        .expect("adaptive status");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert!(max_seen.load(Ordering::SeqCst) > 1);
    assert!(max_seen.load(Ordering::SeqCst) <= 3);
    assert_eq!(adaptive.configured_max_concurrency, 1);
    assert_eq!(adaptive.max_concurrency, 3);
    assert!(adaptive.decisions.iter().any(|decision| {
        decision.action == AgentTaskAdaptiveConcurrencyAction::Increased
            && decision.effective_concurrency == 3
            && decision.reason.contains("runner slots are available")
    }));
}

#[test]
fn adaptive_concurrency_scales_down_under_runner_pressure() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(3);
    plan.options.max_concurrency = 4;
    plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
        max_concurrency: Some(4),
        runner_capacity: Some(3),
        active_leases: 2,
        ..AgentTaskAdaptiveConcurrencyPolicy::default()
    });

    let aggregate = scheduler.run(plan);
    let adaptive = aggregate
        .queue
        .adaptive_concurrency
        .expect("adaptive status");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert!(max_seen.load(Ordering::SeqCst) <= 1);
    assert_eq!(adaptive.effective_concurrency, 1);
    assert!(adaptive.decisions.iter().any(|decision| {
        decision.action == AgentTaskAdaptiveConcurrencyAction::Decreased
            && decision.reason.contains("available runner slots 1")
    }));
}

#[test]
fn adaptive_concurrency_pauses_and_blocks_when_runner_capacity_is_unavailable() {
    let executor = RecordingExecutor {
        statuses: HashMap::new(),
        delay: Duration::from_millis(0),
        running: Arc::new(AtomicUsize::new(0)),
        max_seen: Arc::new(AtomicUsize::new(0)),
        cancel_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(2);
    plan.options.max_concurrency = 2;
    plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
        runner_capacity: Some(1),
        active_leases: 1,
        ..AgentTaskAdaptiveConcurrencyPolicy::default()
    });

    let aggregate = scheduler.run(plan);
    let adaptive = aggregate
        .queue
        .adaptive_concurrency
        .expect("adaptive status");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.blocked, 2);
    assert_eq!(max_seen.load(Ordering::SeqCst), 0);
    assert_eq!(adaptive.effective_concurrency, 0);
    assert!(adaptive.decisions.iter().any(|decision| {
        decision.action == AgentTaskAdaptiveConcurrencyAction::Paused
            && decision.reason.contains("consume runner_capacity=1")
    }));
    assert!(aggregate
        .queue
        .backpressure
        .iter()
        .any(|status| status.kind == "adaptive_concurrency"));
}

#[test]
fn adaptive_concurrency_status_records_held_decision() {
    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(0),
    ));
    let mut plan = plan_with_tasks(1);
    plan.options.max_concurrency = 2;
    plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy::default());

    let aggregate = scheduler.run(plan);
    let adaptive = aggregate
        .queue
        .adaptive_concurrency
        .expect("adaptive status");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(adaptive.effective_concurrency, 2);
    assert!(adaptive.decisions.iter().any(|decision| {
        decision.action == AgentTaskAdaptiveConcurrencyAction::Held
            && decision.reason.contains("configured ceiling")
    }));
}

#[test]
fn applies_per_model_concurrency_below_global_limit() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
    let max_seen = Arc::clone(&executor.max_seen);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(3);
    for task in &mut plan.tasks {
        task.executor.model = Some("model-a".to_string());
    }
    plan.options.max_concurrency = 3;
    plan.options
        .per_model_concurrency
        .insert("test:model-a".to_string(), 1);

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert!(max_seen.load(Ordering::SeqCst) <= 1);
    assert_eq!(
        aggregate.queue.per_model_concurrency.get("test:model-a"),
        Some(&1)
    );
}

#[test]
fn runs_matrix_cells_through_generic_scheduler_and_preserves_axes() {
    let mut statuses = HashMap::new();
    statuses.insert(
        "fanout/site-smoke[model=gpt-5.5,prompt=site-b]".to_string(),
        AgentTaskOutcomeStatus::Failed,
    );
    let scheduler =
        AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
    let matrix_plan = expand_agent_task_matrix(
        "fanout/site-smoke",
        vec![
            AgentTaskMatrixAxis {
                name: "model".to_string(),
                values: vec!["gpt-5.5".to_string(), "claude".to_string()],
            },
            AgentTaskMatrixAxis {
                name: "prompt".to_string(),
                values: vec!["site-a".to_string(), "site-b".to_string()],
            },
        ],
        request("template"),
    )
    .expect("matrix expands");
    let mut schedule_plan = AgentTaskPlan::new(
        matrix_plan.plan_id.clone(),
        matrix_plan
            .cells
            .iter()
            .map(|cell| cell.task.clone())
            .collect(),
    );
    schedule_plan.options.max_concurrency = 2;

    let schedule = scheduler.run(schedule_plan);
    let matrix = AgentTaskMatrixAggregate::from_outcomes(&matrix_plan, &schedule.outcomes);

    assert_eq!(schedule.plan_id, "fanout/site-smoke");
    assert_eq!(schedule.totals.succeeded, 3);
    assert_eq!(schedule.totals.failed, 1);
    assert_eq!(matrix.cells.len(), 4);
    assert!(!matrix.passed);
    let failed = matrix
        .cells
        .iter()
        .find(|cell| cell.status == Some(AgentTaskOutcomeStatus::Failed))
        .expect("failed matrix cell");
    assert_eq!(failed.axes["model"], "gpt-5.5");
    assert_eq!(failed.axes["prompt"], "site-b");
    assert_eq!(failed.evidence_refs[0].kind, "log");
}

#[test]
fn retry_budget_and_failure_classifications_gate_retries() {
    let executor = RetryOnceExecutor::default();
    let attempts = Arc::clone(&executor.attempts);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(1);
    plan.options.retry.max_attempts = 3;
    plan.options.retry.max_retries_total = Some(0);
    plan.options.retry.retryable_failure_classifications =
        vec![AgentTaskFailureClassification::Provider];

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(aggregate.queue.retry_budget_remaining, Some(0));
}

#[test]
fn templates_prior_output_into_downstream_task_request() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: true,
    });
    let mut plan = AgentTaskPlan::new("plan-output-dag", vec![request("idea"), request("design")]);
    plan.options.max_concurrency = 2;
    plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
    plan.tasks[1].executor.config = json!({
        "github_issue": "{{outputs.issue_number}}",
        "instructions": "Use issue {{ outputs.issue_number }}"
    });
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: Vec::new(),
            bindings: HashMap::from([(
                "issue_number".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: "/outputs/issue_number".to_string(),
                    artifact: None,
                    required: true,
                    default: Value::Null,
                },
            )]),
        },
    );

    let aggregate = scheduler.run(plan);
    let observed = observed.lock().expect("observed requests");
    let design = observed
        .iter()
        .find(|request| request.task_id == "design")
        .expect("design request dispatched");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 2);
    assert_eq!(design.instructions, "Build design for issue #3447");
    assert_eq!(design.executor.config["github_issue"], json!(3447));
    assert_eq!(design.executor.config["instructions"], "Use issue 3447");
    assert_eq!(design.metadata["generated_from_outputs"], json!(true));
    assert_eq!(
        design.metadata["resolved_output_bindings"]["issue_number"],
        json!(3447)
    );
    let idea_succeeded_index = aggregate
        .events
        .iter()
        .position(|event| event.task_id == "idea" && event.state == AgentTaskState::Succeeded)
        .expect("idea succeeded event");
    let design_running_index = aggregate
        .events
        .iter()
        .position(|event| event.task_id == "design" && event.state == AgentTaskState::Running)
        .expect("design running event");
    assert!(idea_succeeded_index < design_running_index);
}

#[test]
fn binds_typed_artifact_payload_into_downstream_task_request() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: true,
    });
    let mut plan = AgentTaskPlan::new(
        "plan-artifact-dag",
        vec![request("idea"), request("design")],
    );
    plan.options.max_concurrency = 2;
    plan.tasks[1].inputs = json!({ "packet": "{{outputs.concept_packet}}" });
    plan.artifact_outputs.insert(
        "idea".to_string(),
        vec![AgentTaskArtifactOutputDeclaration {
            name: "concept_packet".to_string(),
            kind: "concept_packet".to_string(),
            schema: Some("example/concept-packet/v1".to_string()),
            artifact_id: None,
            payload_path: Some("/title".to_string()),
        }],
    );
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: Vec::new(),
            bindings: HashMap::from([(
                "concept_packet".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: String::new(),
                    artifact: Some(AgentTaskArtifactBinding {
                        kind: "concept_packet".to_string(),
                        schema: Some("example/concept-packet/v1".to_string()),
                        artifact_id: Some("concept".to_string()),
                        payload_path: Some("/title".to_string()),
                    }),
                    required: true,
                    default: Value::Null,
                },
            )]),
        },
    );

    let aggregate = scheduler.run(plan);
    let observed = observed.lock().expect("observed requests");
    let design = observed
        .iter()
        .find(|request| request.task_id == "design")
        .expect("design request dispatched");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(design.inputs["packet"], json!("Demo concept"));
    assert_eq!(aggregate.artifact_lineage.len(), 1);
    assert_eq!(aggregate.artifact_lineage[0].name, "concept_packet");
    assert_eq!(aggregate.artifact_lineage[0].payload, json!("Demo concept"));
}

#[test]
fn skips_required_typed_artifact_binding_when_artifact_is_missing() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: false,
    });
    let mut plan = AgentTaskPlan::new(
        "plan-artifact-skip",
        vec![request("idea"), request("design")],
    );
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: Vec::new(),
            bindings: HashMap::from([(
                "finding_packet".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: String::new(),
                    artifact: Some(AgentTaskArtifactBinding {
                        kind: "finding_packet".to_string(),
                        schema: None,
                        artifact_id: None,
                        payload_path: None,
                    }),
                    required: true,
                    default: Value::Null,
                },
            )]),
        },
    );

    let aggregate = scheduler.run(plan);
    let observed = observed.lock().expect("observed requests");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
    assert!(observed.iter().all(|request| request.task_id != "design"));
    let skipped = aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == "design")
        .expect("skipped outcome");
    assert!(skipped.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "output_dependency_missing"
            && diagnostic.message.contains("required artifact binding")
    }));
}

#[test]
fn optional_typed_artifact_binding_uses_default() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: false,
    });
    let mut plan = AgentTaskPlan::new(
        "plan-artifact-default",
        vec![request("idea"), request("design")],
    );
    plan.tasks[1].inputs = json!({ "packet": "{{outputs.finding_packet}}" });
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: Vec::new(),
            bindings: HashMap::from([(
                "finding_packet".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: String::new(),
                    artifact: Some(AgentTaskArtifactBinding {
                        kind: "finding_packet".to_string(),
                        schema: None,
                        artifact_id: None,
                        payload_path: None,
                    }),
                    required: false,
                    default: json!({ "findings": [] }),
                },
            )]),
        },
    );

    let aggregate = scheduler.run(plan);
    let observed = observed.lock().expect("observed requests");
    let design = observed
        .iter()
        .find(|request| request.task_id == "design")
        .expect("design request dispatched");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(design.inputs["packet"], json!({ "findings": [] }));
}

#[test]
fn skips_downstream_task_when_required_output_is_missing() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: false,
    });
    let mut plan = AgentTaskPlan::new("plan-output-skip", vec![request("idea"), request("design")]);
    plan.options.max_concurrency = 2;
    plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: Vec::new(),
            bindings: HashMap::from([(
                "issue_number".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: "/outputs/issue_number".to_string(),
                    artifact: None,
                    required: true,
                    default: Value::Null,
                },
            )]),
        },
    );

    let aggregate = scheduler.run(plan);
    let observed = observed.lock().expect("observed requests");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(aggregate.totals.skipped, 1);
    assert_eq!(aggregate.totals.failed, 0);
    assert!(observed.iter().all(|request| request.task_id != "design"));
    assert!(aggregate
        .events
        .iter()
        .any(|event| { event.task_id == "design" && event.state == AgentTaskState::Skipped }));
    let skipped = aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == "design")
        .expect("skipped outcome");
    assert_eq!(skipped.status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        skipped.failure_classification,
        Some(AgentTaskFailureClassification::InvalidInput)
    );
    assert!(skipped.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "output_dependency_missing"
            && diagnostic.message.contains("required output binding")
    }));
}

#[test]
fn static_batch_plans_remain_compatible_without_output_dependencies() {
    let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
        HashMap::new(),
        Duration::from_millis(0),
    ));
    let plan_json = serde_json::to_string(&plan_with_tasks(2)).expect("plan json");
    let plan: AgentTaskPlan = serde_json::from_str(&plan_json).expect("static plan decodes");

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 2);
    assert_eq!(aggregate.totals.skipped, 0);
    assert_eq!(aggregate.queue.max_concurrency, 1);
    assert!(aggregate.queue.adaptive_concurrency.is_none());
}

#[test]
fn plan_level_component_contracts_are_preserved_on_executor_requests() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: true,
    });
    let raw = serde_json::json!({
        "schema": AGENT_TASK_PLAN_SCHEMA,
        "plan_id": "plan-components",
        "component_contracts": [{
            "slug": "generic-component",
            "path": "/workspace/generic-component",
            "loadAs": "plugin",
            "activate": true,
            "opaque_executor_hint": { "preserve": true }
        }],
        "tasks": [{
            "task_id": "task-components",
            "executor": { "backend": "test" },
            "instructions": "run"
        }]
    });
    let plan: AgentTaskPlan = serde_json::from_value(raw).expect("plan parses");

    let aggregate = scheduler.run(plan);
    let observed = observed.lock().expect("observed requests");
    let request = observed.first().expect("request dispatched");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(request.component_contracts.len(), 1);
    assert_eq!(
        request.component_contracts[0].slug.as_deref(),
        Some("generic-component")
    );
    assert_eq!(
        request.component_contracts[0].path.as_deref(),
        Some("/workspace/generic-component")
    );
    assert_eq!(
        request.component_contracts[0].load_as.as_deref(),
        Some("plugin")
    );
    assert_eq!(request.component_contracts[0].activate, Some(true));
    assert_eq!(
        request.component_contracts[0].extra["opaque_executor_hint"]["preserve"],
        true
    );
}

#[test]
fn legacy_agent_task_plan_json_round_trips_through_homeboy_plan_projection() {
    let mut plan = AgentTaskPlan::new("plan-projection", vec![request("idea"), request("design")]);
    plan.group_key = Some("group-a".to_string());
    plan.options.max_concurrency = 2;
    plan.metadata = json!({ "source": "compat" });
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: vec!["idea".to_string()],
            bindings: HashMap::from([(
                "issue_number".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: "/outputs/issue_number".to_string(),
                    artifact: None,
                    required: true,
                    default: Value::Null,
                },
            )]),
        },
    );
    plan.artifact_outputs.insert(
        "idea".to_string(),
        vec![AgentTaskArtifactOutputDeclaration {
            name: "concept_packet".to_string(),
            kind: "concept_packet".to_string(),
            schema: Some("example/concept-packet/v1".to_string()),
            artifact_id: Some("concept".to_string()),
            payload_path: Some("/title".to_string()),
        }],
    );

    let raw = serde_json::to_string(&plan).expect("serialize legacy contract");
    let value: Value = serde_json::from_str(&raw).expect("serialized json");
    assert_eq!(value["schema"], AGENT_TASK_PLAN_SCHEMA);
    assert!(value.get("homeboy_plan").is_none());

    let decoded: AgentTaskPlan = serde_json::from_str(&raw).expect("legacy plan decodes");
    let projected = AgentTaskPlan::from_homeboy_plan(decoded.homeboy_plan.clone());

    assert_eq!(
        decoded.homeboy_plan.kind,
        crate::core::plan::PlanKind::AgentTask
    );
    assert_eq!(projected.schema, AGENT_TASK_PLAN_SCHEMA);
    assert_eq!(projected.plan_id, plan.plan_id);
    assert_eq!(projected.group_key, plan.group_key);
    assert_eq!(projected.tasks, plan.tasks);
    assert_eq!(projected.output_dependencies, plan.output_dependencies);
    assert_eq!(projected.artifact_outputs, plan.artifact_outputs);
    assert_eq!(projected.options, plan.options);
    assert_eq!(projected.metadata, plan.metadata);
}

#[test]
fn scheduler_executes_from_projected_homeboy_plan() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
        observed: Arc::clone(&observed),
        include_issue_number: true,
    });
    let mut plan = AgentTaskPlan::new(
        "plan-homeboy-projection",
        vec![request("idea"), request("design")],
    );
    plan.options.max_concurrency = 2;
    plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
    plan.output_dependencies.insert(
        "design".to_string(),
        AgentTaskOutputDependencies {
            depends_on: vec!["idea".to_string()],
            bindings: HashMap::from([(
                "issue_number".to_string(),
                AgentTaskOutputBinding {
                    task_id: "idea".to_string(),
                    path: "/outputs/issue_number".to_string(),
                    artifact: None,
                    required: true,
                    default: Value::Null,
                },
            )]),
        },
    );
    plan.rebuild_homeboy_plan();
    let projected = AgentTaskPlan::from_homeboy_plan(plan.homeboy_plan.clone());

    let aggregate = scheduler.run(projected);
    let observed = observed.lock().expect("observed requests");
    let design = observed
        .iter()
        .find(|request| request.task_id == "design")
        .expect("design request dispatched");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 2);
    assert_eq!(design.instructions, "Build design for issue #3447");
}

#[test]
fn cancellation_stops_queued_tasks_and_notifies_running_executor() {
    let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(100));
    let cancel_calls = Arc::clone(&executor.cancel_calls);
    let running = Arc::clone(&executor.running);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(3);
    plan.options.max_concurrency = 1;
    let token = AgentTaskCancellationToken::default();
    let worker_token = token.clone();

    let handle = thread::spawn(move || scheduler.run_with_cancellation(plan, worker_token));
    while running.load(Ordering::SeqCst) == 0 {
        thread::sleep(Duration::from_millis(1));
    }
    token.cancel();
    let aggregate = handle.join().expect("scheduler thread");

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Cancelled);
    assert!(aggregate.totals.cancelled >= 2);
    assert!(cancel_calls
        .lock()
        .expect("cancel calls")
        .contains(&"task-1".to_string()));
}
