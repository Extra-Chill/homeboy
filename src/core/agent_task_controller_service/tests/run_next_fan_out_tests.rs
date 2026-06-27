use super::super::*;
use super::*;

#[test]
fn run_next_returns_unclaimed_when_no_pending_actions() {
    with_isolated_home(|_| {
        init(ControllerInitRequest {
            loop_id: "loop-service-noop".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        let result = run_next(
            "loop-service-noop",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("controller polled");

        assert_eq!(result.exit_code, 0);
        assert!(!result.value.claimed);
        assert_eq!(result.value.schema, ACTION_RESULT_SCHEMA);
        assert!(result.value.action_id.is_none());
        assert!(result.value.execution.is_none());
    });
}

#[test]
fn run_next_executes_spawn_task_action_and_records_lineage() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-spawn".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        let plan = test_plan();
        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "run_plan",
                    "run_id": "controller-service-spawn-a",
                    "plan": plan,
                }),
            },
            "finding emitted",
        );
        controller::write_controller(&record).expect("controller written");

        let executor = CapturingExecutor::default();
        let result = run_next("loop-service-spawn", executor.clone(), &NoopDispatchHook)
            .expect("controller action executed");

        assert_eq!(result.exit_code, 0);
        assert!(result.value.claimed);
        assert_eq!(result.value.status.as_deref(), Some("completed"));

        let loaded = controller::load_controller("loop-service-spawn").expect("controller");
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.dedupe_keys["finding:abc:repair"].run_id.as_deref(),
            Some("controller-service-spawn-a")
        );
        assert_eq!(loaded.task_lineage[0].run_id, "controller-service-spawn-a");
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.claimed"));
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.completed"));

        let observed = executor
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");
        assert_eq!(observed.task_id, "controller-service-task");
    });
}

#[test]
fn run_next_indexes_spawn_task_evidence_into_lineage_and_entity_outputs() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-spawn-evidence".to_string(),
            phase: "review".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        let entity_id = record.upsert_entity(
            "candidate".to_string(),
            "one".to_string(),
            Vec::new(),
            Value::Null,
        );

        record.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "candidate:one:review".to_string(),
                entity_id: Some(entity_id.clone()),
                request: json!({
                    "mode": "run_plan",
                    "run_id": "controller-service-spawn-evidence-a",
                    "plan": test_plan(),
                }),
            },
            "candidate emitted",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-spawn-evidence",
            EvidenceExecutor,
            &NoopDispatchHook,
        )
        .expect("controller action executed");

        assert_eq!(result.exit_code, 0);
        let loaded =
            controller::load_controller("loop-service-spawn-evidence").expect("controller");
        let lineage = &loaded.task_lineage[0];
        assert_eq!(lineage.run_id, "controller-service-spawn-evidence-a");
        assert_eq!(lineage.artifact_refs.len(), 3);
        assert_eq!(
            lineage.outputs["evidence_index"]["schema"],
            json!("homeboy/agent-task-loop-controller-evidence-index/v1")
        );
        assert_eq!(
            lineage.outputs["evidence_index"]["entries"][0]["artifacts"][0]["id"],
            json!("report")
        );
        assert_eq!(
            lineage.outputs["evidence_index"]["entries"][0]["evidence_refs"][0]["uri"],
            json!("artifacts/transcript.log")
        );
        assert_eq!(
            lineage.outputs["evidence_index"]["entries"][0]["typed_artifacts"][0]["name"],
            json!("decision")
        );

        let entity = loaded.entities.get(&entity_id).expect("entity indexed");
        assert_eq!(entity.artifact_refs.len(), 3);
        assert_eq!(
            entity.metadata["outputs"]["evidence_indexes"][0]["run_id"],
            json!("controller-service-spawn-evidence-a")
        );
    });
}

#[test]
fn run_next_treats_zero_item_fan_out_as_deterministic_no_actionable_findings() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-empty-fanout".to_string(),
            phase: "collect".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        record.record_action(
            AgentTaskLoopPolicyAction::FanOut {
                dedupe_key: "workflow:empty".to_string(),
                entity_ids: Vec::new(),
                max_items: DEFAULT_FAN_OUT_MAX_ITEMS,
                fail_fast: true,
                request_template: json!({ "mode": "dispatch" }),
            },
            "no findings emitted",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-empty-fanout",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("fan-out action executed");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status.as_deref(), Some("completed"));
        assert_eq!(
            result.value.execution.as_ref().unwrap()["item_count"],
            json!(0)
        );

        let loaded =
            controller::load_controller("loop-service-empty-fanout").expect("controller loaded");
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(loaded.task_lineage.len(), 0);
        assert_eq!(loaded.terminal_outcomes.len(), 1);
        assert_eq!(
            loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::NoActionableFindings
        );
        assert_eq!(loaded.terminal_outcomes[0].details["item_count"], json!(0));
    });
}

