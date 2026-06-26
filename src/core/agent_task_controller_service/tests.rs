//! Tests split from `agent_task_controller_service` god file (#5208).
use super::*;
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

#[derive(Clone, Default)]
struct CapturingExecutor {
    observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

#[derive(Clone, Default)]
struct CapturingDispatchHook {
    observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
struct FailingDispatchHook;

#[derive(Clone, Default)]
struct ArtifactDispatchHook {
    observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
struct TypedArtifactHandoffDispatchHook {
    observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
struct CountingFailingDispatchHook {
    observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
struct RuntimeBudgetDispatchHook;

#[derive(Clone, Default)]
struct EvidenceExecutor;

impl ControllerDispatchHook for CapturingDispatchHook {
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
        self.observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.clone());
        let entity_id = request
            .get("entity_id")
            .and_then(Value::as_str)
            .unwrap_or("workflow");
        Ok((
            json!({
                "schema": "homeboy/test-generic-dispatch-result/v1",
                "run_id": format!("generic-run-{}", entity_id.replace([':', '/', '#', ' '], "_")),
            }),
            0,
        ))
    }
}

impl ControllerDispatchHook for FailingDispatchHook {
    fn dispatch(&self, _request: &Value) -> Result<(Value, i32)> {
        Ok((
            json!({
                "schema": "homeboy/test-generic-dispatch-result/v1",
                "run_id": "generic-run-overlay",
                "aggregate": {
                    "outcomes": [{
                        "task_id": "task-overlay-prepare",
                        "status": "failed",
                        "diagnostics": [{
                            "class": "provider.runtime_overlay",
                            "message": "Recipe runtime overlay preparation failed: download php-scoper timed out after 60004ms",
                            "data": {
                                "provider": "synthetic-runtime",
                                "phase": "runtime_overlay_preparation"
                            }
                        }]
                    }]
                }
            }),
            1,
        ))
    }
}

impl ControllerDispatchHook for ArtifactDispatchHook {
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
        self.observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.clone());
        let entity_id = request
            .get("entity_id")
            .and_then(Value::as_str)
            .unwrap_or("workflow");
        Ok((
            json!({
                "schema": "homeboy/test-generic-dispatch-result/v1",
                "run_id": format!("generic-run-{}", entity_id.replace([':', '/', '#', ' '], "_")),
                "artifacts": [{
                    "id": "candidate-patch",
                    "kind": "diff",
                    "metadata": {
                        "payload": {
                            "entity_id": entity_id
                        }
                    }
                }]
            }),
            0,
        ))
    }
}

impl ControllerDispatchHook for TypedArtifactHandoffDispatchHook {
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
        let mut observed = self
            .observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        observed.push(request.clone());
        let index = observed.len();
        drop(observed);

        let mut result = json!({
            "schema": "homeboy/test-generic-dispatch-result/v1",
            "run_id": format!("typed-artifact-handoff-{index}"),
        });
        if index == 1 {
            result["typed_artifacts"] = json!([{
                "name": "static_site_candidate",
                "type": "static_site",
                "payload": { "path": "dist/index.html" }
            }]);
        }
        Ok((result, 0))
    }
}

impl ControllerDispatchHook for CountingFailingDispatchHook {
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
        self.observed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.clone());
        let entity_id = request
            .get("entity_id")
            .and_then(Value::as_str)
            .unwrap_or("workflow");
        Ok((
            json!({
                "schema": "homeboy/test-generic-dispatch-result/v1",
                "run_id": format!("generic-run-{}", entity_id.replace([':', '/', '#', ' '], "_")),
                "diagnostics": [{
                    "message": format!("synthetic failure for {entity_id}")
                }]
            }),
            1,
        ))
    }
}

impl ControllerDispatchHook for RuntimeBudgetDispatchHook {
    fn dispatch(&self, _request: &Value) -> Result<(Value, i32)> {
        Ok((
            json!({
                "schema": "homeboy/test-generic-dispatch-result/v1",
                "run_id": "generic-run-runtime-budget",
                "aggregate": {
                    "outcomes": [{
                        "task_id": "task-runtime-bundle",
                        "status": "succeeded",
                        "outputs": {
                            "provider_run_result": {
                                "wait_result": {
                                    "terminal_state": "timeout",
                                    "steps_drained": 12,
                                    "actions_drained": 7,
                                    "elapsed_ms": 300123
                                },
                                "job_status": "running",
                                "completion_outcome": "wait_budget_expired",
                                "error_type": "runtime_wait_timeout",
                                "classification": "runtime_incomplete"
                            }
                        }
                    }]
                }
            }),
            0,
        ))
    }
}

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

impl AgentTaskExecutorAdapter for CapturingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        *self
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
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
        }
    }
}

