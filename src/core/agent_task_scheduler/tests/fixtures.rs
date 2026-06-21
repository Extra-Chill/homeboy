//! Shared test fixtures: mock executor adapters and plan/request/outcome
//! builders reused across the scheduler test suites.

use super::super::*;
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactDeclaration, AgentTaskExecutor, AgentTaskLimits,
    AgentTaskPolicy, AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct RetryOnceExecutor {
    pub(super) attempts: Arc<AtomicUsize>,
}

#[derive(Default)]
pub(super) struct EmptyIncompleteThenSuccessExecutor {
    pub(super) attempts: Arc<AtomicUsize>,
}

pub(super) struct EmptyIncompleteExecutor;

pub(super) struct NestedFailedStatusExecutor;

pub(super) struct NestedTerminalStateFailedExecutor;

pub(super) struct NestedAgentResultFailedExecutor;

pub(super) struct SuccessMissingRequiredArtifactsExecutor;

pub(super) struct SuccessEmptyRequiredTypedArtifactExecutor {
    pub(super) artifact_path: std::path::PathBuf,
}

impl AgentTaskExecutorAdapter for RetryOnceExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        let status = if attempt == 1 {
            AgentTaskOutcomeStatus::Failed
        } else {
            AgentTaskOutcomeStatus::Succeeded
        };

        outcome(request.task_id, status)
    }
}

impl AgentTaskExecutorAdapter for EmptyIncompleteThenSuccessExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == 1 {
            return empty_incomplete_outcome(request.task_id);
        }

        outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
    }
}

impl AgentTaskExecutorAdapter for EmptyIncompleteExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        empty_incomplete_outcome(request.task_id)
    }
}

pub(super) struct NestedOutputsIncompleteExecutor;

pub(super) struct RuntimeBundleOutcomeExecutor {
    pub(super) patch_path: std::path::PathBuf,
    pub(super) transcript_path: std::path::PathBuf,
}

impl AgentTaskExecutorAdapter for NestedOutputsIncompleteExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        nested_outputs_incomplete_outcome(request.task_id)
    }
}

impl AgentTaskExecutorAdapter for RuntimeBundleOutcomeExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(
                "Sample runtime agent task did not produce required typed artifacts: patch, agent_result, transcript."
                    .to_string(),
            ),
            failure_classification: Some(AgentTaskFailureClassification::Provider),
            artifacts: vec![
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "sample-runtime-patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("patch.diff".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(self.patch_path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: Some(24),
                    sha256: Some("sha256:patch".to_string()),
                    metadata: json!({
                        "artifact": "files/patch.diff",
                        "provider_kind": "sample-runtime-patch",
                        "role": "patch"
                    }),
                },
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "sample-runtime-transcript".to_string(),
                    kind: "sample-runtime-transcript".to_string(),
                    name: Some("transcript.json".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(self.transcript_path.display().to_string()),
                    url: None,
                    mime: Some("application/json".to_string()),
                    size_bytes: Some(13),
                    sha256: None,
                    metadata: json!({ "artifact": "files/transcript.json" }),
                },
            ],
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "sample-runtime-artifact-bundle".to_string(),
                uri: self
                    .patch_path
                    .parent()
                    .and_then(|path| path.parent())
                    .unwrap_or_else(|| self.patch_path.as_path())
                    .display()
                    .to_string(),
                label: Some("Sample runtime artifact bundle".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "agent_task.required_typed_artifacts_missing".to_string(),
                message: "Sample runtime agent task did not produce required typed artifacts: patch, agent_result, transcript."
                    .to_string(),
                data: Value::Null,
            }],
            outputs: json!({ "runtime_status": "succeeded" }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

impl AgentTaskExecutorAdapter for NestedFailedStatusExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded);
        outcome.outputs = json!({
            "provider_run_result": {
                "job_status": "failed - completion_required_tool_unavailable",
                "completion_status": "partial"
            }
        });
        outcome
    }
}

impl AgentTaskExecutorAdapter for NestedTerminalStateFailedExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded);
        outcome.outputs = json!({
            "provider_run_result": {
                "completion_status": "partial",
                "wait_result": {
                    "terminal_state": "failed - completion_required_tool_unavailable"
                }
            }
        });
        outcome
    }
}

impl AgentTaskExecutorAdapter for NestedAgentResultFailedExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded);
        outcome.typed_artifacts.push(AgentTaskTypedArtifact {
            name: "agent_result".to_string(),
            artifact_type: Some("json".to_string()),
            artifact_schema: Some(AGENT_TASK_OUTCOME_SCHEMA.to_string()),
            payload: json!({
                "schema": AGENT_TASK_OUTCOME_SCHEMA,
                "status": "failed",
                "summary": "provider run missed required typed artifacts"
            }),
            artifact: None,
            metadata: Value::Null,
        });
        outcome
    }
}

impl AgentTaskExecutorAdapter for SuccessMissingRequiredArtifactsExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
    }
}

impl AgentTaskExecutorAdapter for SuccessEmptyRequiredTypedArtifactExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded);
        let artifact = AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "empty-patch".to_string(),
            kind: "patch".to_string(),
            name: Some("patch.diff".to_string()),
            label: None,
            role: None,
            semantic_key: None,
            path: Some(self.artifact_path.display().to_string()),
            url: None,
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(0),
            sha256: None,
            metadata: json!({ "role": "patch" }),
        };
        outcome.typed_artifacts.push(AgentTaskTypedArtifact {
            name: "patch".to_string(),
            artifact_type: Some("file".to_string()),
            artifact_schema: None,
            payload: json!({
                "artifact_id": artifact.id.clone(),
                "kind": artifact.kind.clone(),
                "path": artifact.path.clone(),
                "size_bytes": artifact.size_bytes,
            }),
            artifact: Some(artifact),
            metadata: Value::Null,
        });
        outcome
    }
}

