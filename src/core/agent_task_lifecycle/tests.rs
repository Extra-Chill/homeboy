//! Tests for agent_task_lifecycle (extracted from mod.rs to keep mod.rs under structural thresholds).
#![cfg(test)]

use super::*;
use crate::core::agent_task::{
    AgentTaskArtifactDeclaration, AgentTaskExecutionHandle, AgentTaskExecutor, AgentTaskLimits,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence,
    AgentTaskWorkflowStepStatus, AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
    AGENT_TASK_WORKFLOW_SCHEMA,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use crate::test_support::with_isolated_home;

#[test]
fn provider_run_result_reads_declared_output_alias() {
    let role_aliases: AgentTaskProviderRoleAliases = serde_json::from_value(json!({
        "outputs": {
            "provider_run_result": ["custom_run_result"]
        }
    }))
    .expect("role aliases");
    let outcome = AgentTaskOutcome {
        schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "task-a".to_string(),
        status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
        summary: None,
        failure_classification: None,
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: json!({
            "custom_run_result": {
                "run_id": "custom-run-1"
            }
        }),
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    };

    assert_eq!(
        provider_run_result(&outcome, &role_aliases)
            .and_then(|result| result.get("run_id"))
            .and_then(Value::as_str),
        Some("custom-run-1")
    );
}

#[test]
fn submit_plan_persists_queued_status() {
    with_isolated_home(|_| {
        let plan = test_plan();

        let record = submit_plan(&plan, Some("run/a")).expect("submitted");
        let loaded = status(&record.run_id).expect("status loaded");

        assert_eq!(record.run_id, "run_a");
        assert_eq!(loaded.state, AgentTaskRunState::Queued);
        assert_eq!(loaded.tasks[0].task_id, "task-a");
        assert_eq!(
            loaded.tasks[0].provider_ref.as_deref(),
            Some("test:fixture")
        );
    });
}

#[test]
fn record_promotion_persists_latest_event_on_run_metadata() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-promotion-status")).expect("submitted");

        let promotion = json!({
            "schema": "homeboy/agent-task-promotion-status/v1",
            "status": "applied",
            "source_run_id": "run-promotion-status",
            "patch_artifact_id": "patch.diff",
            "to_worktree": "homeboy@fix-5055",
            "target": {
                "worktree": "homeboy@fix-5055",
                "branch": "fix/5055",
                "head": "abc123"
            },
            "operator_notification": {
                "status": "completed",
                "message": "patch promoted into homeboy@fix-5055"
            }
        });

        let updated = record_promotion("run-promotion-status", promotion.clone())
            .expect("promotion recorded");
        let loaded = status("run-promotion-status").expect("status loaded");

        assert_eq!(updated.metadata["latest_promotion"], promotion);
        assert_eq!(
            loaded.metadata["latest_promotion"]["patch_artifact_id"],
            "patch.diff"
        );
        assert_eq!(
            loaded.metadata["promotions"]
                .as_array()
                .expect("events")
                .len(),
            1
        );
    });
}

#[test]
fn pre_dispatch_failure_persists_failed_run_without_provider_handle() {
    with_isolated_home(|_| {
        let record = record_pre_dispatch_failure(AgentTaskPreDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "cook-lab-predispatch",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--run-id".to_string(),
                    "cook-lab-predispatch".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--cwd".to_string(),
                    "/runner/workspace/repo".to_string(),
                ],
                remote_workspace: "/runner/workspace/repo",
                failure_message: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout",
                stdout: "",
                stderr: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout\n",
                exit_code: 1,
            })
            .expect("pre-dispatch failure recorded");

        let loaded = status("cook-lab-predispatch").expect("status loaded");
        let log = logs("cook-lab-predispatch").expect("logs loaded");
        let artifact_report = artifacts("cook-lab-predispatch").expect("artifacts loaded");
        let legacy_status_path = crate::core::paths::homeboy_data()
            .expect("homeboy data")
            .join("agent-task-runs")
            .join("cook-lab-predispatch")
            .join("status.json");
        std::fs::remove_file(
            crate::core::paths::homeboy_data()
                .expect("homeboy data")
                .join("agent-task-runs")
                .join("cook-lab-predispatch")
                .join("aggregate.json"),
        )
        .expect("aggregate file removed");
        let mirrored_log = logs("cook-lab-predispatch").expect("mirrored logs loaded");
        let mirrored_artifacts =
            artifacts("cook-lab-predispatch").expect("mirrored artifacts loaded");

        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
        assert!(loaded.provider_handles.is_empty());
        assert_eq!(log.events[1].state, AgentTaskState::Failed);
        assert_eq!(mirrored_log.events[1].state, AgentTaskState::Failed);
        assert_eq!(loaded.metadata["provider_run_ids"], serde_json::json!([]));
        assert_eq!(
            loaded.artifact_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            artifact_report.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            mirrored_artifacts.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert!(
            !legacy_status_path.exists(),
            "agent-task status.json is no longer the primary durable run record"
        );
    });
}

