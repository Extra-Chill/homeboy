//! Outcome normalization and failed/incomplete provider-status detection.

use super::super::outcome::{
    is_terminal_status_key, nested_failed_executor_status, provider_run_result_is_empty_incomplete,
};
use super::super::*;
use super::fixtures::*;
use serde_json::json;
use std::fs;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[test]
fn provider_reported_model_is_retained_separately_from_requested_and_resolved_models() {
    let mut request = request("task-1");
    request.executor.model = Some("openai/gpt-5.6-terra".to_string());
    request.metadata = json!({
        "model_selection": { "requested": "openai/gpt-5.6-sol" }
    });
    let mut outcome = outcome("task-1".to_string(), AgentTaskOutcomeStatus::Succeeded);
    outcome.metadata = json!({ "model": "openai/gpt-5.6-terra" });

    crate::agent_task_scheduler::engine::persist_resolved_provider_model(&mut outcome, &request);

    assert_eq!(outcome.selected_model(), Some("openai/gpt-5.6-terra"));
    assert_eq!(
        outcome.metadata["model_identity"],
        json!({
            "requested": "openai/gpt-5.6-sol",
            "attempted": "openai/gpt-5.6-terra",
            "candidate_producing": "openai/gpt-5.6-terra",
            "resolved": "openai/gpt-5.6-terra",
            "provider_reported": "openai/gpt-5.6-terra",
            "actual": "openai/gpt-5.6-terra",
        })
    );
}

#[test]
fn nested_failed_executor_status_fails_succeeded_wrapper_outcome() {
    let scheduler = AgentTaskScheduler::new(NestedFailedStatusExecutor);

    let aggregate = scheduler.run(plan_with_tasks(1));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(aggregate.totals.succeeded, 0);
    assert_eq!(aggregate.outcomes[0].status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::ExecutionFailed)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.nested_executor_failed_status"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["path"],
        json!("outputs.provider_run_result.job_status")
    );
}

#[test]
fn nested_terminal_state_failure_fails_succeeded_wrapper_outcome() {
    let scheduler = AgentTaskScheduler::new(NestedTerminalStateFailedExecutor);

    let aggregate = scheduler.run(plan_with_tasks(1));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(aggregate.totals.succeeded, 0);
    assert_eq!(aggregate.outcomes[0].status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::ExecutionFailed)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.nested_executor_failed_status"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["path"],
        json!("outputs.provider_run_result.wait_result.terminal_state")
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["key"],
        json!("terminal_state")
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["value"],
        json!("failed - completion_required_tool_unavailable")
    );
}

#[test]
fn nested_agent_result_failure_fails_succeeded_wrapper_outcome() {
    let scheduler = AgentTaskScheduler::new(NestedAgentResultFailedExecutor);

    let aggregate = scheduler.run(plan_with_tasks(1));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(aggregate.totals.succeeded, 0);
    assert_eq!(aggregate.outcomes[0].status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.nested_executor_failed_status"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["path"],
        json!("typed_artifacts.agent_result.payload.status")
    );
}

#[test]
fn missing_required_typed_artifacts_fails_succeeded_outcome() {
    let scheduler = AgentTaskScheduler::new(SuccessMissingRequiredArtifactsExecutor);

    let aggregate = scheduler.run(plan_with_required_artifacts(&[
        "import_validation_result",
        "visual_parity_artifact",
    ]));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(aggregate.totals.succeeded, 0);
    assert_eq!(aggregate.outcomes[0].status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::ExecutionFailed)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.required_typed_artifacts_missing"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["missing"],
        json!(["import_validation_result", "visual_parity_artifact"])
    );
}

#[test]
fn nested_executor_failure_context_wins_over_missing_required_artifacts() {
    let scheduler = AgentTaskScheduler::new(NestedFailedStatusMissingArtifactsExecutor);

    let aggregate = scheduler.run(plan_with_required_artifacts(&[
        "patch",
        "agent_result",
        "transcript",
    ]));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    let outcome = &aggregate.outcomes[0];
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::ExecutionFailed)
    );
    assert!(outcome
        .summary
        .as_deref()
        .unwrap_or_default()
        .contains("outputs.provider_run_result.status=failed"));
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "agent_task.nested_executor_failed_status"));
    let nested_diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "agent_task.nested_executor_failed_status")
        .expect("nested executor failure diagnostic");
    assert_eq!(
        nested_diagnostic.data["provider_run_result"]["metadata"]["provider_error"]["message"],
        json!("OpenCode completed without producing declared artifact(s): patch, agent_result, transcript")
    );
    assert_eq!(
        nested_diagnostic.data["provider_run_result"]["refs"]["logs"][0],
        json!("file:///tmp/executor.stderr.log")
    );
    assert_eq!(
        nested_diagnostic.data["provider_run_result"]["refs"]["artifact_bundles"][0],
        json!("file:///tmp/runtime-artifacts")
    );
    assert!(!outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "agent_task.required_typed_artifacts_missing"));
}

