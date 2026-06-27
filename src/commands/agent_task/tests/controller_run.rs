//! Agent-task command controller run/run-next execution tests.

use super::support::*;
use crate::core::agent_task::AgentTaskTypedArtifact;

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
fn controller_dispatch_runtime_component_contracts_reach_spawned_agent_task_request() {
    with_temp_home(|| {
        let component = tempfile::tempdir().expect("agents-api checkout");
        init_runtime_component_checkout(component.path());
        let component_path = component.path().display().to_string();
        let observed_request = Arc::new(Mutex::new(None));
        let mut controller = agent_task_loop_controller::create_controller(
            "loop-controller-runtime-components",
            "repair",
            "v1",
        )
        .expect("controller created");
        controller.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "workflow:generation".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "dispatch": {
                        "prompt": "Run the generic workflow.",
                        "backend": "fixture",
                        "client_context": json!({
                            "schema": "homeboy/repo-loop-workflow-context/v1",
                            "workflow_id": "generation",
                            "inputs": {
                                "runtime_component_contracts": [{
                                    "slug": "agents-api",
                                    "path": component_path,
                                    "loadAs": "plugin",
                                    "activate": true,
                                    "opaque": { "source": "workflow-input" }
                                }]
                            }
                        }).to_string()
                    }
                }),
            },
            "repo loop spec workflow",
        );
        agent_task_loop_controller::write_controller(&controller).expect("controller written");

        let (value, exit_code) = controller_run_next_with_executor(
            "loop-controller-runtime-components".to_string(),
            ArtifactCapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("controller action executed");

        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 0, "{value:#}");
        assert_eq!(observed.component_contracts.len(), 1);
        assert_eq!(
            observed.component_contracts[0].slug.as_deref(),
            Some("agents-api")
        );
        assert_eq!(
            observed.component_contracts[0].path.as_deref(),
            Some(component_path.as_str())
        );
        assert_eq!(
            observed.component_contracts[0].load_as.as_deref(),
            Some("plugin")
        );
        assert_eq!(observed.component_contracts[0].activate, Some(true));
        assert_eq!(
            observed.component_contracts[0].extra["opaque"]["source"],
            "workflow-input"
        );
    });
}

#[test]
fn controller_run_from_spec_materializes_runs_bounded_actions_and_returns_status() {
    with_temp_home(|| {
        let (value, exit_code) = controller_run_from_spec_with_test_executor(
            AgentTaskControllerRunFromSpecArgs {
                spec: serde_json::to_string(&json!({
                    "loop_id": "run-from-spec-loop",
                    "workflows": [
                        { "workflow_id": "brief", "prompt": "Draft the brief." },
                        { "workflow_id": "review", "prompt": "Review the brief." }
                    ]
                }))
                .expect("spec json"),
                inputs: Some(
                    serde_json::to_string(&json!({
                        "inputs": { "topic": "deterministic loops" },
                        "metadata": { "run_key": "run-from-spec-test" }
                    }))
                    .expect("inputs json"),
                ),
                policy_results: Vec::new(),
                max_actions: 1,
                reconcile_stale: false,
                replace: false,
                fork: false,
                resume_existing: false,
                dispatch: AgentTaskControllerDispatchArgs {
                    dispatch_backend: Some("fixture".to_string()),
                    dispatch_selector: None,
                    dispatch_model: None,
                    dispatch_provider_config: None,
                },
            },
            ArtifactCapturingExecutor::default(),
        )
        .expect("run from spec");

        assert_eq!(exit_code, 0, "{value:#}");
        assert_eq!(
            value["schema"],
            "homeboy/agent-task-loop-controller-run-from-spec-result/v1"
        );
        assert_eq!(value["loop_id"], "run-from-spec-loop");
        assert_eq!(value["max_actions"], 1);
        assert_eq!(value["stopped_reason"], "max_actions_reached");
        assert_eq!(
            value["materialization"]["spec"]["workflows"][0]["inputs"]["topic"],
            "deterministic loops"
        );
        assert_eq!(value["from_spec"]["initialized"], true);
        assert_eq!(value["results"].as_array().expect("results").len(), 1);
        assert_eq!(value["results"][0]["claimed"], true);
        assert_eq!(
            value["status"]["controller"]["next_actions"][0]["status"],
            "completed"
        );
        assert_eq!(
            value["status"]["controller"]["next_actions"][1]["status"],
            "pending"
        );
        assert_eq!(
            value["status"]["controller"]["metadata"]["run_key"],
            "run-from-spec-test"
        );
    });
}