#[test]
fn remote_dispatch_failure_preserves_structured_outcome_details() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "logs".to_string(),
                    uri: "homeboy://agent-task/run/remote-run/logs".to_string(),
                    label: Some("remote provider logs".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: serde_json::json!({
                    "provider_run_result": {
                        "status": "failed",
                        "failure_classification": "runtime",
                        "artifacts": [],
                        "refs": { "logs": [], "transcripts": [], "runtimes": [] }
                    }
                }),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "remote_run_id": "provider-run-1",
                    "remote_workspace": "/runner/workspace/repo"
                }),
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some("Remote provider agent task failed.".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus {
                max_concurrency: 1,
                completed: 1,
                ..AgentTaskQueueStatus::default()
            },
        };
        let remote_record =
            record_completed_run(&plan, &aggregate, Some("remote-run")).expect("remote record");
        let envelope = serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": "remote-run",
            "plan_id": plan.plan_id,
            "state": "failed",
            "record": remote_record,
            "aggregate": aggregate,
        });

        let record = record_remote_dispatch_failure(
            AgentTaskRemoteDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "local-run",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_workspace: "/runner/workspace/repo",
                stdout: &envelope.to_string(),
                stderr: "",
                exit_code: 1,
            },
            &envelope,
        )
        .expect("remote dispatch failure recorded")
        .expect("dispatch envelope recognized");

        let loaded = status("local-run").expect("status loaded");
        let log = logs("local-run").expect("logs loaded");
        let artifacts = artifacts("local-run").expect("artifacts loaded");
        let (raw_aggregate, _) = aggregate_source("local-run").expect("aggregate source");

        assert_eq!(record.run_id, "local-run");
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].task_id, "task-a");
        assert_ne!(loaded.tasks[0].task_id, "agent-task-predispatch");
        assert_eq!(
            loaded.metadata["kind"],
            "lab_offload_remote_dispatch_failure"
        );
        assert_eq!(loaded.metadata["runner_id"], "lab-a");
        assert!(std::path::Path::new(&loaded.plan_path).is_file());
        let loaded_plan = load_plan("local-run").expect("plan loaded");
        assert_eq!(loaded_plan.plan_id, "plan-a");
        assert_eq!(loaded_plan.tasks[0].task_id, "task-a");
        assert_eq!(
            loaded.metadata["remote_workspace"],
            "/runner/workspace/repo"
        );
        assert_eq!(
            log.events[0].message.as_deref(),
            Some("Remote provider agent task failed.")
        );
        assert_eq!(artifacts.evidence_refs[0].kind, "logs");
        assert!(raw_aggregate.contains("fixture.agent-task-executor"));
        assert!(raw_aggregate.contains("failure_classification"));
    });
}