#[test]
fn empty_required_patch_artifact_is_valid_no_diff_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let patch_path = temp.path().join("patch.diff");
    fs::write(&patch_path, "").expect("empty patch");
    let scheduler = AgentTaskScheduler::new(SuccessEmptyRequiredTypedArtifactExecutor {
        artifact_path: patch_path.clone(),
        artifact_name: "patch",
        artifact_kind: "patch",
    });

    let aggregate = scheduler.run(plan_with_required_artifacts(&["patch"]));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(
        aggregate.outcomes[0].status,
        AgentTaskOutcomeStatus::Succeeded
    );
    assert!(aggregate.outcomes[0]
        .diagnostics
        .iter()
        .all(|diagnostic| diagnostic.class != "agent_task.required_typed_artifacts_invalid"));
}

#[test]
fn empty_required_non_patch_typed_artifact_fails_with_operator_pointer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report_path = temp.path().join("report.json");
    fs::write(&report_path, "").expect("empty report");
    let scheduler = AgentTaskScheduler::new(SuccessEmptyRequiredTypedArtifactExecutor {
        artifact_path: report_path.clone(),
        artifact_name: "agent_result",
        artifact_kind: "json",
    });

    let plan = plan_with_required_artifacts(&["agent_result"]);
    assert_eq!(plan.options.execution_budget.max_provider_executions, 1);
    assert_eq!(
        plan.clone()
            .canonicalize()
            .options
            .execution_budget
            .max_provider_executions,
        1,
        "the scheduler's canonical plan must retain the explicit test execution budget"
    );
    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(aggregate.outcomes[0].status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::ExecutionFailed)
    );
    let diagnostic = aggregate.outcomes[0]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "agent_task.required_typed_artifacts_invalid")
        .expect("invalid required typed artifact diagnostic");
    assert_eq!(diagnostic.data["invalid"][0]["task_id"], json!("task-1"));
    assert_eq!(diagnostic.data["invalid"][0]["name"], json!("agent_result"));
    assert_eq!(
        diagnostic.data["invalid"][0]["artifact_id"],
        json!("empty-agent_result")
    );
    assert_eq!(
        diagnostic.data["invalid"][0]["path"],
        json!(report_path.display().to_string())
    );
    assert_eq!(diagnostic.data["invalid"][0]["size_bytes"], json!(0));
    assert_eq!(
        diagnostic.data["invalid"][0]["reason"],
        json!("declared artifact size is zero bytes")
    );
}

#[test]
fn nested_terminal_status_detection_is_terminal_status_key_contract() {
    // Status family keys.
    assert!(is_terminal_status_key("status"));
    assert!(is_terminal_status_key("job_status"));
    assert!(is_terminal_status_key("completion_status"));
    assert!(is_terminal_status_key("STATUS"));
    // State family keys (regression for #4683: terminal_state).
    assert!(is_terminal_status_key("state"));
    assert!(is_terminal_status_key("terminal_state"));
    assert!(is_terminal_status_key("run_state"));
    assert!(is_terminal_status_key("Terminal_State"));
    // Non-status-bearing keys are ignored.
    assert!(!is_terminal_status_key("message"));
    assert!(!is_terminal_status_key("estate"));
    assert!(!is_terminal_status_key("status_code"));
}

#[test]
fn nested_terminal_status_detection_only_flags_failure_values() {
    // A terminal_state that is not a failure value does not trip detection.
    let succeeded_state = AgentTaskOutcome {
        outputs: json!({
            "provider_run_result": {
                "wait_result": { "terminal_state": "succeeded" },
                "completion_status": "complete"
            }
        }),
        ..outcome("task-1".to_string(), AgentTaskOutcomeStatus::Succeeded)
    };
    assert!(nested_failed_executor_status(&succeeded_state).is_none());

    // A failed terminal_state nested under wait_result is detected and
    // reported with its full dotted path.
    let failed_state = AgentTaskOutcome {
        outputs: json!({
            "provider_run_result": {
                "completion_status": "partial",
                "wait_result": {
                    "terminal_state": "failed - completion_required_tool_unavailable"
                }
            }
        }),
        ..outcome("task-1".to_string(), AgentTaskOutcomeStatus::Succeeded)
    };
    let detected = nested_failed_executor_status(&failed_state).expect("detect failed state");
    assert_eq!(detected.key, "terminal_state");
    assert_eq!(
        detected.path,
        "outputs.provider_run_result.wait_result.terminal_state"
    );
    assert_eq!(
        detected.value,
        "failed - completion_required_tool_unavailable"
    );
}

#[test]
fn incomplete_empty_executor_result_is_retryable_provider_failure() {
    let executor = EmptyIncompleteThenSuccessExecutor::default();
    let attempts = Arc::clone(&executor.attempts);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(1);
    plan.options.retry.max_attempts = 2;
    plan.options.execution_budget.max_provider_executions = 2;
    plan.options.execution_budget.max_same_provider_retries = 1;
    plan.options.retry.retryable_failure_classifications =
        vec![AgentTaskFailureClassification::Provider];

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(aggregate.events.iter().any(|event| {
        event.state == AgentTaskState::Queued
            && event.attempt == 2
            && event.message.as_deref() == Some("retry queued")
    }));
}