impl AgentTaskExecutorAdapter for EvidenceExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("evidence captured".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "report".to_string(),
                kind: "report".to_string(),
                name: Some("review report".to_string()),
                label: Some("Review report".to_string()),
                role: Some("report".to_string()),
                semantic_key: Some("task.report".to_string()),
                path: Some("artifacts/report.md".to_string()),
                url: None,
                mime: Some("text/markdown".to_string()),
                size_bytes: Some(128),
                sha256: Some("abc123".to_string()),
                metadata: Value::Null,
            }],
            typed_artifacts: vec![AgentTaskTypedArtifact {
                name: "decision".to_string(),
                artifact_type: Some("review-decision".to_string()),
                artifact_schema: Some("example/review-decision/v1".to_string()),
                payload: json!({ "accepted": true }),
                artifact: None,
                metadata: Value::Null,
            }],
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: "artifacts/transcript.log".to_string(),
                label: Some("transcript".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: json!({ "ok": true }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
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

fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "controller-service-plan",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "controller-service-task".to_string(),
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

fn repo_loop_reconcile_spec(loop_id: &str) -> AgentTaskRepoLoopSpec {
    AgentTaskRepoLoopSpec {
        schema: Some("example/repo-loop/v1".to_string()),
        loop_id: loop_id.to_string(),
        phase: "init".to_string(),
        config_version: "repo-v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: vec![
            AgentTaskRepoLoopSpecAbility {
                ability_id: "github_pull_request_publish".to_string(),
                description: None,
                input: Value::Null,
            },
            AgentTaskRepoLoopSpecAbility {
                ability_id: "static_validation".to_string(),
                description: None,
                input: Value::Null,
            },
            AgentTaskRepoLoopSpecAbility {
                ability_id: "static_publication".to_string(),
                description: None,
                input: Value::Null,
            },
        ],
        workflows: vec![
            AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "generation".to_string(),
                agent_id: None,
                prompt: Some("Generate a static site candidate.".to_string()),
                tasks: Vec::new(),
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: vec!["github_pull_request_publish".to_string()],
                artifacts: vec!["static_site_pull_request".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: Value::Null,
                inputs: Value::Null,
            },
            AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "static_validation".to_string(),
                agent_id: None,
                prompt: Some("Validate the generated static site.".to_string()),
                tasks: Vec::new(),
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: vec!["static_validation".to_string()],
                artifacts: Vec::new(),
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec!["static_site_pull_request".to_string()],
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: Value::Null,
                inputs: Value::Null,
            },
        ],
        artifacts: vec![
            AgentTaskRepoLoopSpecArtifact {
                artifact_id: "static_site_pull_request".to_string(),
                kind: "pull_request".to_string(),
                description: None,
                required: true,
            },
            AgentTaskRepoLoopSpecArtifact {
                artifact_id: "static_site_candidate".to_string(),
                kind: "static_site".to_string(),
                description: None,
                required: true,
            },
        ],
        artifact_graph: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    }
}

fn reapply_base_then_mutated(
    loop_id: &str,
    mutate: impl FnOnce(&mut AgentTaskRepoLoopSpec),
) -> ControllerFromSpecReport {
    let base = repo_loop_reconcile_spec(loop_id);
    init_from_spec(ControllerFromSpecRequest { spec: base.clone() })
        .expect("base spec initialized");
    let reapplied = init_from_spec(ControllerFromSpecRequest { spec: base.clone() })
        .expect("base spec reapplied");
    assert!(reapplied
        .actions
        .iter()
        .any(|action| action.status == AgentTaskLoopActionStatus::AlreadySatisfied));

    let mut changed = base;
    mutate(&mut changed);
    init_from_spec(ControllerFromSpecRequest { spec: changed }).expect("changed spec applied")
}

fn workflow_action_context(report: &ControllerFromSpecReport, workflow_id: &str) -> Value {
    let action = report
        .actions
        .iter()
        .find(|action| action.dedupe_key.as_deref() == Some(&format!("workflow:{workflow_id}")))
        .expect("workflow action exists");
    let request = match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask { request, .. } => request,
        AgentTaskLoopPolicyAction::FanOut {
            request_template, ..
        } => request_template,
        other => panic!("expected workflow dispatch action, got {other:?}"),
    };
    serde_json::from_str(
        request["dispatch"]["client_context"]
            .as_str()
            .expect("client_context string"),
    )
    .expect("client_context json")
}

fn assert_reconciled_to_pending(report: &ControllerFromSpecReport, expected_actions: usize) {
    assert_eq!(report.actions.len(), expected_actions);
    assert!(report
        .actions
        .iter()
        .all(|action| action.status == AgentTaskLoopActionStatus::Pending));
    assert_eq!(report.controller.next_actions.len(), expected_actions);
    assert!(report
        .controller
        .next_actions
        .iter()
        .all(|action| action.status == AgentTaskLoopActionStatus::Pending));
    let history = report.controller.history.last().expect("history event");
    assert_eq!(history.payload["reconciled_action_count"], json!(4));
    assert_eq!(history.payload["reconciled_dedupe_key_count"], json!(2));
}

#[test]
fn init_and_status_round_trip_controller_record() {
    with_isolated_home(|_| {
        let record = init(ControllerInitRequest {
            loop_id: "loop-service-init".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        assert_eq!(record.loop_id, "loop-service-init");
        assert_eq!(record.phase, "repair");

        let loaded = status("loop-service-init").expect("controller loaded");
        assert_eq!(loaded, record);
    });
}

#[test]
fn list_returns_existing_controllers() {
    with_isolated_home(|_| {
        init(ControllerInitRequest {
            loop_id: "loop-service-list-a".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller a initialized");
        init(ControllerInitRequest {
            loop_id: "loop-service-list-b".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller b initialized");

        let report = list().expect("controllers listed");
        assert_eq!(report.schema, LIST_RESULT_SCHEMA);
        assert_eq!(report.controllers.len(), 2);
    });
}

#[test]
fn repo_loop_spec_accepts_controller_id_and_keyed_contract_maps() {
    let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
        "schema": "homeboy/controller-spec/v1",
        "controller_id": "repo-loop-keyed-spec",
        "agents": {
            "repair-agent": {
                "role": "repair",
                "tools": ["repo-inspector"]
            }
        },
        "tools": {
            "repo-inspector": {
                "description": "inspect repo files"
            }
        },
        "workflows": {
            "repair-findings": {
                "agent_id": "repair-agent",
                "prompt": "Repair this finding.",
                "tools": ["repo-inspector"],
                "artifacts": ["patch"]
            }
        },
        "artifacts": {
            "patch": {
                "kind": "diff",
                "required": true
            }
        }
    }))
    .expect("keyed controller spec deserializes");

    assert_eq!(spec.loop_id, "repo-loop-keyed-spec");
    assert_eq!(spec.agents[0].agent_id, "repair-agent");
    assert_eq!(spec.tools[0].tool_id, "repo-inspector");
    assert_eq!(spec.workflows[0].workflow_id, "repair-findings");
    assert_eq!(spec.artifacts[0].artifact_id, "patch");
}

#[test]
fn repo_loop_spec_preserves_explicit_ids_inside_keyed_contract_maps() {
    let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
        "loop_id": "repo-loop-explicit-ids",
        "agents": {
            "repair": {
                "agent_id": "repair-agent"
            }
        },
        "tools": {
            "inspect": {
                "tool_id": "repo-inspector"
            }
        },
        "workflows": {
            "repair": {
                "workflow_id": "repair-findings",
                "prompt": "Repair this finding."
            }
        },
        "artifacts": {
            "patch-output": {
                "artifact_id": "patch",
                "kind": "diff"
            }
        }
    }))
    .expect("keyed controller spec deserializes");

    assert_eq!(spec.agents[0].agent_id, "repair-agent");
    assert_eq!(spec.tools[0].tool_id, "repo-inspector");
    assert_eq!(spec.workflows[0].workflow_id, "repair-findings");
    assert_eq!(spec.artifacts[0].artifact_id, "patch");
}