#[test]
fn aggregate_only_remote_dispatch_failure_preserves_lab_outcome_details() {
    with_isolated_home(|_| {
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: "remote-plan".to_string(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "cook-conductor".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "provider-run".to_string(),
                    uri: "homeboy://provider/runs/provider-run-1".to_string(),
                    label: Some("Provider run".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: serde_json::json!({
                    "provider_run_result": {
                        "schema": "custom-provider/agent-task-run-result/v1",
                        "run_id": "provider-run-1",
                        "status": "failed",
                        "failure_classification": "runtime",
                        "metadata": {
                            "remote_plan_ref": "remote-plan",
                            "remote_run_ref": "remote-run"
                        }
                    }
                }),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "remote_run_id": "provider-run-1",
                }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus {
                max_concurrency: 1,
                completed: 1,
                ..AgentTaskQueueStatus::default()
            },
        };
        let envelope = serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": "remote-run",
            "plan_id": "remote-plan",
            "state": "failed",
            "aggregate": aggregate,
        });

        let record = record_remote_dispatch_failure(
            AgentTaskRemoteDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "conductor-full-loop-proof-retry2-20260611",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_workspace: "/runner/workspace/conductor",
                stdout: &envelope.to_string(),
                stderr: "",
                exit_code: 1,
            },
            &envelope,
        )
        .expect("aggregate-only dispatch failure recorded")
        .expect("dispatch envelope recognized");

        let loaded = status("conductor-full-loop-proof-retry2-20260611").expect("status loaded");
        let log = logs("conductor-full-loop-proof-retry2-20260611").expect("logs loaded");
        let artifacts =
            artifacts("conductor-full-loop-proof-retry2-20260611").expect("artifacts loaded");
        let (raw_aggregate, _) = aggregate_source("conductor-full-loop-proof-retry2-20260611")
            .expect("aggregate source");

        assert_eq!(record.run_id, "conductor-full-loop-proof-retry2-20260611");
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].task_id, "cook-conductor");
        assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(loaded.tasks[0].backend, "fixture.agent-task-executor");
        assert_eq!(loaded.provider_handles.len(), 1);
        assert_eq!(loaded.provider_handles[0].provider_run_id, "provider-run-1");
        assert_eq!(loaded.metadata["remote_run_id"], "remote-run");
        assert_eq!(loaded.metadata["remote_plan_path"], "remote-plan");
        assert_eq!(
            log.events[0].message.as_deref(),
            Some("Remote provider agent task failed.")
        );
        assert_eq!(artifacts.evidence_refs[0].kind, "provider-run");
        assert!(raw_aggregate.contains("custom-provider/agent-task-run-result/v1"));
        assert!(raw_aggregate.contains("failure_classification"));
        assert!(raw_aggregate.contains("remote_plan_ref"));
    });
}

#[test]
fn sparse_aggregate_only_remote_dispatch_failure_adds_remote_evidence_refs() {
    with_isolated_home(|_| {
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: "remote-plan".to_string(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "cook-conductor".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("Remote provider agent task failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: serde_json::json!({}),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({
                    "provider": "fixture.agent-task-executor",
                    "provider_run_result": {
                        "schema": "custom-provider/agent-task-run-result/v1",
                        "status": "failed",
                        "failure_classification": "runtime"
                    }
                }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus {
                max_concurrency: 1,
                completed: 1,
                ..AgentTaskQueueStatus::default()
            },
        };
        let envelope = serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": "remote-run",
            "plan_id": "remote-plan",
            "state": "failed",
            "aggregate": aggregate,
        });

        record_remote_dispatch_failure(
            AgentTaskRemoteDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "local-sparse-run",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
                remote_workspace: "/runner/workspace/conductor",
                stdout: "",
                stderr: &envelope.to_string(),
                exit_code: 1,
            },
            &envelope,
        )
        .expect("sparse dispatch failure recorded")
        .expect("dispatch envelope recognized");

        let loaded = status("local-sparse-run").expect("status loaded");
        let artifacts = artifacts("local-sparse-run").expect("artifacts loaded");
        let (raw_aggregate, _) = aggregate_source("local-sparse-run").expect("aggregate source");

        assert_eq!(loaded.tasks[0].task_id, "cook-conductor");
        assert_eq!(loaded.tasks[0].backend, "fixture.agent-task-executor");
        assert_eq!(loaded.metadata["remote_run_id"], "remote-run");
        assert!(artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "remote-agent-task-logs"));
        assert!(artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "remote-agent-task-review"));
        assert!(raw_aggregate.contains("custom-provider/agent-task-run-result/v1"));
        assert!(raw_aggregate.contains("failure_classification"));
    });
}

#[test]
fn record_completed_run_exposes_logs_and_artifacts() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                succeeded: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("patch.diff".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some("/tmp/patch.diff".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "file:///tmp/transcript.json".to_string(),
                    label: Some("provider transcript".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };

        let record =
            record_completed_run(&plan, &aggregate, Some("run-complete")).expect("recorded");
        let log = logs(&record.run_id).expect("logs");
        let artifacts = artifacts(&record.run_id).expect("artifacts");

        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert_eq!(artifacts.artifacts[0].id, "patch");
        assert_eq!(artifacts.evidence_refs[0].kind, "transcript");
    });
}