#[test]
fn incomplete_empty_executor_result_fails_when_retries_are_unavailable() {
    let scheduler = AgentTaskScheduler::new(EmptyIncompleteExecutor);

    let aggregate = scheduler.run(plan_with_tasks(1));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].status,
        AgentTaskOutcomeStatus::ProviderError
    );
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::Provider)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.executor_incomplete_empty_result"
    );
}

#[test]
fn policy_denial_is_terminal_even_when_retry_policy_matches_every_failure() {
    let mut denied = outcome("policy-denied".to_string(), AgentTaskOutcomeStatus::Failed);
    denied.failure_classification = Some(AgentTaskFailureClassification::PolicyDenied);

    assert!(!AgentTaskScheduleSupport::should_retry(
        &denied,
        1,
        2,
        3,
        3,
        None,
        0,
        &[],
    ));
    assert!(!AgentTaskScheduleSupport::should_retry(
        &denied,
        1,
        2,
        3,
        3,
        None,
        0,
        &[AgentTaskFailureClassification::PolicyDenied],
    ));
}

#[test]
fn base_bound_patch_survives_policy_denial_and_missing_review_form_for_fresh_review() {
    let patch = "diff --git a/lib.rs b/lib.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-base\n+candidate\n";
    let mut denied = outcome(
        "policy-denied-patch".to_string(),
        AgentTaskOutcomeStatus::Failed,
    );
    denied.failure_classification = Some(AgentTaskFailureClassification::PolicyDenied);
    denied.artifacts = vec![AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "candidate".to_string(),
        kind: "patch".to_string(),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
            patch.as_bytes(),
        )),
        metadata: json!({
            "task_id": "policy-denied-patch",
            "producer_attempt": 1,
            "run_id": "run-11280",
            "base_ref": "base-11280",
            "provider_backend": "provider",
            "repository_identity": "repository-11280",
            "workspace_identity": "workspace-11280",
        }),
        ..Default::default()
    }];

    AgentTaskScheduleSupport::preserve_base_bound_patch_after_provider_failure(&mut denied);

    assert_eq!(denied.status, AgentTaskOutcomeStatus::CandidateRecoverable);
    assert_eq!(
        denied.failure_classification,
        Some(AgentTaskFailureClassification::PolicyDenied)
    );
    assert!(denied.outputs.get("review_form").is_none());
    assert!(denied.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "agent_task.provider_failure_recoverable_candidate"
            && diagnostic.data["recovery_action"] == "homeboy agent-task review <run-id>"
    }));
    let outcomes = vec![denied.clone()];
    assert_eq!(
        AgentTaskScheduleSupport::aggregate_status(&outcomes),
        AgentTaskAggregateStatus::PartialRecoverable
    );
    let totals = AgentTaskScheduleSupport::totals(1, &outcomes);
    assert_eq!(totals.candidate_recoverable, 1);
    assert_eq!(totals.recoverable_candidates, 1);
    assert_eq!(totals.failed, 0);
    assert!(!AgentTaskScheduleSupport::should_retry(
        &denied,
        1,
        2,
        3,
        3,
        None,
        0,
        &[],
    ));
}

#[test]
fn incomplete_nested_outputs_provider_result_is_detected() {
    // Mirrors a provider wrapper shape: the provider claims
    // top-level success/completed, but `outputs.completed` is false, the reply
    // is empty, and `messages` only contains the initial user prompt.
    let result = json!({
        "success": true,
        "status": "completed",
        "outputs": {
            "reply": "",
            "messages": [{ "role": "user", "content": "cook the issue" }],
            "completed": false,
            "run_id": "run_abc"
        },
        "structured_artifacts": [],
        "diagnostics": {}
    });

    assert!(provider_run_result_is_empty_incomplete(&result));
}

#[test]
fn completed_provider_result_with_assistant_message_is_not_flagged() {
    let result = json!({
        "success": true,
        "status": "completed",
        "outputs": {
            "reply": "patched src/lib.rs",
            "messages": [
                { "role": "user", "content": "cook the issue" },
                { "role": "assistant", "content": "applied the fix" }
            ],
            "completed": true,
            "run_id": "run_abc"
        }
    });

    assert!(!provider_run_result_is_empty_incomplete(&result));
}

#[test]
fn flat_incomplete_provider_result_is_still_detected() {
    let result = json!({
        "completed": false,
        "reply": "",
        "messages": [],
        "tool_calls": []
    });

    assert!(provider_run_result_is_empty_incomplete(&result));
}

#[test]
fn incomplete_nested_outputs_executor_result_fails_when_retries_are_unavailable() {
    let scheduler = AgentTaskScheduler::new(NestedOutputsIncompleteExecutor);

    let aggregate = scheduler.run(plan_with_tasks(1));

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].status,
        AgentTaskOutcomeStatus::ProviderError
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.executor_incomplete_empty_result"
    );
}