#[test]
fn init_from_spec_compiles_repo_workflows_into_deduped_dispatch_actions() {
    with_isolated_home(|_| {
        let spec = AgentTaskRepoLoopSpec {
            schema: Some("example/repo-loop/v1".to_string()),
            loop_id: "repo-loop-spec".to_string(),
            phase: "init".to_string(),
            config_version: "repo-v1".to_string(),
            metadata: json!({
                "domain": "example",
                "dispatch_defaults": {
                    "cwd": "/tmp/repo-loop-spec-checkout",
                    "repo": "repo-loop-spec-checkout"
                }
            }),
            entities: vec![AgentTaskRepoLoopSpecEntity {
                entity_type: "finding".to_string(),
                key: "abc".to_string(),
                parent_entity_ids: Vec::new(),
                metadata: json!({ "severity": "high" }),
            }],
            agents: vec![AgentTaskRepoLoopSpecAgent {
                agent_id: "repair-agent".to_string(),
                role: Some("repair".to_string()),
                instructions: Some("repair the routed finding".to_string()),
                tools: vec!["repo-inspector".to_string()],
                abilities: vec!["apply_patch".to_string()],
                metadata: Value::Null,
            }],
            tools: vec![AgentTaskRepoLoopSpecTool {
                tool_id: "repo-inspector".to_string(),
                description: Some("inspect repo files".to_string()),
                input_schema: Value::Null,
            }],
            abilities: vec![AgentTaskRepoLoopSpecAbility {
                ability_id: "apply_patch".to_string(),
                description: Some("apply focused patches".to_string()),
                input: Value::Null,
            }],
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "repair-findings".to_string(),
                agent_id: Some("repair-agent".to_string()),
                prompt: Some("Repair this finding and report evidence.".to_string()),
                tasks: Vec::new(),
                entity_ids: vec!["finding:abc".to_string()],
                fan_out: None,
                tools: vec!["repo-inspector".to_string()],
                abilities: vec!["apply_patch".to_string()],
                artifacts: vec!["patch".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec![
                    "source-tree".to_string(),
                    "static_site_pull_request".to_string(),
                ],
                gates: vec!["quality".to_string()],
                metrics: vec!["visual-parity".to_string()],
                runtime_execution: Value::Null,
                inputs: json!({ "finding_key": "abc" }),
            }],
            artifacts: vec![
                AgentTaskRepoLoopSpecArtifact {
                    artifact_id: "patch".to_string(),
                    kind: "diff".to_string(),
                    description: Some("candidate patch".to_string()),
                    required: true,
                },
                AgentTaskRepoLoopSpecArtifact {
                    artifact_id: "static_site_pull_request".to_string(),
                    kind: "pull_request".to_string(),
                    description: Some("upstream pull request artifact".to_string()),
                    required: true,
                },
            ],
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
                metrics: vec!["visual-parity".to_string()],
                input: Value::Null,
            }],
            metrics: vec![AgentTaskRepoLoopSpecMetric {
                metric_id: "visual-parity".to_string(),
                description: Some("visual parity threshold".to_string()),
                target: Some(">=0.98".to_string()),
                input: Value::Null,
            }],
            gate_bundles: vec![
                crate::core::agent_task_loop_controller::AgentTaskGateBundle {
                    bundle_id: "quality".to_string(),
                    description: "repo-owned quality gates".to_string(),
                    checks: Vec::new(),
                },
            ],
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        };

        let report = init_from_spec(ControllerFromSpecRequest { spec: spec.clone() })
            .expect("spec initialized");

        assert_eq!(report.schema, FROM_SPEC_RESULT_SCHEMA);
        assert!(report.initialized);
        assert_eq!(report.controller.config_version, "repo-v1");
        assert!(report.controller.entities.contains_key("finding:abc"));
        assert_eq!(report.controller.gate_bundles[0].bundle_id, "quality");
        assert_eq!(report.actions.len(), 1);
        assert_eq!(report.actions[0].status, AgentTaskLoopActionStatus::Pending);
        match &report.actions[0].action {
            AgentTaskLoopPolicyAction::FanOut {
                dedupe_key,
                request_template,
                ..
            } => {
                assert_eq!(dedupe_key, "workflow:repair-findings");
                assert_eq!(request_template["mode"], "dispatch");
                assert_eq!(
                    request_template["dispatch"]["cwd"],
                    "/tmp/repo-loop-spec-checkout"
                );
                assert_eq!(
                    request_template["dispatch"]["repo"],
                    "repo-loop-spec-checkout"
                );
                assert!(request_template["dispatch"].get("backend").is_none());
                assert!(request_template["dispatch"]
                    .get("provider_config")
                    .is_none());
                assert!(request_template["dispatch"]
                    .get("required_capabilities")
                    .is_none());
                let context: Value = serde_json::from_str(
                    request_template["dispatch"]["client_context"]
                        .as_str()
                        .expect("client context string"),
                )
                .expect("client context json");
                assert_eq!(context["agent"]["agent_id"], "repair-agent");
                assert_eq!(
                    context["plan"]["inputs"]["schema"],
                    "homeboy/repo-loop-workflow-plan/v1"
                );
                assert!(context["plan"]["policy"]
                    .get("required_capabilities")
                    .is_none());
                assert_eq!(context["agent"]["tools"], json!(["repo-inspector"]));
                assert_eq!(context["agent"]["abilities"], json!(["apply_patch"]));
                assert_eq!(context["plan"]["steps"][0]["kind"], "agent_task_dispatch");
                assert_eq!(
                    context["plan"]["steps"][0]["needs"],
                    json!(["source-tree", "static_site_pull_request"])
                );
                assert_eq!(context["plan"]["artifacts"][0]["id"], "patch");
                assert_eq!(
                    context["artifact_dependencies"][0]["artifact_id"],
                    "static_site_pull_request"
                );
                assert_eq!(context["gates"][0]["gate_id"], "quality");
                assert_eq!(context["metrics"][0]["metric_id"], "visual-parity");
            }
            other => panic!("expected fan_out workflow action, got {other:?}"),
        }

        let resumed = init_from_spec(ControllerFromSpecRequest { spec }).expect("spec reapplied");

        assert!(!resumed.initialized);
        assert_eq!(
            resumed.actions[0].status,
            AgentTaskLoopActionStatus::AlreadySatisfied
        );
    });
}