#[test]
fn completed_run_exposes_latest_executor_input_output_and_expectations() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        let request = &mut plan.tasks[0];
        request.executor.backend = "sandbox".to_string();
        request.executor.model = Some("gpt-fixture".to_string());
        request.component_contracts = vec![AgentTaskComponentContract {
            slug: Some("runtime-engine".to_string()),
            path: Some("/workspace/runtime-engine".to_string()),
            load_as: Some("plugin".to_string()),
            activate: Some(true),
            extra: Default::default(),
        }];
        request.metadata = json!({
            "runtime_component_paths": ["/runtime/components/sandbox-host"]
        });
        request.expected_artifacts = vec!["patch".to_string()];
        request.artifact_declarations = vec![AgentTaskArtifactDeclaration {
            name: "proof_bundle".to_string(),
            artifact_type: Some("bundle".to_string()),
            artifact_schema: None,
            path: None,
            required: true,
            description: None,
            metadata: Value::Null,
        }];

        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].outputs = json!({
            "provider_run_result": {
                "run_id": "provider-run-123",
                "status": "succeeded"
            }
        });

        let record =
            record_completed_run(&plan, &aggregate, Some("run-evidence")).expect("recorded");
        let evidence = record
            .latest_executor_evidence
            .as_ref()
            .expect("latest executor evidence");
        let artifact_report = artifacts("run-evidence").expect("artifacts loaded");

        assert_eq!(evidence.task_id, "task-a");
        assert_eq!(evidence.backend, "sandbox");
        assert_eq!(evidence.selector.as_deref(), Some("fixture"));
        assert_eq!(evidence.model.as_deref(), Some("gpt-fixture"));
        assert_eq!(
            evidence.provider_run_id.as_deref(),
            Some("provider-run-123")
        );
        assert_eq!(evidence.component_contracts.len(), 1);
        assert_eq!(
            evidence.runtime_component_paths,
            vec![
                "/runtime/components/sandbox-host".to_string(),
                "/workspace/runtime-engine".to_string()
            ]
        );
        assert_eq!(evidence.expected_artifacts, vec!["patch".to_string()]);
        assert_eq!(
            evidence.typed_artifact_expectations,
            vec!["proof_bundle".to_string()]
        );
        assert_eq!(
            record.metadata["latest_executor_evidence"]["input_ref"]["uri"],
            "homeboy://agent-task/run/run-evidence/plan#task=task-a"
        );
        assert!(artifact_report
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "executor-input"));
        assert!(artifact_report
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "executor-normalized-output"));
        assert!(artifact_report
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "executor-outcome"));
    });
}

#[test]
fn submitted_run_can_be_loaded_marked_running_and_completed() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-execute")).expect("submitted");

        let loaded_plan = load_plan("run-execute").expect("plan loaded");
        let running = mark_running("run-execute").expect("marked running");
        let aggregate = succeeded_aggregate(&loaded_plan);

        let completed =
            record_run_aggregate("run-execute", &loaded_plan, &aggregate).expect("completed");
        let durable_status = status("run-execute").expect("status");

        assert_eq!(loaded_plan.plan_id, "plan-a");
        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.tasks[0].state, AgentTaskState::Running);
        assert_eq!(
            running.lifecycle.execution.state,
            RunExecutionState::Running
        );
        assert!(running.lifecycle.heartbeat.is_some());
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(
            completed.lifecycle.execution.state,
            RunExecutionState::Succeeded
        );
        assert_eq!(completed.totals, Some(aggregate.totals.clone()));
        assert_eq!(durable_status.state, AgentTaskRunState::Succeeded);
        assert_eq!(durable_status.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(durable_status.totals, Some(aggregate.totals.clone()));
        assert!(completed.aggregate_path.is_some());
    });
}

