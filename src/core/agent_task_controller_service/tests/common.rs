//! Shared test fixtures, dispatch-hook/executor adapters, and helper builders
//! for the `agent_task_controller_service` test groups (split from #5208 god file).
use super::super::*;
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
pub(crate) struct CapturingExecutor {
    pub(crate) observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

#[derive(Clone, Default)]
pub(crate) struct CapturingDispatchHook {
    pub(crate) observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
pub(crate) struct FailingDispatchHook;

#[derive(Clone, Default)]
pub(crate) struct ArtifactDispatchHook {
    pub(crate) observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
pub(crate) struct TypedArtifactHandoffDispatchHook {
    pub(crate) observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
pub(crate) struct CountingFailingDispatchHook {
    pub(crate) observed_requests: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Default)]
pub(crate) struct EvidenceExecutor;

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

pub(crate) fn test_plan() -> AgentTaskPlan {
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

pub(crate) fn repo_loop_reconcile_spec(loop_id: &str) -> AgentTaskRepoLoopSpec {
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

pub(crate) fn reapply_base_then_mutated(
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

pub(crate) fn workflow_action_context(
    report: &ControllerFromSpecReport,
    workflow_id: &str,
) -> Value {
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

pub(crate) fn assert_reconciled_to_pending(
    report: &ControllerFromSpecReport,
    expected_actions: usize,
) {
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

pub(crate) fn changed_resume_spec(loop_id: &str) -> (AgentTaskRepoLoopSpec, AgentTaskRepoLoopSpec) {
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

pub(crate) fn plan_stage<'a>(plan: &'a HomeboyPlan, id: &str) -> &'a PlanStep {
    plan.steps
        .iter()
        .find(|step| step.id == id)
        .unwrap_or_else(|| panic!("plan stage '{id}' exists"))
}
