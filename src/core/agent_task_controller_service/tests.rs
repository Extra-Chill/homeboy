//! Tests split from `agent_task_controller_service` god file (#5208).
use super::*;
use crate::core::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy,
    AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_loop_controller::{
    AgentTaskGateBundle, AgentTaskGateBundleCheck, AgentTaskGateBundleCheckKind,
    AgentTaskGateBundleStatus, AgentTaskLoopFindingPacket, AgentTaskLoopPolicyAction,
    AgentTaskLoopTerminalStatus, AgentTaskLoopWait, AgentTaskLoopWaitStatus,
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
struct ArtifactDispatchHook {
    observed_requests: Arc<Mutex<Vec<Value>>>,
}

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
                tools: Vec::new(),
                abilities: vec!["github_pull_request_publish".to_string()],
                artifacts: vec!["static_site_pull_request".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                inputs: Value::Null,
            },
            AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "static_validation".to_string(),
                agent_id: None,
                prompt: Some("Validate the generated static site.".to_string()),
                tasks: Vec::new(),
                entity_ids: Vec::new(),
                tools: Vec::new(),
                abilities: vec!["static_validation".to_string()],
                artifacts: Vec::new(),
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec!["static_site_pull_request".to_string()],
                gates: Vec::new(),
                metrics: Vec::new(),
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
                tools: Vec::new(),
                abilities: vec!["static_publication".to_string()],
                artifacts: vec!["static_site_pull_request".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec!["static_site_candidate".to_string()],
                gates: Vec::new(),
                metrics: Vec::new(),
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
                tools: vec!["missing-tool".to_string()],
                abilities: Vec::new(),
                artifacts: Vec::new(),
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
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
                tools: vec!["repo-inspector".to_string()],
                abilities: vec!["patch-writer".to_string()],
                artifacts: vec!["candidate-patch".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: vec!["source-tree".to_string()],
                gates: vec!["quality".to_string()],
                metrics: vec!["coverage".to_string()],
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
