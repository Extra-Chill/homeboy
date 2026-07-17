use super::super::*;
use super::*;

#[test]
fn plan_from_spec_projects_controller_actions_without_writing_state() {
    with_isolated_home(|_| {
        let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
            "loop_id": "controller-plan-fixture",
            "phase": "review",
            "agents": {
                "builder": {
                    "role": "builder",
                    "tools": ["patch"]
                }
            },
            "tools": {
                "patch": {
                    "description": "produce a patch",
                    "input_schema": {"type": "object"}
                }
            },
            "workflows": {
                "build-candidate": {
                    "agent_id": "builder",
                    "prompt": "Build a candidate patch.",
                    "artifacts": ["patch-artifact"],
                    "gates": ["tests"]
                }
            },
            "artifacts": {
                "patch-artifact": {
                    "kind": "patch",
                    "required": true
                }
            },
            "gates": {
                "tests": {
                    "description": "tests pass"
                }
            },
            "gate_bundles": [{
                "bundle_id": "review-gates",
                "status": "pending",
                "checks": []
            }],
            "phases": [{
                "phase": "review",
                "actions": [{
                    "action": "run_gates",
                    "bundle_id": "review-gates",
                    "entity_id": "candidate:1"
                }]
            }]
        }))
        .expect("spec parses");

        let report = plan_from_spec(ControllerPlanRequest { spec }).expect("plan compiles");

        assert_eq!(report.schema, PLAN_RESULT_SCHEMA);
        assert_eq!(report.loop_id, "controller-plan-fixture");
        assert_eq!(report.plan.kind, PlanKind::Controller);
        assert_eq!(report.plan.mode.as_deref(), Some("plan"));
        assert_eq!(report.actions.len(), 2);
        assert_eq!(report.plan.steps.len(), 2);
        assert!(report
            .plan
            .steps
            .iter()
            .any(|step| step.kind == "controller.spawn_task"));
        assert!(report
            .plan
            .steps
            .iter()
            .any(|step| step.kind == "controller.run_gates"));
        assert!(report.spec_fingerprint.starts_with("sha256:"));
        assert!(controller::load_controller("controller-plan-fixture").is_err());
    });
}

#[test]
fn execute_controller_action_marks_blocked_no_local_fallback_policy() {
    with_isolated_home(|_| {
        let mut record =
            AgentTaskLoopControllerRecord::new("loop-runner-blocked", "dispatch", "v1");
        let action = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "task:no-local".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "local_fallback": false
                }),
            },
            "policy matched",
        );
        let executor = CapturingExecutor::default();
        let dispatch = CapturingDispatchHook::default();

        let result = execute_controller_action_with_runner_availability(
            &mut record,
            &action.action_id,
            executor.clone(),
            &dispatch,
            |_| unreachable!("no runner should not probe availability"),
        )
        .expect("policy block succeeds");

        assert_eq!(result.exit_code, 1);
        assert_eq!(
            result.value.status.as_deref(),
            Some("blocked_local_fallback_denied")
        );
        let persisted_action = result
            .value
            .controller
            .next_actions
            .iter()
            .find(|candidate| candidate.action_id == action.action_id)
            .expect("action persisted");
        assert_eq!(
            persisted_action.status,
            AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
        );
        assert_eq!(persisted_action.diagnostics.len(), 1);
        assert_eq!(
            persisted_action.diagnostics[0].code,
            "blocked_local_fallback_denied"
        );
        assert!(executor
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none());
        assert!(dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
    });
}

#[test]
fn execute_controller_action_dispatches_an_available_runner_target() {
    with_isolated_home(|_| {
        let mut record = AgentTaskLoopControllerRecord::new("loop-runner-target", "dispatch", "v1");
        let action = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "task:runner".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "runner": "homeboy-lab",
                    "local_fallback": false
                }),
            },
            "policy matched",
        );
        let executor = CapturingExecutor::default();
        let dispatch = CapturingDispatchHook::default();

        let result = execute_controller_action_with_runner_availability(
            &mut record,
            &action.action_id,
            executor.clone(),
            &dispatch,
            |runner| {
                assert_eq!(runner, "homeboy-lab");
                AgentTaskLoopRunnerAvailability::Available
            },
        )
        .expect("runner target dispatches");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status.as_deref(), Some("completed"));
        let persisted_action = result
            .value
            .controller
            .next_actions
            .iter()
            .find(|candidate| candidate.action_id == action.action_id)
            .expect("action persisted");
        assert_eq!(
            persisted_action.status,
            AgentTaskLoopActionStatus::Completed
        );
        assert!(executor
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none());
        let dispatches = dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(dispatches.len(), 1);
        assert_eq!(dispatches[0]["runner"], "homeboy-lab");
        assert!(dispatches[0]
            .get("dispatch")
            .unwrap_or(&dispatches[0])
            .get("run_id")
            .and_then(Value::as_str)
            .expect("controller run identity")
            .starts_with("controller-loop-runner-target-action-1-task_runner"));
    });
}

