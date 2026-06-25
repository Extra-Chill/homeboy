//! Command-gate execution, from-spec resume drive, and event application tests.
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
fn run_gates_executes_command_bundle_and_records_result() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-gates".to_string(),
            phase: "validate".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "quality".to_string(),
            description: "quality gates".to_string(),
            checks: vec![AgentTaskGateBundleCheck {
                check_id: "true-command".to_string(),
                kind: AgentTaskGateBundleCheckKind::Command,
                input: json!({ "command": "true" }),
                retryable: false,
            }],
        });
        record.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "quality".to_string(),
                entity_id: None,
            },
            "run quality gates",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-gates",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gates executed");

        assert_eq!(result.exit_code, 0);
        let loaded = controller::load_controller("loop-service-gates").expect("controller");
        assert_eq!(loaded.gate_results.len(), 1);
        assert_eq!(
            loaded.gate_results[0].status,
            AgentTaskGateBundleStatus::Passed
        );
    });
}

#[test]
fn command_gate_check_runs_from_configured_cwd() {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let cwd_path = cwd.path().to_string_lossy().into_owned();
    std::fs::write(cwd.path().join("gate-marker"), "ok").expect("marker file");
    let check = AgentTaskGateBundleCheck {
        check_id: "pwd-command".to_string(),
        kind: AgentTaskGateBundleCheckKind::Command,
        input: json!({
            "command": "test -f gate-marker && printf ok",
            "cwd": cwd_path,
        }),
        retryable: false,
    };

    let result = run_command_gate_check(&check).expect("command gate executed");

    assert_eq!(result.status, AgentTaskGateBundleStatus::Passed);
    assert_eq!(result.details["stdout"].as_str(), Some("ok"));
    assert_eq!(result.details["cwd"].as_str(), Some(cwd_path.as_str()));
}

#[test]
fn command_gate_check_caps_stored_stdout_and_records_truncation() {
    let check = AgentTaskGateBundleCheck {
        check_id: "large-output-command".to_string(),
        kind: AgentTaskGateBundleCheckKind::Command,
        input: json!({
            "command": "yes x | head -c 70000",
            "timeout_seconds": 5,
        }),
        retryable: false,
    };

    let result = run_command_gate_check(&check).expect("command gate executed");

    assert_eq!(result.status, AgentTaskGateBundleStatus::Passed);
    assert_eq!(result.details["stdout_truncated"].as_bool(), Some(true));
    assert_eq!(
        result.details["stdout_stored_bytes"].as_u64(),
        Some(64 * 1024)
    );
    assert_eq!(result.details["stdout_bytes"].as_u64(), Some(70000));
    assert_eq!(
        result.details["stdout"]
            .as_str()
            .expect("stdout stored")
            .len(),
        64 * 1024
    );
}

#[test]
fn from_spec_resume_drives_generic_workflow_gates_completion_and_lineage() {
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

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.results.len(), 3);
        assert_eq!(
            result.value.controller.state,
            AgentTaskLoopControllerState::Completed
        );
        assert!(result
            .value
            .controller
            .next_actions
            .iter()
            .all(|action| action.status == AgentTaskLoopActionStatus::Completed));
        assert_eq!(result.value.controller.gate_results.len(), 1);
        assert_eq!(
            result.value.controller.gate_results[0].status,
            AgentTaskGateBundleStatus::Warn
        );
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