#[test]
fn init_from_spec_compiles_workflow_fan_out_items_into_deduped_dispatch_action() {
    with_isolated_home(|_| {
        let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
            "loop_id": "repo-loop-fan-out-items",
            "workflows": [{
                "workflow_id": "repair-findings",
                "prompt": "Repair each routed finding.",
                "fan_out": {
                    "items": ["finding:alpha", "finding:beta"],
                    "max_items": 1,
                    "fail_fast": false
                }
            }]
        }))
        .expect("spec deserializes");

        let report = init_from_spec(ControllerFromSpecRequest { spec }).expect("spec initialized");

        assert_eq!(report.actions.len(), 1);
        match &report.actions[0].action {
            AgentTaskLoopPolicyAction::FanOut {
                dedupe_key,
                entity_ids,
                max_items,
                fail_fast,
                ..
            } => {
                assert_eq!(dedupe_key, "workflow:repair-findings");
                assert_eq!(
                    entity_ids,
                    &vec!["finding:alpha".to_string(), "finding:beta".to_string()]
                );
                assert_eq!(*max_items, 1);
                assert!(!fail_fast);
            }
            other => panic!("expected fan_out workflow action, got {other:?}"),
        }
    });
}

#[test]
fn init_from_spec_rejects_dynamic_artifact_fan_out_until_artifact_expansion_exists() {
    with_isolated_home(|_| {
        let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
            "loop_id": "repo-loop-artifact-fan-out",
            "workflows": [{
                "workflow_id": "iterator",
                "prompt": "Route each emitted finding group.",
                "fan_out": {
                    "mode": "per_artifact",
                    "artifact": "finding_group",
                    "group_by": ["owner_repo", "root_cause", "group_id"],
                    "requires_non_empty": true
                }
            }]
        }))
        .expect("spec deserializes");

        let error = init_from_spec(ControllerFromSpecRequest { spec })
            .expect_err("dynamic artifact fan-out needs controller artifact expansion");

        let message = error.to_string();
        assert!(message.contains("workflows[].fan_out"), "{message}");
        assert!(
            message.contains("artifact-to-entity expansion"),
            "{message}"
        );
    });
}

#[test]
fn init_from_spec_reconciles_changed_workflow_dependencies() {
    with_isolated_home(|_| {
        let report = reapply_base_then_mutated("repo-loop-reconcile-dependencies", |spec| {
            spec.workflows
                .iter_mut()
                .find(|workflow| workflow.workflow_id == "static_validation")
                .expect("static validation workflow")
                .dependencies = vec!["static_site_candidate".to_string()];
        });

        assert_reconciled_to_pending(&report, 2);
        let context = workflow_action_context(&report, "static_validation");
        assert_eq!(
            context["plan"]["steps"][0]["needs"],
            json!(["static_site_candidate"])
        );
        assert_eq!(
            context["artifact_dependencies"][0]["artifact_id"],
            "static_site_candidate"
        );
    });
}

#[test]
fn init_from_spec_for_resume_rejects_changed_existing_spec() {
    with_isolated_home(|_| {
        let base = repo_loop_reconcile_spec("repo-loop-resume-stale-guard");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base.clone() })
            .expect("base spec initialized for resume");

        let mut changed = base;
        changed
            .workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "static_validation")
            .expect("static validation workflow")
            .dependencies = vec!["static_site_candidate".to_string()];

        let error = init_from_spec_for_resume(ControllerFromSpecRequest { spec: changed })
            .expect_err("changed spec is blocked before resume");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error
            .message
            .contains("refusing to reuse stale persisted controller state"));
        assert!(error
            .message
            .contains("--reconcile-stale to safely reset run-scoped state"));
        let tried = error
            .details
            .get("tried")
            .and_then(Value::as_array)
            .expect("guard diagnostic lists tried details");
        let detail_has = |needle: &str| {
            tried.iter().any(|detail| {
                detail
                    .as_str()
                    .is_some_and(|detail| detail.contains(needle))
            })
        };
        // Diagnostic must name the state path, prior + requested fingerprint,
        // and the safe next action (#6221 acceptance criteria).
        assert!(detail_has("state_path="), "{tried:?}");
        assert!(detail_has("prior_spec_fingerprint="), "{tried:?}");
        assert!(detail_has("requested_spec_fingerprint="), "{tried:?}");
        assert!(
            detail_has("safe_next_action=--reconcile-stale"),
            "{tried:?}"
        );
    });
}