#[test]
fn lifecycle_store_round_trips_record_log_artifacts_and_lifecycle_contract() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts = vec![artifact_ref_artifact(
            "patch",
            "patch",
            None,
            Some("/tmp/patch.diff"),
        )];
        aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("provider transcript".to_string()),
        }];

        let record = record_completed_run(&plan, &aggregate, Some("run/store-contract"))
            .expect("completed run recorded");
        let loaded = status("run/store-contract").expect("status loaded by unsanitized id");
        let log = logs("run/store-contract").expect("logs loaded by unsanitized id");
        let artifact_report =
            artifacts("run/store-contract").expect("artifacts loaded by unsanitized id");
        let records = list_records().expect("records listed");

        assert_eq!(record.run_id, "run_store-contract");
        assert!(run_record_exists("run/store-contract").expect("record exists"));
        assert_eq!(loaded.state, AgentTaskRunState::Succeeded);
        assert_eq!(loaded.lifecycle.schema, RUN_LIFECYCLE_RECORD_SCHEMA);
        assert_eq!(
            loaded.lifecycle.execution.state,
            RunExecutionState::Succeeded
        );
        assert_eq!(loaded.lifecycle.cleanup.state, CleanupState::Preserved);
        assert_eq!(
            loaded.lifecycle.artifact_retention.status,
            ArtifactRetentionStatus::Retained
        );
        assert_eq!(log.schema, schemas::RUN_LOG);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert_eq!(artifact_report.schema, schemas::RUN_ARTIFACTS);
        assert_eq!(artifact_report.artifacts[0].id, "patch");
        assert_eq!(artifact_report.evidence_refs[0].kind, "transcript");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run_store-contract");
    });
}

#[test]
fn completed_run_persists_opaque_provider_handles_from_outcome_metadata() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].metadata = json!({
            "provider_handle": AgentTaskExecutionHandle {
                kind: AgentTaskExecutionHandleKind::ProviderRun,
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                run_id: "provider-run-123".to_string(),
                stream_uri: Some("provider://runs/provider-run-123/events".to_string()),
                metadata: json!({ "opaque": { "provider_owned": true } }),
            }
        });

        let record =
            record_completed_run(&plan, &aggregate, Some("run-provider-handle")).expect("recorded");

        assert_eq!(record.provider_handles.len(), 1);
        assert_eq!(record.provider_handles[0].task_id, "task-a");
        assert_eq!(record.provider_handles[0].backend, "sample-runtime");
        assert_eq!(
            record.provider_handles[0].provider_run_id,
            "provider-run-123"
        );
        assert_eq!(
            record.provider_handles[0].stream_uri.as_deref(),
            Some("provider://runs/provider-run-123/events")
        );
        assert_eq!(
            record.provider_handles[0].state,
            Some(AgentTaskState::Succeeded)
        );
        assert_eq!(
            record.provider_handles[0].metadata["opaque"]["provider_owned"],
            json!(true)
        );
        assert_eq!(
            record.metadata["provider_run_ids"],
            json!(["provider-run-123"])
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            ProviderRuntimeState::Succeeded
        );
        assert_eq!(
            record.lifecycle.external_runtime_ids[0].value,
            "provider-run-123"
        );
        assert_eq!(
            record.lifecycle.artifact_retention.status,
            ArtifactRetentionStatus::NotApplicable
        );
    });
}

#[test]
fn failed_provider_run_exposes_workflow_evidence_refs() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                failed: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                summary: Some("provider task failed".to_string()),
                failure_classification: Some(
                    crate::core::agent_task::AgentTaskFailureClassification::ExecutionFailed,
                ),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: Some(AgentTaskWorkflowEvidence {
                    schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
                    id: "provider-run-123".to_string(),
                    label: Some("provider workflow".to_string()),
                    steps: vec![AgentTaskWorkflowStepEvidence {
                        id: "runtime".to_string(),
                        label: Some("runtime evidence".to_string()),
                        status: AgentTaskWorkflowStepStatus::Failed,
                        depends_on: Vec::new(),
                        started_at: None,
                        finished_at: None,
                        duration_ms: None,
                        metrics: Value::Null,
                        artifact_refs: vec![AgentTaskEvidenceRef {
                            kind: "provider-transcript".to_string(),
                            uri: "provider://runs/provider-run-123/transcript".to_string(),
                            label: Some("Provider transcript".to_string()),
                        }],
                        diagnostics: Vec::new(),
                        suggestions: Vec::new(),
                        metadata: Value::Null,
                    }],
                    metadata: Value::Null,
                }),
                follow_up: None,
                metadata: Value::Null,
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some("provider task failed".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };

        let record =
            record_completed_run(&plan, &aggregate, Some("run-provider-failed")).expect("recorded");
        let durable_status = status(&record.run_id).expect("status");
        let durable_artifacts = artifacts(&record.run_id).expect("artifacts");

        assert_eq!(durable_status.state, AgentTaskRunState::Failed);
        assert_eq!(durable_status.artifact_refs.len(), 1);
        assert_eq!(durable_status.artifact_refs[0].kind, "provider-transcript");
        assert_eq!(durable_artifacts.evidence_refs.len(), 4);
        assert_eq!(
            durable_artifacts.evidence_refs[0].uri,
            "provider://runs/provider-run-123/transcript"
        );
        assert!(durable_artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "executor-input"));
    });
}

