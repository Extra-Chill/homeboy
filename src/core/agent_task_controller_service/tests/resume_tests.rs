//! Resume, gate terminal outcomes, and per-action controller tests.
use super::super::*;
use super::common::*;
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskTypedArtifact,
    AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_loop_controller::{
    AgentTaskGateBundle, AgentTaskGateBundleCheck, AgentTaskGateBundleCheckKind,
    AgentTaskGateBundleStatus, AgentTaskLoopFindingPacket, AgentTaskLoopPolicyAction,
    AgentTaskLoopTerminalStatus, AgentTaskLoopWait, AgentTaskLoopWaitStatus,
    DEFAULT_FAN_OUT_MAX_ITEMS,
};
use crate::core::agent_task_scheduler::AgentTaskExecutionContext;
use crate::test_support::with_isolated_home;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn run_gates_records_generic_terminal_outcomes() {
    with_isolated_home(|_| {
        let mut passed = init(ControllerInitRequest {
            loop_id: "loop-service-gate-passed".to_string(),
            phase: "verify".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        passed.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "green".to_string(),
            description: String::new(),
            checks: Vec::new(),
        });
        passed.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "green".to_string(),
                entity_id: None,
            },
            "run green gate",
        );
        controller::write_controller(&passed).expect("passed controller written");

        let passed_result = run_next(
            "loop-service-gate-passed",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gate action executed");
        assert_eq!(passed_result.exit_code, 0);
        let passed_loaded = controller::load_controller("loop-service-gate-passed")
            .expect("passed controller loaded");
        assert_eq!(
            passed_loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::Passed
        );

        let mut blocked = init(ControllerInitRequest {
            loop_id: "loop-service-gate-blocked".to_string(),
            phase: "verify".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        blocked.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "red".to_string(),
            description: String::new(),
            checks: vec![AgentTaskGateBundleCheck {
                check_id: "api-check".to_string(),
                kind: AgentTaskGateBundleCheckKind::Api,
                input: Value::Null,
                retryable: false,
            }],
        });
        blocked.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "red".to_string(),
                entity_id: None,
            },
            "run red gate",
        );
        controller::write_controller(&blocked).expect("blocked controller written");

        let blocked_result = run_next(
            "loop-service-gate-blocked",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gate action executed");
        assert_eq!(blocked_result.exit_code, 1);
        let blocked_loaded = controller::load_controller("loop-service-gate-blocked")
            .expect("blocked controller loaded");
        assert_eq!(
            blocked_loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::BlockedByGate
        );
    });
}

#[test]
fn resume_fails_required_workflow_artifact_handoff_before_downstream_action() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-required-handoff".to_string(),
            phase: "collect".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        let workflow_context = json!({
            "schema": "homeboy/repo-loop-workflow-context/v1",
            "workflow_id": "finding-packets",
            "plan": {
                "schema": "homeboy/repo-loop-workflow-plan/v1",
                "artifacts": [{
                    "id": "finding_groups",
                    "artifact_type": "finding-packets",
                    "data": {
                        "kind": "finding-packets",
                        "required": true
                    }
                }]
            }
        });
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:finding-packets".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "run_id": "controller-service-required-handoff-a",
                    "dispatch": {
                        "client_context": workflow_context.to_string()
                    }
                }),
            },
            "produce finding packets",
        );
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:iterator".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "run_id": "controller-service-required-handoff-b",
                }),
            },
            "consume finding packets",
        );
        controller::write_controller(&record).expect("controller written");

        let result = resume(
            "loop-service-required-handoff",
            CapturingExecutor::default(),
            &CapturingDispatchHook::default(),
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.results.len(), 1);
        let loaded = controller::load_controller("loop-service-required-handoff")
            .expect("controller loaded");
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Failed
        );
        assert_eq!(
            loaded.next_actions[1].status,
            AgentTaskLoopActionStatus::Pending
        );
        assert_eq!(
            loaded.next_actions[0].diagnostics[0].code,
            "required_workflow_artifacts_missing"
        );
        assert_eq!(
            loaded.next_actions[0].diagnostics[0].details["missing_artifacts"][0]["artifact_id"],
            json!("finding_groups")
        );
        assert_eq!(
            loaded.next_actions[0].diagnostics[0].details["missing_artifacts"][0]["kind"],
            json!("finding-packets")
        );
        assert!(loaded.history.iter().any(|event| {
            event.event_type == "controller.action.failed"
                && event.payload["diagnostics"][0]["code"]
                    == json!("required_workflow_artifacts_missing")
        }));
    });
}