#[test]
fn init_from_spec_for_resume_reconcile_stale_resets_state_without_manual_cleanup() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-reconcile-stale");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base })
            .expect("base initialized");

        // One flag: no prior manual state cleanup required.
        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest {
                spec: changed.clone(),
            },
            ControllerResumeStateResolution::ReconcileStale,
        )
        .expect("reconcile-stale re-initializes controller");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "replacing");
        assert_eq!(resume_state.resolution, "reconcile-stale");
        assert!(resume_state.existing_controller);
        assert!(!resume_state.fingerprint_match);
        // Run-scoped state is re-derived from the requested spec, not the stale base.
        assert_eq!(
            repo_loop_spec_fingerprint_from_metadata(&report.controller),
            Some(repo_loop_spec_fingerprint(&changed).expect("fingerprint"))
        );
        assert_eq!(report.loop_id, "repo-loop-resume-reconcile-stale");
    });
}

fn changed_resume_spec(loop_id: &str) -> (AgentTaskRepoLoopSpec, AgentTaskRepoLoopSpec) {
    let base = repo_loop_reconcile_spec(loop_id);
    let mut changed = base.clone();
    changed
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow")
        .dependencies = vec!["static_site_candidate".to_string()];
    (base, changed)
}

#[test]
fn init_from_spec_for_resume_reports_creating_on_fresh_loop() {
    with_isolated_home(|_| {
        let base = repo_loop_reconcile_spec("repo-loop-resume-fresh");
        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: base },
            ControllerResumeStateResolution::Guard,
        )
        .expect("fresh loop initializes");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "creating");
        assert!(!resume_state.existing_controller);
        assert!(!resume_state.fingerprint_match);
        assert!(resume_state.previous_spec_fingerprint.is_none());
    });
}

#[test]
fn init_from_spec_for_resume_reports_resuming_on_matching_spec() {
    with_isolated_home(|_| {
        let base = repo_loop_reconcile_spec("repo-loop-resume-match");
        init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: base.clone() },
            ControllerResumeStateResolution::Guard,
        )
        .expect("base initialized");
        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: base },
            ControllerResumeStateResolution::Guard,
        )
        .expect("unchanged spec resumes");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "resuming");
        assert!(resume_state.existing_controller);
        assert!(resume_state.fingerprint_match);
    });
}

#[test]
fn init_from_spec_for_resume_replace_resets_stale_state() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-replace");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base })
            .expect("base initialized");

        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest {
                spec: changed.clone(),
            },
            ControllerResumeStateResolution::Replace,
        )
        .expect("replace re-initializes controller");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "replacing");
        assert_eq!(resume_state.resolution, "replace");
        // Replaced controller carries the new fingerprint and starts fresh.
        assert_eq!(
            repo_loop_spec_fingerprint_from_metadata(&report.controller),
            Some(repo_loop_spec_fingerprint(&changed).expect("fingerprint"))
        );
        assert_eq!(report.loop_id, "repo-loop-resume-replace");
    });
}

#[test]
fn init_from_spec_for_resume_fork_isolates_under_derived_loop_id() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-fork");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base.clone() })
            .expect("base initialized");

        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest {
                spec: changed.clone(),
            },
            ControllerResumeStateResolution::Fork,
        )
        .expect("fork applies under a derived loop id");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "forking");
        assert_eq!(resume_state.requested_loop_id, "repo-loop-resume-fork");
        assert_ne!(report.loop_id, "repo-loop-resume-fork");
        assert!(report.loop_id.starts_with("repo-loop-resume-fork-fork-"));

        // The original controller still carries the base fingerprint, untouched.
        let original = status("repo-loop-resume-fork").expect("original controller intact");
        assert_eq!(
            repo_loop_spec_fingerprint_from_metadata(&original),
            Some(repo_loop_spec_fingerprint(&base).expect("base fingerprint"))
        );
    });
}

#[test]
fn init_from_spec_for_resume_existing_accepts_stale_state() {
    with_isolated_home(|_| {
        let (base, changed) = changed_resume_spec("repo-loop-resume-existing");
        init_from_spec_for_resume(ControllerFromSpecRequest { spec: base })
            .expect("base initialized");

        let report = init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec: changed },
            ControllerResumeStateResolution::ResumeExisting,
        )
        .expect("resume-existing accepts stale state");
        let resume_state = report.resume_state.expect("resume_state present");
        assert_eq!(resume_state.action, "resuming");
        assert_eq!(resume_state.resolution, "resume-existing");
        assert!(resume_state.existing_controller);
        assert!(!resume_state.fingerprint_match);
        assert_eq!(report.loop_id, "repo-loop-resume-existing");
    });
}