#[test]
fn controller_run_from_spec_persists_dispatch_defaults_for_generated_actions() {
    with_temp_home(|| {
        let observed_request = Arc::new(Mutex::new(None));
        let (value, exit_code) = controller_run_from_spec_with_test_executor(
            AgentTaskControllerRunFromSpecArgs {
                spec: serde_json::to_string(&json!({
                    "loop_id": "run-from-spec-dispatch-defaults-loop",
                    "workflows": [{ "workflow_id": "cook", "prompt": "Cook." }]
                }))
                .expect("spec json"),
                inputs: None,
                policy_results: Vec::new(),
                max_actions: 1,
                reconcile_stale: false,
                replace: false,
                fork: false,
                resume_existing: false,
                dispatch: AgentTaskControllerDispatchArgs {
                    dispatch_backend: Some("fixture".to_string()),
                    dispatch_selector: None,
                    dispatch_model: Some("gpt-test".to_string()),
                    dispatch_provider_config: Some(
                        r#"{"runtime_wordpress_version":"6.9"}"#.to_string(),
                    ),
                },
            },
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("run from spec");

        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 1, "{value:#}");
        assert!(!value
            .to_string()
            .contains("requires --backend because no default backend policy is configured"));
        assert_eq!(observed.executor.backend, "fixture");
        assert_eq!(observed.executor.selector, None);
        assert_eq!(observed.executor.model.as_deref(), Some("gpt-test"));
        assert_eq!(observed.executor.config["runtime_wordpress_version"], "6.9");
        assert_eq!(
            value["from_spec"]["actions"][0]["action"]["request"]["dispatch"]["backend"],
            "fixture"
        );
        assert_eq!(
            value["materialization"]["spec"]["metadata"]["dispatch_defaults"]["backend"],
            "fixture"
        );
    });
}

#[test]
fn controller_run_from_spec_preserves_runtime_execution_and_components() {
    with_temp_home(|| {
        let component = tempfile::tempdir().expect("agents-api checkout");
        init_runtime_component_checkout(component.path());
        let component_path = component.path().display().to_string();
        let observed_request = Arc::new(Mutex::new(None));
        let (value, exit_code) = controller_run_from_spec_with_test_executor(
            AgentTaskControllerRunFromSpecArgs {
                spec: serde_json::to_string(&json!({
                    "loop_id": "run-from-spec-runtime-execution-loop",
                    "workflows": [{
                        "workflow_id": "store-idea",
                        "prompt": "Generate a concept packet.",
                        "runtime_execution": {
                            "kind": "bundle",
                            "ability": "runtime-package/run",
                            "input": {
                                "package": { "source": "bundles/store-idea-agent" },
                                "workflow": { "id": "store-idea-artifact-flow" },
                                "input": { "wait_for_completion": true }
                            }
                        },
                        "emits": ["concept_packet"]
                    }],
                    "artifacts": [{
                        "artifact_id": "concept_packet",
                        "kind": "wp-site-generator/ConceptPacket/v1",
                        "required": true
                    }]
                }))
                .expect("spec json"),
                inputs: Some(
                    serde_json::to_string(&json!({
                        "inputs": {
                            "runtime_config": {
                                "component_contracts": [{
                                    "slug": "agents-api",
                                    "path": component_path,
                                    "loadAs": "plugin",
                                    "activate": true
                                }]
                            }
                        }
                    }))
                    .expect("inputs json"),
                ),
                policy_results: Vec::new(),
                max_actions: 1,
                reconcile_stale: false,
                replace: false,
                fork: false,
                resume_existing: false,
                dispatch: AgentTaskControllerDispatchArgs {
                    dispatch_backend: Some("fixture".to_string()),
                    dispatch_selector: None,
                    dispatch_model: Some("gpt-cli".to_string()),
                    dispatch_provider_config: Some(
                        json!({
                            "provider": "codex",
                            "model": "gpt-config",
                            "options": { "reasoning_effort": "high" }
                        })
                        .to_string(),
                    ),
                },
            },
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("run from spec");

        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 1, "{value:#}");
        assert_eq!(
            observed.inputs["runtime_task"]["ability"],
            "runtime-package/run"
        );
        assert_eq!(
            observed.inputs["runtime_task"]["input"]["package"]["source"],
            "bundles/store-idea-agent"
        );
        assert_eq!(
            observed.inputs["runtime_task"]["input"]["provider"],
            "codex"
        );
        assert_eq!(observed.inputs["runtime_task"]["input"]["model"], "gpt-cli");
        assert_eq!(
            observed.inputs["runtime_task"]["input"]["options"]["reasoning_effort"],
            "high"
        );
        assert_eq!(observed.component_contracts.len(), 1);
        assert_eq!(
            observed.component_contracts[0].slug.as_deref(),
            Some("agents-api")
        );
        assert_eq!(
            observed.component_contracts[0].path.as_deref(),
            Some(component_path.as_str())
        );
        assert_eq!(
            value["materialization"]["spec"]["workflows"][0]["runtime_execution"]["ability"],
            "runtime-package/run"
        );
    });
}

