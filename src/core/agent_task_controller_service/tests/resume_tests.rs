use super::super::*;
use super::*;

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
            failed["runtime_evidence"]["runtime_invocation_id"],
            json!("invocation-123")
        );
        assert_eq!(
            failed["runtime_evidence"]["runtime_id"],
            json!("runtime-abc")
        );
        assert_eq!(
            failed["runtime_evidence"]["provider_id"],
            json!("synthetic-runtime")
        );
        assert_eq!(
            failed["runtime_evidence"]["failure_classification"],
            json!("provider")
        );
        assert_eq!(
            failed["runtime_evidence"]["phase"],
            json!("runtime_overlay_preparation")
        );
        assert_eq!(
            failed["runtime_evidence"]["artifact_refs"][0]["path"],
            json!("artifacts/bundle.zip")
        );
        assert_eq!(
            failed["runtime_evidence"]["transcript_refs"][0]["uri"],
            json!("artifacts/transcript.ndjson")
        );
        assert_eq!(
            failed["runtime_evidence"]["result_refs"][0]["uri"],
            json!("artifacts/result.json")
        );
        assert_eq!(
            failed["runtime_evidence"]["diagnostics_refs"][0]["uri"],
            json!("artifacts/diagnostics.json")
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

#[test]
fn from_spec_resume_drives_workflow_lineage_then_blocks_on_pending_manual_gate() {
    with_isolated_home(|_| {
        let spec = AgentTaskRepoLoopSpec {
            schema: Some("example/repo-loop/v1".to_string()),
            loop_id: "repo-loop-generic-execution".to_string(),
            phase: "repair".to_string(),
            config_version: "repo-v1".to_string(),
            metadata: json!({ "domain": "example" }),
            entities: vec![
                AgentTaskRepoLoopSpecEntity {
                    entity_type: "finding".to_string(),
                    key: "alpha".to_string(),
                    parent_entity_ids: Vec::new(),
                    metadata: json!({ "severity": "high" }),
                },
                AgentTaskRepoLoopSpecEntity {
                    entity_type: "finding".to_string(),
                    key: "beta".to_string(),
                    parent_entity_ids: Vec::new(),
                    metadata: json!({ "severity": "medium" }),
                },
            ],
            agents: vec![AgentTaskRepoLoopSpecAgent {
                agent_id: "repair-agent".to_string(),
                role: Some("repair".to_string()),
                instructions: Some("repair findings and return artifacts".to_string()),
                tools: vec!["repo-inspector".to_string()],
                abilities: vec!["patch-writer".to_string()],
                metadata: Value::Null,
            }],
            tools: vec![AgentTaskRepoLoopSpecTool {
                tool_id: "repo-inspector".to_string(),
                description: Some("inspect repository state".to_string()),
                input_schema: Value::Null,
            }],
            abilities: vec![AgentTaskRepoLoopSpecAbility {
                ability_id: "patch-writer".to_string(),
                description: Some("write candidate patches".to_string()),
                input: Value::Null,
            }],
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "repair-findings".to_string(),
                agent_id: Some("repair-agent".to_string()),
                prompt: Some("Repair each routed finding and report evidence.".to_string()),
                tasks: Vec::new(),
                entity_ids: vec!["finding:alpha".to_string(), "finding:beta".to_string()],
                fan_out: None,
                tools: vec!["repo-inspector".to_string()],
                abilities: vec!["patch-writer".to_string()],
                artifacts: vec!["candidate-patch".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec!["source-tree".to_string()],
                gates: vec!["quality".to_string()],
                metrics: vec!["coverage".to_string()],
                runtime_execution: Value::Null,
                inputs: json!({ "scope": "changed findings" }),
            }],
            artifacts: vec![AgentTaskRepoLoopSpecArtifact {
                artifact_id: "candidate-patch".to_string(),
                kind: "diff".to_string(),
                description: Some("candidate patch".to_string()),
                required: true,
            }],
            artifact_graph: Vec::new(),
            dependencies: vec![AgentTaskRepoLoopSpecDependency {
                dependency_id: "source-tree".to_string(),
                kind: "repo".to_string(),
                value: None,
                required: true,
            }],
            gates: vec![AgentTaskRepoLoopSpecGate {
                gate_id: "quality".to_string(),
                description: Some("repo quality gate".to_string()),
                metrics: vec!["coverage".to_string()],
                input: Value::Null,
            }],
            metrics: vec![AgentTaskRepoLoopSpecMetric {
                metric_id: "coverage".to_string(),
                description: Some("coverage should not regress".to_string()),
                target: Some("maintained".to_string()),
                input: Value::Null,
            }],
            gate_bundles: vec![AgentTaskGateBundle {
                bundle_id: "quality".to_string(),
                description: "repo quality gate bundle".to_string(),
                checks: vec![AgentTaskGateBundleCheck {
                    check_id: "external-quality-signal".to_string(),
                    kind: AgentTaskGateBundleCheckKind::Manual,
                    input: json!({ "metric": "coverage" }),
                    retryable: false,
                }],
            }],
            policy: None,
            phases: Vec::new(),
            actions: vec![
                AgentTaskLoopPolicyAction::RunGates {
                    bundle_id: "quality".to_string(),
                    entity_id: Some("finding:alpha".to_string()),
                },
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("repo loop contract executed".to_string()),
                },
            ],
            initial_event: None,
        };

        let initialized =
            init_from_spec(ControllerFromSpecRequest { spec }).expect("repo loop spec initialized");
        assert!(initialized.initialized);
        assert_eq!(initialized.actions.len(), 3);
        assert_eq!(
            initialized.actions[0].status,
            AgentTaskLoopActionStatus::Pending
        );
        match &initialized.actions[0].action {
            AgentTaskLoopPolicyAction::FanOut {
                request_template, ..
            } => {
                assert_eq!(request_template["mode"], "dispatch");
                let dispatch = request_template["dispatch"]
                    .as_object()
                    .expect("compiled dispatch request");
                assert!(dispatch.get("backend").is_none());
                assert!(dispatch.get("provider_config").is_none());
                assert!(dispatch.get("executor").is_none());
            }
            other => panic!("expected compiled workflow fan-out, got {other:?}"),
        }

        let dispatch = ArtifactDispatchHook::default();
        let result = resume(
            "repo-loop-generic-execution",
            CapturingExecutor::default(),
            &dispatch,
        )
        .expect("controller resumed");

        // The fan-out workflow dispatches both findings, then the manual-only
        // acceptance gate blocks the loop: a manual check is Pending (awaiting an
        // external result), so the gate fails closed instead of auto-passing as a
        // non-blocking warning. The terminal Complete action is never reached.
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.results.len(), 2);
        assert_eq!(result.value.stopped_reason, "action_failed");
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Running
        );
        assert_eq!(result.value.controller.gate_results.len(), 1);
        assert_eq!(
            result.value.controller.gate_results[0].status,
            AgentTaskGateBundleStatus::Pending
        );
        let gate_action = result
            .value
            .controller
            .next_actions
            .iter()
            .find(|action| {
                matches!(
                    action.action,
                    AgentTaskLoopPolicyAction::RunGates { .. }
                )
            })
            .expect("run-gates action present");
        assert_eq!(gate_action.status, AgentTaskLoopActionStatus::Failed);
        assert!(result
            .value
            .controller
            .next_actions
            .iter()
            .any(|action| matches!(action.action, AgentTaskLoopPolicyAction::Complete { .. })
                && action.status == AgentTaskLoopActionStatus::Pending));
        assert!(result
            .value
            .controller
            .terminal_outcomes
            .iter()
            .any(|outcome| outcome.status == AgentTaskLoopTerminalStatus::BlockedByGate));
        assert_eq!(result.value.controller.task_lineage.len(), 2);
        assert!(result.value.controller.task_lineage.iter().any(|lineage| {
            lineage.run_id == "generic-run-finding_alpha"
                && lineage.entity_id.as_deref() == Some("finding:alpha")
                && lineage.dedupe_key.as_deref() == Some("workflow:repair-findings:finding:alpha")
                && lineage.inputs["dispatch"]["client_context"]
                    .as_str()
                    .is_some_and(|context| context.contains("repair-findings"))
        }));
        assert!(result.value.controller.task_lineage.iter().any(|lineage| {
            lineage.run_id == "generic-run-finding_beta"
                && lineage.entity_id.as_deref() == Some("finding:beta")
                && lineage.dedupe_key.as_deref() == Some("workflow:repair-findings:finding:beta")
        }));

        let observed = dispatch
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(observed.len(), 2);
        assert!(observed.iter().all(|request| {
            let dispatch = request["dispatch"].as_object().expect("dispatch object");
            dispatch.get("backend").is_none()
                && dispatch.get("provider_config").is_none()
                && dispatch.get("executor").is_none()
        }));
    });
}
