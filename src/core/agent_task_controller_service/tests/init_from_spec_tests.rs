use super::super::*;
use super::*;

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
