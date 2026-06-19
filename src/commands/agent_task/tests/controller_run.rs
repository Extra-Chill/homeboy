//! Agent-task command controller run/run-next execution tests.

use super::support::*;

#[test]
fn controller_run_next_executes_spawn_task_plan_and_records_dedupe_lineage() {
    with_temp_home(|| {
        let observed_request = Arc::new(Mutex::new(None));
        let mut controller = agent_task_loop_controller::create_controller(
            "loop-controller-run-next",
            "repair",
            "v1",
        )
        .expect("controller created");
        let mut plan = test_plan();
        plan.tasks[0].executor.selector = Some("homeboy-lab".to_string());
        plan.tasks[0].executor.config = json!({
            "artifact_root": "/tmp/homeboy-lab-artifacts/controller-run-next"
        });

        controller.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "run_plan",
                    "run_id": "controller-run-next-a",
                    "plan": plan,
                }),
            },
            "finding emitted",
        );
        agent_task_loop_controller::write_controller(&controller).expect("controller written");

        let (value, exit_code) = controller_run_next_with_executor(
            "loop-controller-run-next".to_string(),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("controller action executed");

        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");
        let loaded = agent_task_loop_controller::load_controller("loop-controller-run-next")
            .expect("controller loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["claimed"], true);
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.dedupe_keys["finding:abc:repair"].run_id.as_deref(),
            Some("controller-run-next-a")
        );
        assert_eq!(loaded.task_lineage[0].run_id, "controller-run-next-a");
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.claimed"));
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.completed"));
        assert_eq!(observed.executor.selector.as_deref(), Some("homeboy-lab"));
        assert_eq!(
            observed.executor.config["artifact_root"],
            json!("/tmp/homeboy-lab-artifacts/controller-run-next")
        );
    });
}

#[test]
fn controller_run_executes_requested_action_id_only() {
    with_temp_home(|| {
        let mut controller = agent_task_loop_controller::create_controller(
            "loop-controller-run-action",
            "repair",
            "v1",
        )
        .expect("controller created");
        controller.record_action(
            AgentTaskLoopPolicyAction::WaitForEvent(
                agent_task_loop_controller::AgentTaskLoopWait {
                    wait_key: "wait-a".to_string(),
                    event_type: "task.completed".to_string(),
                    entity_id: None,
                    external_ref: None,
                    timeout_at: None,
                    escalation_policy: None,
                    status: agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                    satisfied_by_event_id: None,
                },
            ),
            "wait first",
        );
        controller.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("done".to_string()),
            },
            "complete second",
        );
        agent_task_loop_controller::write_controller(&controller).expect("controller written");

        let (_value, exit_code) = controller_run_action_with_executor(
            "loop-controller-run-action".to_string(),
            "action-2".to_string(),
            CapturingExecutor::default(),
        )
        .expect("specific action executed");
        let loaded = agent_task_loop_controller::load_controller("loop-controller-run-action")
            .expect("controller loaded");

        assert_eq!(exit_code, 0);
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