#[test]
fn resume_failed_action_result_includes_top_level_failure_summary() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-failure-summary".to_string(),
            phase: "prepare".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        let workflow_context = json!({
            "schema": "homeboy/repo-loop-workflow-context/v1",
            "workflow_id": "scope-runtime",
        });
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:scope-runtime".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "dispatch": {
                        "client_context": workflow_context.to_string()
                    }
                }),
            },
            "prepare runtime overlay",
        );
        controller::write_controller(&record).expect("controller written");

        let result = resume(
            "loop-service-failure-summary",
            CapturingExecutor::default(),
            &FailingDispatchHook,
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.results.len(), 1);
        let failed = &result.value.results[0];
        assert_eq!(failed["status"], json!("failed"));
        assert_eq!(failed["failure_summary"]["action_id"], failed["action_id"]);
        assert_eq!(
            failed["failure_summary"]["dedupe_key"],
            json!("workflow:scope-runtime")
        );
        assert_eq!(
            failed["failure_summary"]["workflow_id"],
            json!("scope-runtime")
        );
        assert_eq!(
            failed["failure_summary"]["run_id"],
            json!("generic-run-overlay")
        );
        assert_eq!(
            failed["failure_summary"]["task_id"],
            json!("task-overlay-prepare")
        );
        assert_eq!(failed["failure_summary"]["phase"], json!("prepare"));
        assert_eq!(
            failed["failure_summary"]["provider"],
            json!("synthetic-runtime")
        );
        assert_eq!(
            failed["failure_summary"]["failure_phase"],
            json!("runtime_overlay_preparation")
        );
        assert_eq!(
            failed["failure_summary"]["diagnostic"],
            json!("Recipe runtime overlay preparation failed: download php-scoper timed out after 60004ms")
        );
        assert_eq!(
            failed["execution"]["result"]["aggregate"]["outcomes"][0]["diagnostics"][0]["message"],
            failed["failure_summary"]["diagnostic"]
        );
    });
}

#[test]
fn resume_with_options_stops_at_max_actions_with_pending_work_remaining() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-bounded-resume".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        for index in 0..3 {
            record.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: format!("finding:{index}:repair"),
                    entity_id: None,
                    request: json!({
                        "mode": "run_plan",
                        "run_id": format!("controller-service-bounded-{index}"),
                        "plan": test_plan(),
                    }),
                },
                "finding emitted",
            );
        }
        controller::write_controller(&record).expect("controller written");

        let result = resume_with_options(
            "loop-service-bounded-resume",
            CapturingExecutor::default(),
            &NoopDispatchHook,
            ControllerResumeOptions {
                max_actions: 2,
                stop_on_terminal: true,
            },
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 0);
        assert!(result.value.claimed);
        assert_eq!(result.value.stopped_reason, "max_actions_reached");
        assert_eq!(result.value.results.len(), 2);

        let loaded =
            controller::load_controller("loop-service-bounded-resume").expect("controller loaded");
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.next_actions[1].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.next_actions[2].status,
            AgentTaskLoopActionStatus::Pending
        );
    });
}

#[test]
fn resume_with_options_stops_after_terminal_state() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-terminal-resume".to_string(),
            phase: "finalize".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("done".to_string()),
            },
            "complete loop",
        );
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:after-terminal".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "run_plan",
                    "run_id": "controller-service-after-terminal",
                    "plan": test_plan(),
                }),
            },
            "should not run after terminal state",
        );
        controller::write_controller(&record).expect("controller written");

        let result = resume_with_options(
            "loop-service-terminal-resume",
            CapturingExecutor::default(),
            &NoopDispatchHook,
            ControllerResumeOptions {
                max_actions: 10,
                stop_on_terminal: true,
            },
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 0);
        assert!(result.value.claimed);
        assert_eq!(result.value.stopped_reason, "terminal_state");
        assert_eq!(result.value.results.len(), 1);
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Completed
        );

        let loaded =
            controller::load_controller("loop-service-terminal-resume").expect("controller loaded");
        assert_eq!(loaded.state, AgentTaskLoopControllerState::Completed);
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.next_actions[1].status,
            AgentTaskLoopActionStatus::Pending
        );
    });
}