#[test]
fn init_from_spec_projects_runtime_component_dependencies_to_contracts() {
    with_isolated_home(|_| {
        let mut spec = repo_loop_reconcile_spec("repo-loop-runtime-components");
        spec.dependencies.push(AgentTaskRepoLoopSpecDependency {
            dependency_id: "agents-api".to_string(),
            kind: "runtime_component".to_string(),
            value: Some("/tmp/homeboy-test/agents-api".to_string()),
            required: true,
        });
        spec.workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "static_validation")
            .expect("static validation workflow")
            .dependencies = vec!["agents-api".to_string()];

        let report = init_from_spec(ControllerFromSpecRequest { spec }).expect("spec initializes");
        let context = workflow_action_context(&report, "static_validation");

        assert_eq!(
            context["runtime_component_contracts"],
            json!([{
                "slug": "agents-api",
                "path": "/tmp/homeboy-test/agents-api",
                "required": true,
                "source": "repo_loop_spec_dependency",
                "dependency_kind": "runtime_component"
            }])
        );
    });
}

#[test]
fn init_from_spec_maps_workflow_consumes_to_artifact_dependencies() {
    with_isolated_home(|_| {
        let mut spec = repo_loop_reconcile_spec("repo-loop-consumes-artifacts");
        spec.workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "generation")
            .expect("generation workflow")
            .emits = vec!["static_site_pull_request".to_string()];
        spec.workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "static_validation")
            .expect("static validation workflow")
            .consumes = vec!["static_site_pull_request".to_string()];

        let report = init_from_spec(ControllerFromSpecRequest { spec }).expect("spec initialized");
        let context = workflow_action_context(&report, "static_validation");

        assert_eq!(
            context["artifact_dependencies"],
            json!([{
                "artifact_id": "static_site_pull_request",
                "kind": "pull_request",
                "required": true,
                "producer_workflow_ids": ["generation"]
            }])
        );
    });
}

#[test]
fn init_from_spec_projects_artifact_graph_edges_to_controller_metadata() {
    with_isolated_home(|_| {
        let mut spec = repo_loop_reconcile_spec("repo-loop-artifact-graph");
        spec.workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "generation")
            .expect("generation workflow")
            .emits = vec!["static_site_pull_request".to_string()];
        spec.workflows
            .iter_mut()
            .find(|workflow| workflow.workflow_id == "static_validation")
            .expect("static validation workflow")
            .consumes = vec!["static_site_pull_request".to_string()];
        spec.artifact_graph = vec![AgentTaskRepoLoopSpecArtifactGraphEdge {
            artifact_id: "static_site_pull_request".to_string(),
            from_workflow_id: "generation".to_string(),
            to_workflow_id: "static_validation".to_string(),
            required: true,
        }];

        let report = init_from_spec(ControllerFromSpecRequest { spec }).expect("spec initializes");
        let context = workflow_action_context(&report, "static_validation");

        assert_eq!(
            context["artifact_graph_edges"],
            json!([{
                "artifact_id": "static_site_pull_request",
                "from_workflow_id": "generation",
                "to_workflow_id": "static_validation",
                "required": true
            }])
        );
        assert_eq!(
            context["artifact_dependencies"][0]["producer_workflow_ids"],
            json!(["generation"])
        );
    });
}

#[test]
fn init_from_spec_reconciles_changed_emitted_artifacts() {
    with_isolated_home(|_| {
        let report = reapply_base_then_mutated("repo-loop-reconcile-artifacts", |spec| {
            spec.workflows
                .iter_mut()
                .find(|workflow| workflow.workflow_id == "generation")
                .expect("generation workflow")
                .artifacts = vec!["static_site_candidate".to_string()];
        });

        assert_reconciled_to_pending(&report, 2);
        let context = workflow_action_context(&report, "generation");
        assert_eq!(
            context["plan"]["artifacts"][0]["id"],
            "static_site_candidate"
        );
        assert_eq!(
            context["artifacts"][0]["artifact_id"],
            "static_site_candidate"
        );
    });
}

#[test]
fn init_from_spec_reconciles_removed_and_added_workflows() {
    with_isolated_home(|_| {
        let report = reapply_base_then_mutated("repo-loop-reconcile-workflows", |spec| {
            spec.workflows
                .retain(|workflow| workflow.workflow_id != "static_validation");
            spec.workflows.push(AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "static_publication".to_string(),
                agent_id: None,
                prompt: Some("Publish the validated static site.".to_string()),
                tasks: Vec::new(),
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: vec!["static_publication".to_string()],
                artifacts: vec!["static_site_pull_request".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec!["static_site_candidate".to_string()],
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: Value::Null,
                inputs: Value::Null,
            });
        });

        assert_reconciled_to_pending(&report, 2);
        let dedupe_keys = report
            .controller
            .next_actions
            .iter()
            .filter_map(|action| action.dedupe_key.as_deref())
            .collect::<Vec<_>>();
        assert!(dedupe_keys.contains(&"workflow:generation"));
        assert!(dedupe_keys.contains(&"workflow:static_publication"));
        assert!(!dedupe_keys.contains(&"workflow:static_validation"));
    });
}

#[test]
fn init_from_spec_reconciles_changed_workflow_abilities() {
    with_isolated_home(|_| {
        let report = reapply_base_then_mutated("repo-loop-reconcile-capabilities", |spec| {
            spec.workflows
                .iter_mut()
                .find(|workflow| workflow.workflow_id == "generation")
                .expect("generation workflow")
                .abilities = vec!["static_publication".to_string()];
        });

        assert_reconciled_to_pending(&report, 2);
        let generation = report
            .actions
            .iter()
            .find(|action| action.dedupe_key.as_deref() == Some("workflow:generation"))
            .expect("generation action");
        let request = match &generation.action {
            AgentTaskLoopPolicyAction::SpawnTask { request, .. } => request,
            AgentTaskLoopPolicyAction::FanOut {
                request_template, ..
            } => request_template,
            other => panic!("expected workflow dispatch action, got {other:?}"),
        };
        assert!(request["dispatch"].get("required_capabilities").is_none());
        let context = workflow_action_context(&report, "generation");
        assert!(context["plan"]["policy"]
            .get("required_capabilities")
            .is_none());
        assert_eq!(context["abilities"][0]["ability_id"], "static_publication");
    });
}