#[test]
fn execute_controller_dispatch_injects_action_scoped_identity() {
    with_isolated_home(|_| {
        let mut record = AgentTaskLoopControllerRecord::new("loop-dispatch-identity", "init", "v1");
        let action = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:store-idea".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "dispatch": {
                        "prompt": "Produce a concept packet.",
                        "repo": "wp-site-generator"
                    }
                }),
            },
            "repo loop workflow",
        );
        let dispatch = CapturingDispatchHook::default();

        let result = execute_controller_action_with_runner_availability(
            &mut record,
            &action.action_id,
            CapturingExecutor::default(),
            &dispatch,
            |_| unreachable!("dispatch action has no runner policy"),
        )
        .expect("dispatch action runs");

        assert_eq!(result.exit_code, 0);
        let observed = dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = observed.first().expect("observed dispatch request");
        let run_id = request["dispatch"]["run_id"].as_str().expect("run id");
        let task_id = request["dispatch"]["task_id"].as_str().expect("task id");
        assert!(
            run_id.starts_with("controller-loop-dispatch-identity-action-1-workflow_store-idea")
        );
        assert!(task_id
            .starts_with("controller-task-loop-dispatch-identity-action-1-workflow_store-idea"));
        assert_eq!(request["dispatch"]["repo"], "wp-site-generator");
    });
}

#[test]
fn accepted_lab_runner_handoff_waits_and_terminal_replay_is_idempotent() {
    with_isolated_home(|_| {
        let mut record = AgentTaskLoopControllerRecord::new("loop-runner-replay", "dispatch", "v1");
        let action = record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "task:runner-replay".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "runner": "homeboy-lab",
                    "local_fallback": false,
                    "dispatch": { "prompt": "Run remotely." },
                }),
            },
            "policy matched",
        );
        let run_id = "controller-loop-runner-replay-action-1-task_runner-replay";
        lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("persisted run");
        lifecycle::record_detached_lab_run(lifecycle::DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-8515",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .expect("accepted runner job binds the persisted run");

        let accepted = execute_controller_action_with_runner_availability(
            &mut record,
            &action.action_id,
            CapturingExecutor::default(),
            &LabRunnerHandoffDispatchHook,
            |_| AgentTaskLoopRunnerAvailability::Available,
        )
        .expect("runner accepts handoff");
        assert_eq!(accepted.exit_code, 0);
        assert_eq!(accepted.value.status.as_deref(), Some("waiting_for_runner"));
        assert_eq!(
            accepted.value.controller.next_actions[0].status,
            AgentTaskLoopActionStatus::WaitingForRunner
        );
        assert_eq!(
            accepted.value.controller.next_actions[0].diagnostics[0].details["identity"]
                ["runner_job_id"],
            "job-8515"
        );

        lifecycle::cancel(run_id).expect("terminal runner run");
        controller::write_controller(&accepted.value.controller).expect("persist waiting action");
        let payload = json!({
            "identity": {
                "run_id": run_id,
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-8515",
            },
        });
        let first = apply_event(ControllerApplyEventRequest {
            loop_id: "loop-runner-replay".to_string(),
            event_type: "agent_task.runner_terminal".to_string(),
            event_id: Some("runner-job-8515-terminal".to_string()),
            event_key: None,
            entity_id: None,
            payload: payload.clone(),
        })
        .expect("terminal replay projects cancellation");
        assert_eq!(
            first.controller.next_actions[0].status,
            AgentTaskLoopActionStatus::Cancelled
        );
        let projected = first
            .controller
            .history
            .iter()
            .filter(|event| event.event_type == "controller.action.runner_terminal_projected")
            .count();

        let duplicate = apply_event(ControllerApplyEventRequest {
            loop_id: "loop-runner-replay".to_string(),
            event_type: "agent_task.runner_terminal".to_string(),
            event_id: Some("runner-job-8515-terminal-duplicate".to_string()),
            event_key: None,
            entity_id: None,
            payload,
        })
        .expect("duplicate terminal replay is idempotent");
        assert_eq!(
            duplicate.controller.next_actions[0].status,
            AgentTaskLoopActionStatus::Cancelled
        );
        assert_eq!(
            duplicate
                .controller
                .history
                .iter()
                .filter(|event| event.event_type == "controller.action.runner_terminal_projected")
                .count(),
            projected
        );
    });
}

#[test]
fn accepted_lab_runner_handoff_rejects_an_unbound_persisted_identity() {
    with_isolated_home(|_| {
        let run_id = "controller-unbound-lab-runner-handoff";
        lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("persisted queued run");

        let error = accepted_lab_runner_handoff_identity(&json!({
            "schema": "homeboy/agent-task-controller-lab-handoff/v1",
            "run_id": run_id,
            "identity": {
                "run_id": run_id,
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-8515",
            },
        }))
        .expect_err("handoff must bind the persisted run to the same runner job");

        assert!(error.message.contains("persisted run/runner/job binding"));
    });
}