#[test]
fn completed_typed_artifacts_are_carried_to_later_required_workflow_artifacts() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-typed-artifact-handoff".to_string(),
            phase: "generate".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        let workflow_context = json!({
            "schema": "homeboy/repo-loop-workflow-context/v1",
            "workflow_id": "import-static-site",
            "plan": {
                "schema": "homeboy/repo-loop-workflow-plan/v1",
                "artifacts": [{
                    "id": "static_site_candidate",
                    "artifact_type": "static_site",
                    "required": true
                }]
            }
        });
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:build-static-site".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "run_id": "typed-artifact-handoff-producer"
                }),
            },
            "produce static site candidate",
        );
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:import-static-site".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "run_id": "typed-artifact-handoff-consumer",
                    "dispatch": {
                        "client_context": workflow_context.to_string()
                    }
                }),
            },
            "consume static site candidate",
        );
        controller::write_controller(&record).expect("controller written");

        let dispatch = TypedArtifactHandoffDispatchHook::default();
        let result = resume(
            "loop-service-typed-artifact-handoff",
            CapturingExecutor::default(),
            &dispatch,
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.results.len(), 2);
        let loaded = controller::load_controller("loop-service-typed-artifact-handoff")
            .expect("controller loaded");
        assert!(loaded
            .next_actions
            .iter()
            .all(|action| action.status == AgentTaskLoopActionStatus::Completed));
        assert!(loaded
            .next_actions
            .iter()
            .all(|action| action.diagnostics.is_empty()));
        assert_eq!(
            result.value.results[1]["execution"]["workflow_artifacts"][0]["name"],
            json!("static_site_candidate")
        );

        let observed = dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(observed.len(), 2);
        assert_eq!(
            observed[1]["workflow_artifacts"][0]["type"],
            json!("static_site")
        );
        assert_eq!(
            observed[1]["dispatch"]["workflow_artifacts"][0]["name"],
            json!("static_site_candidate")
        );
    });
}

#[test]
fn resume_recovers_running_action_with_stale_child_run() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-stale-child".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        let plan = test_plan();
        crate::core::agent_task_lifecycle::submit_plan(
            &plan,
            Some("controller-service-stale-child-a"),
        )
        .expect("child submitted");
        crate::core::agent_task_lifecycle::mark_running("controller-service-stale-child-a")
            .expect("child marked running");
        crate::core::agent_task_lifecycle::rewrite_record_for_test(
            "controller-service-stale-child-a",
            |record| {
                record.metadata["runner_pid"] = json!(999999u32);
            },
        )
        .expect("stale child status written");

        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "run_plan",
                    "run_id": "controller-service-stale-child-a",
                    "plan": plan,
                }),
            },
            "finding emitted",
        );
        record.next_actions[0].status = AgentTaskLoopActionStatus::Running;
        record
            .dedupe_keys
            .get_mut("finding:abc:repair")
            .expect("dedupe record")
            .run_id = Some("controller-service-stale-child-a".to_string());
        controller::write_controller(&record).expect("controller written");

        let result = resume(
            "loop-service-stale-child",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.results.len(), 1);
        let loaded = controller::load_controller("loop-service-stale-child").expect("controller");
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.next_actions[0].diagnostics[0].code,
            "stale_child_run_recovery"
        );
        assert!(loaded.history.iter().any(|event| {
            event.event_type == "controller.action.stale_child_recovery"
                && event.payload["run_id"] == json!("controller-service-stale-child-a")
        }));
        let child = crate::core::agent_task_lifecycle::status("controller-service-stale-child-a")
            .expect("child status");
        assert_eq!(child.metadata["reclaimed_stale_running"], json!(true));
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
fn resume_stops_when_wait_action_blocks_controller() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-wait-stop".to_string(),
            phase: "delegate".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.record_action(
            AgentTaskLoopPolicyAction::WaitForController {
                loop_id: "missing-child".to_string(),
                entity_id: None,
                wait_key: None,
                terminal_states: Vec::new(),
            },
            "wait for child",
        );
        record.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("must not run yet".to_string()),
            },
            "complete after wait",
        );
        record.state = AgentTaskLoopControllerState::Running;
        controller::write_controller(&record).expect("controller written");

        let result = resume(
            "loop-service-wait-stop",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.results.len(), 1);
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Waiting
        );
        assert_eq!(
            result.value.controller.next_actions[1].status,
            AgentTaskLoopActionStatus::Pending
        );
    });
}

#[test]
fn wait_for_controller_resumes_after_child_terminal_state() {
    with_isolated_home(|_| {
        let mut parent = init(ControllerInitRequest {
            loop_id: "loop-service-parent".to_string(),
            phase: "delegate".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("parent initialized");
        parent.record_action(
            AgentTaskLoopPolicyAction::WaitForController {
                loop_id: "loop-service-child".to_string(),
                entity_id: None,
                wait_key: None,
                terminal_states: Vec::new(),
            },
            "wait for child",
        );
        parent.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("child done".to_string()),
            },
            "complete after child",
        );
        parent.state = AgentTaskLoopControllerState::Running;
        controller::write_controller(&parent).expect("parent written");

        let mut child = init(ControllerInitRequest {
            loop_id: "loop-service-child".to_string(),
            phase: "work".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("child initialized");
        child.state = AgentTaskLoopControllerState::Completed;
        controller::write_controller(&child).expect("child written");

        let result = resume(
            "loop-service-parent",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("controller resumed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.results.len(), 2);
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Completed
        );
    });
}