#[test]
fn cancel_marks_queued_run_and_tasks_cancelled() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel")).expect("submitted");

        let record = cancel("run-cancel").expect("cancelled");

        assert_eq!(record.state, AgentTaskRunState::Cancelled);
        assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
        assert!(record.metadata["cancel_requested_at"].is_string());
    });
}

#[test]
fn retry_submits_new_run_from_existing_plan() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-original")).expect("submitted");

        let record = retry("run-original", Some("run-retry")).expect("retry submitted");
        let loaded_plan = load_plan("run-retry").expect("retry plan loaded");

        assert_eq!(record.run_id, "run-retry");
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(record.metadata["retry_of"], json!("run-original"));
        assert_eq!(loaded_plan.plan_id, "plan-a");
    });
}

#[test]
fn status_recovers_terminal_state_from_durable_aggregate() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-status")).expect("submitted");
        mark_running("run-stale-status").expect("marked running");
        let aggregate = succeeded_aggregate(&plan);
        store::write_aggregate("run-stale-status", &aggregate).expect("aggregate written");

        let recovered = status("run-stale-status").expect("status recovered");
        let persisted = store::read_record("run-stale-status").expect("record persisted");

        assert_eq!(recovered.state, AgentTaskRunState::Succeeded);
        assert_eq!(recovered.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(recovered.totals, Some(aggregate.totals.clone()));
        assert_eq!(persisted.state, AgentTaskRunState::Succeeded);
        assert_eq!(persisted.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(persisted.totals, Some(aggregate.totals.clone()));
    });
}

#[test]
fn status_marks_running_run_without_owner_as_stale() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-missing-owner")).expect("submitted");
        let mut record = store::read_record("run-stale-missing-owner").expect("record");
        record.state = AgentTaskRunState::Running;
        store::write_record(&record).expect("stored running record");

        let loaded = status("run-stale-missing-owner").expect("status loaded");

        assert_eq!(loaded.state, AgentTaskRunState::Running);
        assert_eq!(loaded.metadata["stale_running"], json!(true));
        assert_eq!(
            loaded.metadata["stale_running_reason"],
            "missing_runner_pid"
        );
    });
}

#[test]
fn aggregate_source_loads_completed_run_without_path_spelunking() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                succeeded: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };
        record_completed_run(&plan, &aggregate, Some("run-source")).expect("recorded");
        let local_path = store::aggregate_path("run-source").expect("local aggregate path");
        let mut record = store::read_record("run-source").expect("record loaded");
        record.aggregate_path = Some("/home/user/remote/aggregate.json".to_string());
        store::write_record(&record).expect("remote aggregate path stored");
        std::fs::remove_file(&local_path).expect("local aggregate removed");

        let (raw, path) = aggregate_source("run-source").expect("aggregate source");

        assert!(path.ends_with("aggregate.json"));
        assert_ne!(path, PathBuf::from("/home/user/remote/aggregate.json"));
        assert!(raw.contains("task-a"));
    });
}

#[test]
fn mark_running_reclaims_stale_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-dead-owner")).expect("submitted");
        let mut record = store::read_record("run-stale-dead-owner").expect("record");
        record.state = AgentTaskRunState::Running;
        record.metadata = json!({ "runner_pid": u32::MAX });
        store::write_record(&record).expect("stored stale record");

        let running = mark_running("run-stale-dead-owner").expect("reclaimed");

        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.metadata["reclaimed_stale_running"], json!(true));
        assert_eq!(running.metadata["runner_pid"], json!(std::process::id()));
    });
}

