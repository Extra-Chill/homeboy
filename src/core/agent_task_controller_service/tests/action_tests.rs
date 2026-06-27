use super::super::*;
use super::*;

#[test]
fn runtime_bundle_action_result_exposes_budgets_and_observed_wait_outcome() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-runtime-budget-evidence".to_string(),
            phase: "generate".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        let workflow_context = json!({
            "schema": "homeboy/repo-loop-workflow-context/v1",
            "workflow_id": "bundle-generation",
            "runtime_task": {
                "kind": "bundle",
                "ability": "runtime-package/run",
                "input": {
                    "package": { "source": "bundles/site-generator" },
                    "time_budget_ms": 900000,
                    "step_budget": 42,
                    "input": {
                        "wait_for_completion": true,
                        "drain_budget_ms": 300000
                    }
                }
            },
            "plan": {
                "schema": "homeboy/repo-loop-workflow-plan/v1",
                "artifacts": [{
                    "id": "generated_candidate",
                    "artifact_type": "runtime-candidate",
                    "data": {
                        "kind": "runtime-candidate",
                        "required": true
                    }
                }]
            }
        });
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:bundle-generation".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "run_id": "controller-service-runtime-budget",
                    "dispatch": {
                        "client_context": workflow_context.to_string()
                    }
                }),
            },
            "run runtime bundle",
        );
        controller::write_controller(&record).expect("controller written");

        let result = resume(
            "loop-service-runtime-budget-evidence",
            CapturingExecutor::default(),
            &RuntimeBudgetDispatchHook,
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 1);
        let failed = &result.value.results[0];
        assert_eq!(failed["status"], json!("failed"));
        assert_eq!(
            failed["execution"]["runtime_bundle"]["configured"]["tasks"][0]["budgets"]
                ["time_budget_ms"],
            json!(900000)
        );
        assert_eq!(
            failed["execution"]["runtime_bundle"]["configured"]["tasks"][0]["budgets"]
                ["step_budget"],
            json!(42)
        );
        assert_eq!(
            failed["execution"]["runtime_bundle"]["configured"]["tasks"][0]["budgets"]
                ["drain_budget_ms"],
            json!(300000)
        );
        let observed = &failed["execution"]["runtime_bundle"]["observed"]["results"];
        assert!(observed
            .as_array()
            .expect("observed results")
            .iter()
            .any(|entry| {
                entry["wait_result"]["terminal_state"] == json!("timeout")
                    && entry["wait_result"]["steps_drained"] == json!(12)
                    && entry["wait_result"]["actions_drained"] == json!(7)
                    && entry["wait_result"]["elapsed_ms"] == json!(300123)
            }));
        assert!(observed
            .as_array()
            .expect("observed results")
            .iter()
            .any(|entry| {
                entry["job_status"] == json!("running")
                    && entry["completion_outcome"] == json!("wait_budget_expired")
                    && entry["error_type"] == json!("runtime_wait_timeout")
                    && entry["classification"] == json!("runtime_incomplete")
            }));
        assert_eq!(
            failed["execution"]["result"]["aggregate"]["outcomes"][0]["outputs"]
                ["provider_run_result"]["wait_result"]["terminal_state"],
            json!("timeout")
        );
    });
}

#[test]
fn run_action_executes_only_requested_action_id() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-action".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        record.record_action(
            AgentTaskLoopPolicyAction::WaitForEvent(AgentTaskLoopWait {
                wait_key: "wait-a".to_string(),
                event_type: "task.completed".to_string(),
                entity_id: None,
                external_ref: None,
                timeout_at: None,
                escalation_policy: None,
                status: AgentTaskLoopWaitStatus::Open,
                satisfied_by_event_id: None,
            }),
            "wait first",
        );
        record.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("done".to_string()),
            },
            "complete second",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_action(
            "loop-service-action",
            "action-2",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("action executed");

        assert_eq!(result.exit_code, 0);
        assert!(result.value.claimed);
        let loaded = controller::load_controller("loop-service-action").expect("controller");
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Pending
        );
        assert_eq!(
            loaded.next_actions[1].status,
            AgentTaskLoopActionStatus::Completed
        );
    });
}

#[test]
fn retry_action_queues_durable_retry_and_records_parent_lineage() {
    with_isolated_home(|_| {
        crate::core::agent_task_lifecycle::submit_plan(&test_plan(), Some("retry-source-run"))
            .expect("source run submitted");
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-retry".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.record_action(
            AgentTaskLoopPolicyAction::Retry {
                target_run_id: "retry-source-run".to_string(),
            },
            "red gate requested retry",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-retry",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("retry action executed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status.as_deref(), Some("completed"));
        let retry_run_id = result
            .value
            .execution
            .as_ref()
            .and_then(|execution| execution.get("retry_run_id"))
            .and_then(Value::as_str)
            .expect("retry run id returned")
            .to_string();
        let retry_record =
            crate::core::agent_task_lifecycle::status(&retry_run_id).expect("retry record exists");
        assert_eq!(retry_record.metadata["retry_of"], json!("retry-source-run"));

        let loaded = controller::load_controller("loop-service-retry").expect("controller");
        assert_eq!(loaded.task_lineage.len(), 1);
        assert_eq!(loaded.task_lineage[0].run_id, retry_run_id);
        assert_eq!(
            loaded.task_lineage[0].parent_run_id.as_deref(),
            Some("retry-source-run")
        );
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.retry_queued"));
    });
}