#[test]
fn controller_run_from_spec_rejects_unbounded_zero_max_actions() {
    with_temp_home(|| {
        let error = controller_run_from_spec_with_test_executor(
            AgentTaskControllerRunFromSpecArgs {
                spec: serde_json::to_string(&json!({
                    "loop_id": "run-from-spec-zero-loop",
                    "workflows": [{ "workflow_id": "brief", "prompt": "Draft." }]
                }))
                .expect("spec json"),
                inputs: None,
                policy_results: Vec::new(),
                max_actions: 0,
                reconcile_stale: false,
                replace: false,
                fork: false,
                resume_existing: false,
                dispatch: AgentTaskControllerDispatchArgs {
                    dispatch_backend: Some("fixture".to_string()),
                    dispatch_selector: None,
                    dispatch_model: None,
                    dispatch_provider_config: None,
                },
            },
            CapturingExecutor::default(),
        )
        .expect_err("zero max-actions is rejected");

        assert_eq!(error.details["field"], "max-actions");
        assert!(error.message.contains("greater than zero"));
    });
}

#[test]
fn controller_run_from_spec_rejects_command_runtime_without_command_kind() {
    with_temp_home(|| {
        let observed_request = Arc::new(Mutex::new(None));
        let error = controller_run_from_spec_with_test_executor(
            AgentTaskControllerRunFromSpecArgs {
                spec: serde_json::to_string(&json!({
                    "loop_id": "run-from-spec-command-missing-kind",
                    "workflows": [{
                        "workflow_id": "static-validation",
                        "prompt": "Run static validation.",
                        "runtime_execution": {
                            "command": "/bin/sh",
                            "args": ["-c", "printf ok"]
                        }
                    }]
                }))
                .expect("spec json"),
                inputs: None,
                policy_results: Vec::new(),
                max_actions: 1,
                reconcile_stale: false,
                replace: false,
                fork: false,
                resume_existing: false,
                dispatch: AgentTaskControllerDispatchArgs {
                    dispatch_backend: Some("fixture".to_string()),
                    dispatch_selector: None,
                    dispatch_model: None,
                    dispatch_provider_config: None,
                },
            },
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect_err("command-shaped runtime execution is rejected before dispatch");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert_eq!(error.details["field"], "workflows[].runtime_execution.kind");
        assert_eq!(error.details["id"], "static-validation");
        assert!(error.message.contains("kind: command"), "{error}");
        assert!(observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none());
    });
}

fn run_from_spec_proof_args(
    loop_id: &str,
    prompt: &str,
    reconcile_stale: bool,
) -> AgentTaskControllerRunFromSpecArgs {
    AgentTaskControllerRunFromSpecArgs {
        spec: serde_json::to_string(&json!({
            "loop_id": loop_id,
            "workflows": [{ "workflow_id": "brief", "prompt": prompt }]
        }))
        .expect("spec json"),
        inputs: None,
        policy_results: Vec::new(),
        max_actions: 1,
        reconcile_stale,
        replace: false,
        fork: false,
        resume_existing: false,
        dispatch: AgentTaskControllerDispatchArgs {
            dispatch_backend: Some("fixture".to_string()),
            dispatch_selector: None,
            dispatch_model: None,
            dispatch_provider_config: None,
        },
    }
}

#[test]
fn controller_run_from_spec_guards_stale_state_with_actionable_diagnostics() {
    with_temp_home(|| {
        // First proof run persists base controller state.
        controller_run_from_spec_with_test_executor(
            run_from_spec_proof_args("run-from-spec-stale-guard", "Draft the brief.", false),
            ArtifactCapturingExecutor::default(),
        )
        .expect("base proof run");

        // A changed spec under the same loop id must NOT silently reuse the
        // stale base controller state on a run-scoped proof run.
        let error = controller_run_from_spec_with_test_executor(
            run_from_spec_proof_args("run-from-spec-stale-guard", "Rewrite the brief.", false),
            ArtifactCapturingExecutor::default(),
        )
        .expect_err("stale state is guarded");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error
            .message
            .contains("refusing to reuse stale persisted controller state"));
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
fn controller_run_from_spec_reconcile_stale_recovers_without_manual_cleanup() {
    with_temp_home(|| {
        controller_run_from_spec_with_test_executor(
            run_from_spec_proof_args("run-from-spec-reconcile", "Draft the brief.", false),
            ArtifactCapturingExecutor::default(),
        )
        .expect("base proof run");

        // The one-flag safe mode resets run-scoped state automatically.
        let (value, exit_code) = controller_run_from_spec_with_test_executor(
            run_from_spec_proof_args("run-from-spec-reconcile", "Rewrite the brief.", true),
            ArtifactCapturingExecutor::default(),
        )
        .expect("reconcile-stale recovers the proof run");

        assert_eq!(exit_code, 0, "{value:#}");
        assert_eq!(value["loop_id"], "run-from-spec-reconcile");
        assert_eq!(value["from_spec"]["initialized"], true);
        assert_eq!(value["from_spec"]["resume_state"]["action"], "replacing");
        assert_eq!(
            value["from_spec"]["resume_state"]["resolution"],
            "reconcile-stale"
        );
        assert_eq!(
            value["from_spec"]["resume_state"]["fingerprint_match"],
            false
        );
    });
}

#[test]
fn controller_run_from_spec_fork_isolates_repeated_replays_from_stale_child_runs() {
    with_temp_home(|| {
        controller_run_from_spec_with_test_executor(
            run_from_spec_proof_args("run-from-spec-fork-isolation", "Draft the brief.", false),
            ArtifactCapturingExecutor::default(),
        )
        .expect("base proof run");

        let mut first_fork_args =
            run_from_spec_proof_args("run-from-spec-fork-isolation", "Rewrite the brief.", false);
        first_fork_args.fork = true;
        let mut second_fork_args =
            run_from_spec_proof_args("run-from-spec-fork-isolation", "Rewrite the brief.", false);
        second_fork_args.fork = true;

        let (first, first_exit_code) = controller_run_from_spec_with_test_executor(
            first_fork_args,
            ArtifactCapturingExecutor::default(),
        )
        .expect("first fork run");
        let (second, second_exit_code) = controller_run_from_spec_with_test_executor(
            second_fork_args,
            ArtifactCapturingExecutor::default(),
        )
        .expect("second fork run");

        assert_eq!(first_exit_code, 0, "{first:#}");
        assert_eq!(second_exit_code, 0, "{second:#}");
        assert_ne!(first["loop_id"], second["loop_id"]);
        assert_eq!(first["from_spec"]["resume_state"]["action"], "forking");
        assert_eq!(second["from_spec"]["resume_state"]["action"], "forking");
        assert_eq!(first["results"][0]["claimed"], true);
        assert_eq!(second["results"][0]["claimed"], true);
        assert_ne!(
            first["results"][0]["execution"]["result"]["run_id"],
            second["results"][0]["execution"]["result"]["run_id"]
        );
        assert_eq!(first["status"]["controller"]["loop_id"], first["loop_id"]);
        assert_eq!(second["status"]["controller"]["loop_id"], second["loop_id"]);
    });
}

#[derive(Clone, Default)]
struct ArtifactCapturingExecutor {
    observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

impl AgentTaskExecutorAdapter for ArtifactCapturingExecutor {
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
            typed_artifacts: vec![
                AgentTaskTypedArtifact {
                    name: "patch".to_string(),
                    artifact_type: Some("diff".to_string()),
                    artifact_schema: None,
                    payload: json!({ "content": "diff --git a/README.md b/README.md" }),
                    artifact: None,
                    metadata: Value::Null,
                },
                AgentTaskTypedArtifact {
                    name: "transcript".to_string(),
                    artifact_type: Some("text".to_string()),
                    artifact_schema: None,
                    payload: json!({ "content": "ok" }),
                    artifact: None,
                    metadata: Value::Null,
                },
            ],
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
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