#[test]
fn init_from_spec_rejects_undeclared_workflow_requirements() {
    with_isolated_home(|_| {
        let spec = AgentTaskRepoLoopSpec {
            schema: None,
            loop_id: "repo-loop-invalid-reference".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
            metadata: Value::Null,
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "repair".to_string(),
                agent_id: None,
                prompt: Some("Repair with a declared tool.".to_string()),
                tasks: Vec::new(),
                entity_ids: Vec::new(),
                fan_out: None,
                tools: vec!["missing-tool".to_string()],
                abilities: Vec::new(),
                artifacts: Vec::new(),
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: Value::Null,
                inputs: Value::Null,
            }],
            artifacts: Vec::new(),
            artifact_graph: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        };

        let error = init_from_spec(ControllerFromSpecRequest { spec })
            .expect_err("missing requirement declaration should fail");

        assert_eq!(error.details["field"], "workflows[0].tools");
        assert!(error
            .message
            .contains("references an undeclared contract id"));
    });
}

#[test]
fn init_from_spec_applies_event_gated_policy() {
    with_isolated_home(|_| {
        let spec = AgentTaskRepoLoopSpec {
            schema: None,
            loop_id: "repo-loop-event".to_string(),
            phase: "collect".to_string(),
            config_version: "v1".to_string(),
            metadata: Value::Null,
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            workflows: Vec::new(),
            artifacts: Vec::new(),
            artifact_graph: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: vec![AgentTaskRepoLoopSpecPhase {
                phase: "collect".to_string(),
                transition_id: None,
                on_event_type: Some("artifact.ready".to_string()),
                when_json_path: None,
                actions: vec![AgentTaskLoopPolicyAction::RunGates {
                    bundle_id: "quality".to_string(),
                    entity_id: None,
                }],
            }],
            actions: Vec::new(),
            initial_event: Some(AgentTaskRepoLoopSpecEvent {
                event_type: "artifact.ready".to_string(),
                event_id: Some("artifact-ready-1".to_string()),
                event_key: None,
                entity_id: None,
                payload: Value::Null,
            }),
        };

        let report =
            init_from_spec(ControllerFromSpecRequest { spec }).expect("event-gated spec applied");

        assert_eq!(report.actions.len(), 1);
        assert!(report
            .controller
            .history
            .iter()
            .any(|event| event.event_id == "artifact-ready-1"));
    });
}

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

#[test]
fn run_failure_summary_normalizes_nested_provider_failure() {
    // Mirrors a real run-from-spec envelope: the root cause is buried under
    // results[*].failure_summary + a nested provider diagnostics array.
    let results = vec![serde_json::json!({
        "schema": ACTION_RESULT_SCHEMA,
        "status": "failed",
        "action_id": "spawn-impl",
        "failure_summary": {
            "action_id": "spawn-impl",
            "provider": "wp-codebox",
            "failure_phase": "plugin_activation",
            "run_id": "run-42",
            "diagnostic": "PHP fatal: Uncaught Error: Class 'Foo' not found",
        },
        "execution": {
            "runner_job_id": "job-77",
            "diagnostics": [
                { "message": "PHP fatal: Uncaught Error: Class 'Foo' not found" }
            ],
            "artifacts": [
                { "kind": "log_bundle", "uri": "file:///runs/run-42/codebox.log", "label": "codebox log" }
            ],
        },
    })];
    let status = serde_json::json!({
        "controller": { "phase": "implement" },
    });

    let summary = build_run_failure_summary("loop-9", "action_failed", &results, &status);

    assert_eq!(summary.schema, CONTROLLER_RUN_FAILURE_SUMMARY_SCHEMA);
    assert_eq!(summary.stopped_reason, "action_failed");
    assert_eq!(summary.phase.as_deref(), Some("implement"));
    assert_eq!(summary.owner_surface, "wp_codebox");
    assert_eq!(
        summary.root_blocker,
        "PHP fatal: Uncaught Error: Class 'Foo' not found"
    );
    assert_eq!(summary.action_id.as_deref(), Some("spawn-impl"));
    assert_eq!(summary.provider.as_deref(), Some("wp-codebox"));
    assert_eq!(summary.failure_phase.as_deref(), Some("plugin_activation"));
    assert!(summary.next_command.contains("loop-9"));

    // Durable evidence refs: persisted run evidence, runner job log, per-run
    // evidence, and the declared provider artifact bundle.
    let kinds: Vec<&str> = summary
        .evidence_refs
        .iter()
        .map(|reference| reference.kind.as_str())
        .collect();
    assert!(kinds.contains(&"runner_job_log"), "kinds={kinds:?}");
    assert!(kinds.contains(&"run_evidence"), "kinds={kinds:?}");
    assert!(kinds.contains(&"artifact_bundle"), "kinds={kinds:?}");
    assert!(summary
        .evidence_refs
        .iter()
        .any(|reference| reference.uri.contains("job-77")));
    assert!(summary
        .evidence_refs
        .iter()
        .any(|reference| reference.uri == "file:///runs/run-42/codebox.log"));
}

#[test]
fn run_failure_summary_handles_runner_block_without_diagnostic_message() {
    let results = vec![serde_json::json!({
        "schema": ACTION_RESULT_SCHEMA,
        "status": "blocked_runner_unavailable",
        "action_id": "gate-run",
        "failure_summary": {
            "action_id": "gate-run",
            "diagnostic": "runner `lab-1` is not available for controller action execution",
        },
    })];
    let status = serde_json::json!({ "controller": { "phase": "verify" } });

    let summary = build_run_failure_summary("loop-7", "action_failed", &results, &status);

    assert_eq!(summary.owner_surface, "lab_runner");
    assert!(summary.root_blocker.contains("runner"));
    assert!(summary.next_command.contains("--resume"));
    // Still always surfaces the persisted run-evidence ref.
    assert!(summary
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "run_evidence"));
}