#[test]
fn mark_running_rejects_live_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-live-owner")).expect("submitted");
        mark_running("run-live-owner").expect("marked running");

        let error = mark_running("run-live-owner").expect_err("live run rejected");

        assert!(error.message.contains("already running"));
    });
}

#[test]
fn cancel_run_marks_queued_record_cancelled() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-queued")).expect("submitted");

        let cancelled =
            cancel_run("run-cancel-queued", Some("loser cell")).expect("queued run cancelled");
        let loaded = status("run-cancel-queued").expect("status loaded");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(cancelled.metadata["cancel_reason"], json!("loser cell"));
        assert_eq!(loaded.state, AgentTaskRunState::Cancelled);
    });
}

#[test]
fn cancel_run_reclaims_stale_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-stale")).expect("submitted");
        let mut record = store::read_record("run-cancel-stale").expect("record");
        record.state = AgentTaskRunState::Running;
        record.tasks[0].state = AgentTaskState::Running;
        record.metadata = json!({ "runner_pid": u32::MAX });
        store::write_record(&record).expect("stored stale record");

        let cancelled = cancel_run("run-cancel-stale", None).expect("stale run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(cancelled.metadata["cancelled_stale_running"], json!(true));
        assert!(cancelled.metadata.get("stale_running").is_none());
    });
}

#[test]
fn cancel_run_signals_live_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-live")).expect("submitted");
        mark_running("run-cancel-live").expect("marked running");

        let cancelled = cancel_run("run-cancel-live", None).expect("live run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(
            cancelled.metadata["live_cancellation"]["owner_pid"],
            json!(std::process::id())
        );
        assert_eq!(
            cancelled.metadata["live_cancellation"]["signal"],
            json!("SIGTERM")
        );
    });
}

#[test]
fn cancel_run_emits_recovery_commands_for_runner_backed_run() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-runner")).expect("submitted");
        let mut record = store::read_record("run-cancel-runner").expect("record");
        record.state = AgentTaskRunState::Running;
        record.tasks[0].state = AgentTaskState::Running;
        // Runner-backed: owner pid lives on the runner host (not running
        // here), so live cancellation must hand back recovery commands.
        record.metadata = json!({
            "runner_pid": u32::MAX,
            "runner_id": "lab-a",
            "runner_job_id": "job-123",
        });
        store::write_record(&record).expect("stored runner record");

        let cancelled = cancel_run("run-cancel-runner", None).expect("runner run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        let unsupported = &cancelled.metadata["live_cancellation_unsupported"];
        assert!(unsupported.is_object());
        assert_eq!(unsupported["runner_id"], json!("lab-a"));
        assert_eq!(unsupported["runner_job_id"], json!("job-123"));
        let commands = unsupported["recovery_commands"]
            .as_array()
            .expect("recovery commands array");
        assert!(!commands.is_empty());
        // The first recovery command should route cancellation to the
        // owning runner so the operator can act deterministically.
        assert!(commands[0]
            .as_str()
            .expect("command string")
            .contains("homeboy runner exec lab-a"));
        // No real local process was signalled.
        assert!(cancelled.metadata.get("live_cancellation").is_none());
    });
}

#[test]
fn list_records_skips_malformed_observation_records() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("good-run")).expect("submitted");
        let store = crate::core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        store
            .upsert_imported_run(&crate::core::observation::RunRecord {
                id: "bad-run".to_string(),
                kind: "agent-task".to_string(),
                component_id: None,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                finished_at: None,
                status: "running".to_string(),
                command: None,
                cwd: None,
                homeboy_version: None,
                git_sha: None,
                rig_id: None,
                metadata_json: json!({ "schema": "homeboy/agent-task-observation-record/v1" }),
            })
            .expect("bad record inserted");

        let records = list_records().expect("records listed");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "good-run");
    });
}

fn outcome_with_refs(
    task_id: &str,
    artifacts: Vec<AgentTaskArtifact>,
    evidence_refs: Vec<AgentTaskEvidenceRef>,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
        summary: Some("ok".to_string()),
        failure_classification: None,
        artifacts,
        typed_artifacts: Vec::new(),
        evidence_refs,
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

fn artifact_ref_artifact(
    id: &str,
    kind: &str,
    url: Option<&str>,
    path: Option<&str>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: Some(format!("{kind} artifact")),
        label: None,
        role: None,
        semantic_key: None,
        path: path.map(str::to_string),
        url: url.map(str::to_string),
        mime: None,
        size_bytes: None,
        sha256: None,
        metadata: Value::Null,
    }
}