pub(super) struct RecordingExecutor {
    pub(super) statuses: HashMap<String, AgentTaskOutcomeStatus>,
    pub(super) delay: Duration,
    pub(super) running: Arc<AtomicUsize>,
    pub(super) max_seen: Arc<AtomicUsize>,
    pub(super) cancel_calls: Arc<Mutex<Vec<String>>>,
}

pub(super) struct OutputTemplateExecutor {
    pub(super) observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
    pub(super) include_issue_number: bool,
}

impl AgentTaskExecutorAdapter for OutputTemplateExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        self.observed
            .lock()
            .expect("observed requests")
            .push(request.clone());
        let metadata = if request.task_id == "idea" && self.include_issue_number {
            json!({ "github": { "issue_number": 3447 } })
        } else {
            json!({})
        };
        let outputs = if request.task_id == "idea" && self.include_issue_number {
            json!({ "issue_number": 3447 })
        } else {
            Value::Null
        };

        let artifacts = if request.task_id == "idea" && self.include_issue_number {
            vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "concept".to_string(),
                kind: "concept_packet".to_string(),
                name: Some("concept.json".to_string()),
                label: None,
                role: None,
                semantic_key: None,
                path: Some("artifacts/concept.json".to_string()),
                url: None,
                mime: Some("application/json".to_string()),
                size_bytes: None,
                sha256: Some("sha256:concept".to_string()),
                metadata: json!({
                    "payload_schema": "example/concept-packet/v1",
                    "payload": { "title": "Demo concept" }
                }),
            }]
        } else {
            Vec::new()
        };

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts,
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs,
            workflow: None,
            follow_up: None,
            metadata,
        }
    }
}

impl RecordingExecutor {
    pub(super) fn new(statuses: HashMap<String, AgentTaskOutcomeStatus>, delay: Duration) -> Self {
        Self {
            statuses,
            delay,
            running: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
            cancel_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl AgentTaskExecutorAdapter for RecordingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_seen.fetch_max(running, Ordering::SeqCst);

        let deadline = Instant::now() + self.delay;
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }

        self.running.fetch_sub(1, Ordering::SeqCst);
        outcome(
            request.task_id.clone(),
            *self
                .statuses
                .get(&request.task_id)
                .unwrap_or(&AgentTaskOutcomeStatus::Succeeded),
        )
    }

    fn cancel(&self, task_id: &str) {
        self.cancel_calls
            .lock()
            .expect("cancel calls")
            .push(task_id.to_string());
    }
}

pub(super) fn plan_with_tasks(count: usize) -> AgentTaskPlan {
    let mut tasks = Vec::new();
    for index in 1..=count {
        tasks.push(request(&format!("task-{index}")));
    }
    AgentTaskPlan::new("plan-1", tasks)
}

pub(super) fn plan_with_required_artifacts(names: &[&str]) -> AgentTaskPlan {
    let mut task = request("task-1");
    task.artifact_declarations = names
        .iter()
        .map(|name| AgentTaskArtifactDeclaration {
            name: (*name).to_string(),
            artifact_type: None,
            artifact_schema: None,
            path: None,
            required: true,
            description: None,
            metadata: Value::Null,
        })
        .collect();
    AgentTaskPlan::new("plan-1", vec![task])
}

pub(super) fn request(task_id: &str) -> AgentTaskRequest {
    AgentTaskRequest {
        schema: crate::core::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        group_key: None,
        parent_plan_id: Some("plan-1".to_string()),
        executor: AgentTaskExecutor {
            backend: "test".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: Value::Null,
        },
        instructions: "do the task".to_string(),
        inputs: Value::Null,
        source_refs: Vec::new(),
        workspace: AgentTaskWorkspace::default(),
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        metadata: Value::Null,
    }
}

pub(super) fn outcome(task_id: String, status: AgentTaskOutcomeStatus) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status,
        summary: Some(format!("{status:?}")),
        failure_classification: match status {
            AgentTaskOutcomeStatus::Failed => Some(AgentTaskFailureClassification::ExecutionFailed),
            _ => None,
        },
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "log".to_string(),
            uri: "artifact://task/log".to_string(),
            label: Some("task log".to_string()),
        }],
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: json!({}),
    }
}

pub(super) fn empty_incomplete_outcome(task_id: String) -> AgentTaskOutcome {
    let mut outcome = outcome(task_id, AgentTaskOutcomeStatus::Succeeded);
    outcome.summary = Some("provider wrapper exited successfully".to_string());
    outcome.outputs = json!({
        "provider_run_result": {
            "completed": false,
            "reply": "",
            "messages": [],
            "tool_calls": []
        }
    });
    outcome
}

pub(super) fn nested_outputs_incomplete_outcome(task_id: String) -> AgentTaskOutcome {
    let mut outcome = outcome(task_id, AgentTaskOutcomeStatus::Succeeded);
    outcome.summary =
        Some("Agent sandbox completed successfully without actionable file changes.".to_string());
    outcome.outputs = json!({
        "provider_run_result": {
            "success": true,
            "status": "completed",
            "outputs": {
                "reply": "",
                "messages": [{ "role": "user", "content": "cook the issue" }],
                "completed": false,
                "run_id": "run_abc"
            },
            "structured_artifacts": [],
            "diagnostics": {}
        }
    });
    outcome
}