#[test]
fn run_failure_summary_falls_back_to_stopped_reason() {
    // No per-action failure_summary and no diagnostics: the summary still names
    // a sensible blocker derived from the stopped reason.
    let results: Vec<Value> = Vec::new();
    let status = serde_json::json!({ "phase": "plan" });

    let summary = build_run_failure_summary("loop-3", "max_actions_reached", &results, &status);

    assert_eq!(summary.stopped_reason, "max_actions_reached");
    assert_eq!(summary.owner_surface, "homeboy");
    assert!(summary.root_blocker.contains("max-actions"));
    assert_eq!(summary.phase.as_deref(), Some("plan"));
    assert!(!summary.evidence_refs.is_empty());
}

fn plan_stage<'a>(plan: &'a HomeboyPlan, id: &str) -> &'a PlanStep {
    plan.steps
        .iter()
        .find(|step| step.id == id)
        .unwrap_or_else(|| panic!("plan stage '{id}' exists"))
}

#[test]
fn compile_plan_from_spec_derives_stage_dependencies_from_artifact_flow() {
    let mut spec = repo_loop_reconcile_spec("loop-plan-from-spec-flow");
    spec.workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "generation")
        .expect("generation workflow")
        .emits = vec!["static_site_pull_request".to_string()];
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    validation.consumes = vec!["static_site_pull_request".to_string()];
    spec.artifact_graph = vec![AgentTaskRepoLoopSpecArtifactGraphEdge {
        artifact_id: "static_site_pull_request".to_string(),
        from_workflow_id: "generation".to_string(),
        to_workflow_id: "static_validation".to_string(),
        required: true,
    }];

    let report = compile_plan_from_spec(ControllerPlanRequest { spec }).expect("plan compiles");

    assert_eq!(report.schema, EXECUTABLE_PLAN_RESULT_SCHEMA);
    assert_eq!(report.loop_id, "loop-plan-from-spec-flow");
    assert!(report.spec_fingerprint.starts_with("sha256:"));
    assert_eq!(report.plan.kind, PlanKind::AgentTask);

    let generation = plan_stage(&report.plan, "stage:generation");
    assert!(generation.needs.is_empty());
    assert_eq!(
        generation.outputs["emits"],
        json!(["static_site_pull_request"])
    );

    let validation = plan_stage(&report.plan, "stage:static_validation");
    assert_eq!(validation.needs, vec!["stage:generation".to_string()]);
}

#[test]
fn compile_plan_from_spec_synthesizes_homeboy_runtime_artifact_stage() {
    let mut spec = repo_loop_reconcile_spec("loop-plan-from-spec-runtime");
    spec.workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "generation")
        .expect("generation workflow")
        .emits = vec!["static_site_pull_request".to_string()];
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    validation.consumes = vec![
        "static_site_pull_request".to_string(),
        "static_validation_run".to_string(),
    ];
    // Homeboy-owned runtime artifact: no workflow produces it, kind ends in `_run`.
    spec.artifacts.push(AgentTaskRepoLoopSpecArtifact {
        artifact_id: "static_validation_run".to_string(),
        kind: "static_validation_run".to_string(),
        description: Some("Homeboy static validation run".to_string()),
        required: true,
    });
    spec.artifact_graph = vec![AgentTaskRepoLoopSpecArtifactGraphEdge {
        artifact_id: "static_site_pull_request".to_string(),
        from_workflow_id: "generation".to_string(),
        to_workflow_id: "static_validation".to_string(),
        required: true,
    }];

    let report = compile_plan_from_spec(ControllerPlanRequest { spec }).expect("plan compiles");

    assert_eq!(
        report.runtime_artifacts,
        vec!["static_validation_run".to_string()]
    );

    let runtime_stage = plan_stage(&report.plan, "runtime:static_validation_run");
    assert_eq!(runtime_stage.kind, "homeboy_runtime_artifact");
    assert_eq!(
        runtime_stage.inputs["runtime_artifact"]["owner"],
        json!("homeboy_runtime")
    );

    let validation = plan_stage(&report.plan, "stage:static_validation");
    assert!(validation.needs.contains(&"stage:generation".to_string()));
    assert!(validation
        .needs
        .contains(&"runtime:static_validation_run".to_string()));

    assert!(report
        .plan
        .artifacts
        .iter()
        .any(|artifact| artifact.id == "static_validation_run"));
}

#[test]
fn compile_plan_from_spec_rejects_unbacked_artifact_consumption() {
    let mut spec = repo_loop_reconcile_spec("loop-plan-from-spec-unbacked");
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    // Consumes a repo artifact that no workflow emits and no artifact_flow edge backs.
    validation.consumes = vec!["static_site_pull_request".to_string()];

    let error = compile_plan_from_spec(ControllerPlanRequest { spec })
        .expect_err("unbacked consumption rejected");
    let message = error.to_string();
    assert!(
        message.contains("artifact_flow") || message.contains("static_site_pull_request"),
        "unexpected error: {message}"
    );
}

#[test]
fn validate_artifact_flow_bindings_accepts_emit_consume_pairing() {
    let mut spec = repo_loop_reconcile_spec("loop-flow-bindings");
    spec.workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "generation")
        .expect("generation workflow")
        .emits = vec!["static_site_pull_request".to_string()];
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    validation.consumes = vec!["static_site_pull_request".to_string()];

    validate_artifact_flow_bindings(&spec).expect("emit/consume pairing is valid");
}
