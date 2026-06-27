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
fn execute_controller_action_fails_closed_for_unimplemented_runner_target() {
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
        .expect("runner target fails closed");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status.as_deref(), Some("failed"));
        let execution = result.value.execution.as_ref().expect("execution result");
        assert_eq!(
            execution
                .pointer("/diagnostics/0/code")
                .and_then(Value::as_str),
            Some("runner_action_dispatch_unimplemented")
        );
        assert_eq!(
            execution
                .pointer("/diagnostics/0/runner")
                .and_then(Value::as_str),
            Some("homeboy-lab")
        );
        let persisted_action = result
            .value
            .controller
            .next_actions
            .iter()
            .find(|candidate| candidate.action_id == action.action_id)
            .expect("action persisted");
        assert_eq!(persisted_action.status, AgentTaskLoopActionStatus::Failed);
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
