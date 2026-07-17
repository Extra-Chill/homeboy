use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

use super::schema::request_schema;
use super::{
    AgentTaskArtifactDeclaration, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome,
    AgentTaskPolicy, AgentTaskSourceRef, AgentTaskWorkspace,
};

/// Provider capability payload used by extension discovery and durable run metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorCapabilities {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub supports_sync_completion: bool,
    #[serde(default)]
    pub supports_async_polling: bool,
    #[serde(default)]
    pub supports_streaming: bool,
    #[serde(default)]
    pub supports_cancel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPreparedWorkspace {
    pub mode: super::AgentTaskWorkspaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskExecutionHandle {
    #[serde(
        default,
        skip_serializing_if = "AgentTaskExecutionHandleKind::is_provider_run"
    )]
    pub kind: AgentTaskExecutionHandleKind,
    pub task_id: String,
    pub backend: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskExecutionHandleKind {
    QueuedRecord,
    LocalPid,
    RunnerJob,
    #[default]
    ProviderRun,
}

impl AgentTaskExecutionHandleKind {
    pub(crate) fn is_provider_run(&self) -> bool {
        matches!(self, Self::ProviderRun)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskStart {
    pub handle: AgentTaskExecutionHandle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentTaskOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskProgress {
    pub handle: AgentTaskExecutionHandle,
    pub state: AgentTaskExecutionState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentTaskProgressEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentTaskOutcome>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskExecutionState {
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}

impl AgentTaskExecutionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskProgressEvent {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRequest {
    #[serde(default = "request_schema")]
    pub schema: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_plan_id: Option<String>,
    pub executor: AgentTaskExecutor,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub inputs: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<AgentTaskSourceRef>,
    #[serde(default)]
    pub workspace: AgentTaskWorkspace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default)]
    pub policy: AgentTaskPolicy,
    #[serde(default)]
    pub limits: AgentTaskLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_declarations: Vec<AgentTaskArtifactDeclaration>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Provider-facing request materialized on the host that executes the task.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskExecutorRequest {
    #[serde(flatten)]
    pub request: AgentTaskRequest,
    pub artifacts_path: PathBuf,
    pub artifacts_path_provenance: AgentTaskArtifactsPathProvenance,
    #[serde(skip)]
    pub(crate) artifacts_root_identity:
        crate::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskArtifactsPathProvenance {
    pub owner: String,
    pub locality: String,
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub task_id: String,
    pub attempt: u32,
}

impl std::ops::Deref for AgentTaskExecutorRequest {
    type Target = AgentTaskRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

impl AgentTaskRequest {
    pub fn canonical_artifact_declarations(&self) -> Vec<AgentTaskArtifactDeclaration> {
        let mut declarations = Vec::new();
        for declaration in &self.artifact_declarations {
            if let Some(declaration) = declaration.canonical() {
                push_artifact_declaration_once(&mut declarations, declaration);
            }
        }

        for expected in &self.expected_artifacts {
            if let Some(declaration) =
                AgentTaskArtifactDeclaration::from_expected_artifact(expected)
            {
                push_artifact_declaration_once(&mut declarations, declaration);
            }
        }

        declarations
    }

    pub fn normalize_artifact_declarations(&mut self) {
        self.artifact_declarations = self.canonical_artifact_declarations();
    }

    /// Compile this agent-task request into a runner execution envelope,
    /// carrying the request itself opaquely in the envelope's `agent_task`
    /// field. The agent-task layer owns this conversion so
    /// `runner_execution_envelope` (core) does not depend on the agent-task
    /// request type.
    pub fn to_runner_execution_envelope(
        &self,
    ) -> homeboy_core::runner_execution_envelope::RunnerExecutionEnvelope {
        use homeboy_core::runner_execution_envelope::{
            RunnerExecutionArtifactDeclaration, RunnerExecutionEnvelope, RunnerExecutionResultRefs,
            RunnerExecutionSource, RUNNER_EXECUTION_ENVELOPE_SCHEMA,
        };
        use homeboy_core::secret_env_plan::SecretEnvPlan;

        let artifact_declarations = self
            .canonical_artifact_declarations()
            .into_iter()
            .map(|declaration| RunnerExecutionArtifactDeclaration {
                name: declaration.name,
                artifact_type: declaration.artifact_type,
                artifact_schema: declaration.artifact_schema,
                path: declaration.path,
                required: declaration.required,
                description: declaration.description,
                metadata: declaration.metadata,
            })
            .collect();
        let secret_env = SecretEnvPlan::from_secret_env_names(self.executor.secret_env.clone());
        let result_refs = RunnerExecutionResultRefs {
            task_id: Some(self.task_id.clone()),
            plan_id: self.parent_plan_id.clone(),
            ..RunnerExecutionResultRefs::default()
        };

        RunnerExecutionEnvelope {
            schema: RUNNER_EXECUTION_ENVELOPE_SCHEMA.to_string(),
            envelope_id: self.task_id.clone(),
            source: RunnerExecutionSource {
                kind: "agent_task".to_string(),
                ref_id: Some(self.task_id.clone()),
            },
            lab_runner_workload: None,
            agent_task: serde_json::to_value(self).ok(),
            secret_env: Some(secret_env),
            env_materialization: None,
            dispatch: None,
            lifecycle: None,
            lifecycle_policy: Default::default(),
            artifact_declarations,
            loop_policy: Default::default(),
            mutation_policy: Default::default(),
            publication_intent: Default::default(),
            result_refs,
            metadata: Value::Null,
        }
    }
}

fn push_artifact_declaration_once(
    declarations: &mut Vec<AgentTaskArtifactDeclaration>,
    declaration: AgentTaskArtifactDeclaration,
) {
    if declarations
        .iter()
        .any(|existing| existing.name == declaration.name)
    {
        return;
    }
    declarations.push(declaration);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskComponentContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
impl AgentTaskRequest {
    pub(crate) fn redacted(&self) -> Self {
        let policy = homeboy_core::redaction::RedactionPolicy::default();
        let mut redacted = self.clone();
        redacted.instructions = policy.redact_string(&redacted.instructions);
        redacted.inputs = policy.redact_json(&redacted.inputs);
        redacted.executor.config = policy.redact_json(&redacted.executor.config);
        redacted.workspace.materialization =
            policy.redact_json(&redacted.workspace.materialization);
        redacted.artifact_declarations = redacted
            .artifact_declarations
            .into_iter()
            .map(|declaration| declaration.redacted_with(&policy))
            .collect();
        redacted.metadata = policy.redact_json(&redacted.metadata);
        redacted
    }
}

#[cfg(test)]
mod runner_execution_envelope_tests {
    use super::*;
    use crate::agent_task::{AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA};
    use homeboy_core::runner_execution_envelope::{
        RunnerExecutionEnvelope, RUNNER_EXECUTION_ENVELOPE_SCHEMA,
    };
    use serde_json::json;

    #[test]
    fn agent_task_request_compiles_secret_env_and_artifacts_into_envelope() {
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            group_key: None,
            parent_plan_id: Some("plan-1".to_string()),
            executor: AgentTaskExecutor {
                backend: "sandbox".to_string(),
                selector: Some("provider-a".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: vec!["TOKEN_A".to_string()],
                model: None,
                config: Value::Null,
            },
            instructions: "Run the task.".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                mode: AgentTaskWorkspaceMode::Materialized,
                root: Some("/workspace/project".to_string()),
                ..AgentTaskWorkspace::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: Default::default(),
            expected_artifacts: vec!["patch".to_string()],
            artifact_declarations: vec![AgentTaskArtifactDeclaration {
                name: "report".to_string(),
                artifact_type: Some("json".to_string()),
                artifact_schema: Some("example/report/v1".to_string()),
                path: Some("artifacts/report.json".to_string()),
                required: true,
                description: None,
                metadata: Value::Null,
            }],
            metadata: Value::Null,
        };

        let envelope = request.to_runner_execution_envelope();

        assert_eq!(envelope.schema, RUNNER_EXECUTION_ENVELOPE_SCHEMA);
        assert_eq!(envelope.source.kind, "agent_task");
        assert_eq!(envelope.result_refs.task_id.as_deref(), Some("task-1"));
        assert_eq!(envelope.result_refs.plan_id.as_deref(), Some("plan-1"));
        assert_eq!(
            envelope
                .secret_env
                .expect("secret env plan")
                .secret_env_names(),
            vec!["TOKEN_A".to_string()]
        );
        assert_eq!(
            envelope
                .artifact_declarations
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect::<Vec<_>>(),
            vec!["report", "patch"]
        );
    }

    #[test]
    fn extensions_shaped_runtime_fixture_compiles_without_losing_runtime_selection() {
        let request: AgentTaskRequest = serde_json::from_value(json!({
            "schema": AGENT_TASK_REQUEST_SCHEMA,
            "task_id": "task-runtime-fixture",
            "executor": {
                "backend": "legacy-backend",
                "selector": "legacy-provider",
                "runtime": {
                    "runtime_id": "runtime-1",
                    "backend": "runtime-backend",
                    "selector": "runtime-provider",
                    "provider": "oauth-provider",
                    "model": "model-a",
                    "substrate_ref": "sandbox://run/1"
                },
                "secret_env": ["TOKEN_A"],
                "required_capabilities": ["structured_output"]
            },
            "instructions": "Execute the fixture.",
            "workspace": {
                "mode": "materialized",
                "root": "/workspace/project"
            },
            "expected_artifacts": ["patch"],
            "artifact_declarations": [
                {
                    "name": "report",
                    "type": "json",
                    "artifact_schema": "example/report/v1",
                    "path": "artifacts/report.json",
                    "required": true
                }
            ]
        }))
        .expect("decode extensions-shaped fixture");

        let selection = request.executor.runtime_selection();
        let envelope = request.to_runner_execution_envelope();
        let encoded = serde_json::to_value(&envelope).expect("serialize envelope");
        let decoded: RunnerExecutionEnvelope =
            serde_json::from_value(encoded).expect("decode envelope");

        assert_eq!(selection.runtime_id.as_deref(), Some("runtime-1"));
        assert_eq!(
            selection.executor_backend.as_deref(),
            Some("runtime-backend")
        );
        assert_eq!(
            selection.executor_provider_id.as_deref(),
            Some("runtime-provider")
        );
        let decoded_request: AgentTaskRequest =
            serde_json::from_value(decoded.agent_task.expect("agent task carried opaquely"))
                .expect("decode carried agent task request");
        assert_eq!(decoded_request.executor.runtime_id(), Some("runtime-1"));
        assert_eq!(
            decoded
                .secret_env
                .expect("secret env plan")
                .secret_env_names(),
            vec!["TOKEN_A".to_string()]
        );
        assert_eq!(decoded.artifact_declarations.len(), 2);
    }
}