#[test]
fn request_changes_action_records_normalized_feedback() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-request-changes".to_string(),
            phase: "review".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.record_action(
            AgentTaskLoopPolicyAction::RequestChanges {
                target_run_id: "candidate-run-1".to_string(),
                feedback_id: Some("review-feedback-1".to_string()),
            },
            "review requested changes",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-request-changes",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("request changes action executed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status.as_deref(), Some("completed"));
        let loaded =
            controller::load_controller("loop-service-request-changes").expect("controller");
        assert_eq!(loaded.feedback.len(), 1);
        assert_eq!(loaded.feedback[0].feedback_id, "review-feedback-1");
        assert_eq!(
            loaded.feedback[0].target_run_id.as_deref(),
            Some("candidate-run-1")
        );
        assert_eq!(
            loaded.feedback[0].status,
            controller::AgentTaskLoopFeedbackStatus::ChangesRequested
        );
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.changes_requested"));
    });
}

#[test]
fn complete_action_transitions_only_when_executed() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-complete".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        record.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("done".to_string()),
            },
            "complete",
        );
        assert_eq!(record.state, AgentTaskLoopControllerState::Running);
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-complete",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("complete action executed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Completed
        );
        assert_eq!(result.value.status.as_deref(), Some("completed"));
    });
}

#[test]
fn route_finding_action_spawns_materialized_plan() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-route-finding".to_string(),
            phase: "triage".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        let plan = test_plan();
        record.record_action(
            AgentTaskLoopPolicyAction::RouteFinding {
                finding: AgentTaskLoopFindingPacket {
                    finding_id: "finding-1".to_string(),
                    severity: "high".to_string(),
                    summary: "broken".to_string(),
                    owner: None,
                    source_transformer: None,
                    reproduction_key: Some("repo-loop-gap".to_string()),
                    lineage: Vec::new(),
                    payload: Value::Null,
                },
                dedupe_key: "finding:repo-loop-gap".to_string(),
                entity_id: None,
                request_template: json!({
                    "mode": "run_plan",
                    "run_id": "controller-service-route-finding-a",
                    "plan": plan,
                }),
            },
            "route finding",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-route-finding",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("route finding executed");

        assert_eq!(result.exit_code, 0);
        let loaded = controller::load_controller("loop-service-route-finding").expect("controller");
        assert!(loaded.entities.contains_key("finding:repo-loop-gap"));
        assert_eq!(
            loaded.task_lineage[0].run_id,
            "controller-service-route-finding-a"
        );
    });
}

#[test]
fn apply_event_persists_actions_and_keeps_event_envelope() {
    with_isolated_home(|_| {
        init(ControllerInitRequest {
            loop_id: "loop-service-event".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        let report = apply_event(ControllerApplyEventRequest {
            loop_id: "loop-service-event".to_string(),
            event_type: "task.completed".to_string(),
            event_id: None,
            event_key: None,
            entity_id: None,
            payload: Value::Null,
        })
        .expect("event applied");

        assert_eq!(report.schema, APPLY_EVENT_RESULT_SCHEMA);
        assert!(!report
            .controller
            .history
            .iter()
            .all(|event| event.event_type == "controller.action.claimed"));
    });
}

#[test]
fn controller_escalates_when_action_cap_is_reached() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-action-cap".to_string(),
            phase: "verify".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        // Fill the controller to its lifetime action cap with non-dedupable
        // actions, mirroring a stuck loop that keeps recording follow-ups.
        for index in 0..MAX_CONTROLLER_LIFETIME_ACTIONS {
            record.record_action(
                AgentTaskLoopPolicyAction::Retry {
                    target_run_id: format!("run-{index}"),
                },
                "stuck loop follow-up",
            );
        }
        assert_eq!(record.next_actions.len(), MAX_CONTROLLER_LIFETIME_ACTIONS);
        controller::write_controller(&record).expect("controller written");

        // The next execution must not claim and run another action: it escalates.
        let result = run_next(
            "loop-service-action-cap",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("cap guard returns a report");
        assert_eq!(result.exit_code, 1);
        assert!(!result.value.claimed);
        assert_eq!(result.value.status.as_deref(), Some("escalated"));

        let loaded =
            controller::load_controller("loop-service-action-cap").expect("controller loaded");
        assert_eq!(loaded.state, AgentTaskLoopControllerState::Escalated);
        assert_eq!(loaded.terminal_outcomes.len(), 1);
        assert_eq!(
            loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::Failed
        );
        assert!(loaded
            .next_actions
            .iter()
            .all(|action| action.status == AgentTaskLoopActionStatus::Pending));

        // Re-running against an already-escalated controller is idempotent: it
        // claims no further action, so growth halts and no duplicate terminal
        // outcomes are appended.
        let again = run_next(
            "loop-service-action-cap",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("escalated controller returns a report again");
        assert_eq!(again.exit_code, 0);
        assert!(!again.value.claimed);
        let reloaded =
            controller::load_controller("loop-service-action-cap").expect("controller reloaded");
        assert_eq!(reloaded.terminal_outcomes.len(), 1);
        assert_eq!(reloaded.next_actions.len(), MAX_CONTROLLER_LIFETIME_ACTIONS);
    });
}