#[test]
fn fan_out_stops_after_max_items() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-fanout-max-items".to_string(),
            phase: "review".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        record.record_action(
            AgentTaskLoopPolicyAction::FanOut {
                dedupe_key: "candidate:bounded-review".to_string(),
                entity_ids: vec![
                    "candidate:first".to_string(),
                    "candidate:second".to_string(),
                    "candidate:third".to_string(),
                ],
                max_items: 2,
                fail_fast: true,
                request_template: json!({ "mode": "dispatch" }),
            },
            "review bounded candidates",
        );
        controller::write_controller(&record).expect("controller written");

        let dispatch = CapturingDispatchHook::default();
        let result = run_next(
            "loop-service-fanout-max-items",
            CapturingExecutor::default(),
            &dispatch,
        )
        .expect("fan-out action executed");

        assert_eq!(result.exit_code, 0);
        let execution = result.value.execution.as_ref().expect("execution");
        assert_eq!(execution["item_count"], json!(2));
        assert_eq!(execution["total_item_count"], json!(3));
        assert_eq!(execution["max_items"], json!(2));
        assert_eq!(execution["fail_fast"], json!(true));
        assert_eq!(execution["concurrency"], json!(1));
        assert_eq!(execution["truncated"], json!(true));
        assert_eq!(execution["results"].as_array().expect("results").len(), 2);
        let observed = dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0]["entity_id"], json!("candidate:first"));
        assert_eq!(observed[1]["entity_id"], json!("candidate:second"));
    });
}

#[test]
fn fan_out_fail_fast_stops_after_first_failure() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-fanout-fail-fast".to_string(),
            phase: "review".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        record.record_action(
            AgentTaskLoopPolicyAction::FanOut {
                dedupe_key: "candidate:fail-fast-review".to_string(),
                entity_ids: vec![
                    "candidate:first".to_string(),
                    "candidate:second".to_string(),
                ],
                max_items: DEFAULT_FAN_OUT_MAX_ITEMS,
                fail_fast: true,
                request_template: json!({ "mode": "dispatch" }),
            },
            "review candidates until failure",
        );
        controller::write_controller(&record).expect("controller written");

        let dispatch = CountingFailingDispatchHook::default();
        let result = run_next(
            "loop-service-fanout-fail-fast",
            CapturingExecutor::default(),
            &dispatch,
        )
        .expect("fan-out action executed");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status.as_deref(), Some("failed"));
        let execution = result.value.execution.as_ref().expect("execution");
        assert_eq!(execution["item_count"], json!(1));
        assert_eq!(execution["total_item_count"], json!(2));
        assert_eq!(execution["fail_fast"], json!(true));
        assert_eq!(execution["concurrency"], json!(1));
        assert_eq!(execution["truncated"], json!(true));
        assert_eq!(execution["results"].as_array().expect("results").len(), 1);
        let observed = dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["entity_id"], json!("candidate:first"));
    });
}

#[test]
fn fan_out_indexes_each_child_task_evidence_on_its_entity() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-fanout-evidence".to_string(),
            phase: "review".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        let first = record.upsert_entity("candidate", "first", Vec::new(), Value::Null);
        let second = record.upsert_entity("candidate", "second", Vec::new(), Value::Null);

        record.record_action(
            AgentTaskLoopPolicyAction::FanOut {
                dedupe_key: "candidate:review".to_string(),
                entity_ids: vec![first.clone(), second.clone()],
                max_items: DEFAULT_FAN_OUT_MAX_ITEMS,
                fail_fast: true,
                request_template: json!({
                    "mode": "run_plan",
                    "plan": test_plan(),
                }),
            },
            "review candidates",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-fanout-evidence",
            EvidenceExecutor,
            &NoopDispatchHook,
        )
        .expect("fan-out action executed");

        assert_eq!(result.exit_code, 0);
        let loaded =
            controller::load_controller("loop-service-fanout-evidence").expect("controller");
        assert_eq!(loaded.task_lineage.len(), 2);
        for entity_id in [first, second] {
            let entity = loaded.entities.get(&entity_id).expect("entity indexed");
            assert_eq!(entity.artifact_refs.len(), 3);
            assert_eq!(
                entity.metadata["outputs"]["evidence_indexes"][0]["entries"][0]["typed_artifacts"]
                    [0]["artifact_schema"],
                json!("example/review-decision/v1")
            );
        }
    });
}