#[test]
fn artifact_refs_omit_evidence_refs_with_empty_uri() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        Vec::new(),
        vec![
            AgentTaskEvidenceRef {
                kind: "sample-runtime-command-log".to_string(),
                uri: "".to_string(),
                label: Some("command log".to_string()),
            },
            AgentTaskEvidenceRef {
                kind: "sample-runtime-command-evidence".to_string(),
                uri: "   ".to_string(),
                label: None,
            },
            AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: "file:///tmp/transcript.json".to_string(),
                label: Some("provider transcript".to_string()),
            },
        ],
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(refs.len(), 1, "empty/whitespace evidence URIs are dropped");
    assert_eq!(refs[0].kind, "transcript");
    assert_eq!(refs[0].uri, "file:///tmp/transcript.json");
}

#[test]
fn artifact_refs_omit_artifacts_with_empty_url_and_path() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![
            artifact_ref_artifact(
                "dir-empty",
                "sample-runtime-artifact-directory",
                Some(""),
                Some(""),
            ),
            artifact_ref_artifact("dir-none", "sample-runtime-agent-task-input", None, None),
            artifact_ref_artifact("patch", "patch", None, Some("/tmp/patch.diff")),
        ],
        Vec::new(),
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(refs.len(), 1, "artifacts lacking a usable uri are dropped");
    assert_eq!(refs[0].kind, "patch");
    assert_eq!(refs[0].uri, "/tmp/patch.diff");
}

#[test]
fn artifact_refs_treat_empty_url_as_missing_and_fall_back_to_path() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![artifact_ref_artifact(
            "dir",
            "sample-runtime-artifact-directory",
            Some("   "),
            Some("/tmp/artifacts/dir"),
        )],
        Vec::new(),
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].uri, "/tmp/artifacts/dir");
}

#[test]
fn artifact_refs_dedup_identical_refs_across_artifacts_and_evidence() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![artifact_ref_artifact(
            "transcript",
            "transcript",
            Some("file:///tmp/transcript.json"),
            None,
        )],
        vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("transcript artifact".to_string()),
        }],
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(
        refs.len(),
        1,
        "exact-duplicate refs collapse to a single entry"
    );
}

#[test]
fn status_filters_empty_uri_artifact_refs() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                succeeded: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![outcome_with_refs(
                "task-a",
                vec![
                    artifact_ref_artifact(
                        "dir-empty",
                        "sample-runtime-artifact-directory",
                        Some(""),
                        None,
                    ),
                    artifact_ref_artifact("patch", "patch", None, Some("/tmp/patch.diff")),
                ],
                vec![
                    AgentTaskEvidenceRef {
                        kind: "sample-runtime-command-log".to_string(),
                        uri: "".to_string(),
                        label: Some("command log".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "transcript".to_string(),
                        uri: "file:///tmp/transcript.json".to_string(),
                        label: Some("provider transcript".to_string()),
                    },
                ],
            )],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };

        let record =
            record_completed_run(&plan, &aggregate, Some("run-empty-refs")).expect("recorded");
        let durable_status = status(&record.run_id).expect("status");

        let uris: Vec<&str> = durable_status
            .artifact_refs
            .iter()
            .map(|r| r.uri.as_str())
            .collect();
        assert!(
            uris.iter().all(|uri| !uri.is_empty()),
            "no empty-URI refs leak into status output: {uris:?}"
        );
        let kinds: Vec<&str> = durable_status
            .artifact_refs
            .iter()
            .map(|r| r.kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["patch", "transcript"]);
    });
}

fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "plan-a",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }],
    )
}

fn succeeded_aggregate(plan: &AgentTaskPlan) -> AgentTaskAggregate {
    AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Succeeded,
        totals: AgentTaskAggregateTotals {
            queued: 1,
            succeeded: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }],
        events: vec![AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Succeeded,
            attempt: 1,
            message: Some("ok".to_string()),
        }],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    }
}
