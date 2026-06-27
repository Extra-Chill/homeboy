use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command_contract::{RunnerWorkload, RunnerWorkloadArtifactRef};
use crate::core::agent_task::{AgentTaskArtifactDeclaration, AgentTaskRequest};
use crate::core::secret_env_plan::SecretEnvPlan;

pub const RUNNER_EXECUTION_ENVELOPE_SCHEMA: &str = "homeboy/runner-execution-envelope/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunnerExecutionEnvelope {
    #[serde(default = "runner_execution_envelope_schema")]
    pub schema: String,
    pub envelope_id: String,
    #[serde(default)]
    pub source: RunnerExecutionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_workload: Option<RunnerWorkload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task: Option<AgentTaskRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_env: Option<SecretEnvPlan>,
    #[serde(default)]
    pub lifecycle_policy: RunnerExecutionLifecyclePolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_declarations: Vec<RunnerExecutionArtifactDeclaration>,
    #[serde(default)]
    pub loop_policy: RunnerExecutionLoopPolicy,
    #[serde(default)]
    pub mutation_policy: RunnerExecutionMutationPolicy,
    #[serde(default)]
    pub publication_intent: RunnerExecutionPublicationIntent,
    #[serde(default)]
    pub result_refs: RunnerExecutionResultRefs,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
}

impl Default for RunnerExecutionSource {
    fn default() -> Self {
        Self {
            kind: "unspecified".to_string(),
            ref_id: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionLifecyclePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunnerExecutionArtifactDeclaration {
    pub name: String,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionLoopPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionMutationPolicy {
    #[serde(default)]
    pub capture_patch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_flag: Option<String>,
    #[serde(default)]
    pub allow_dirty_workspace: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionPublicationIntent {
    #[serde(default)]
    pub publish: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionResultRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<RunnerWorkloadArtifactRef>,
}

impl RunnerExecutionEnvelope {
    pub fn planned(envelope_id: impl Into<String>, source_kind: impl Into<String>) -> Self {
        let envelope_id = envelope_id.into();

        Self {
            schema: RUNNER_EXECUTION_ENVELOPE_SCHEMA.to_string(),
            envelope_id: envelope_id.clone(),
            source: RunnerExecutionSource {
                kind: source_kind.into(),
                ref_id: Some(envelope_id),
            },
            runner_workload: None,
            agent_task: None,
            secret_env: None,
            lifecycle_policy: RunnerExecutionLifecyclePolicy::default(),
            artifact_declarations: Vec::new(),
            loop_policy: RunnerExecutionLoopPolicy::default(),
            mutation_policy: RunnerExecutionMutationPolicy::default(),
            publication_intent: RunnerExecutionPublicationIntent::default(),
            result_refs: RunnerExecutionResultRefs::default(),
            metadata: Value::Null,
        }
    }

    pub fn with_source_ref(mut self, ref_id: impl Into<String>) -> Self {
        self.source.ref_id = Some(ref_id.into());
        self
    }

    pub fn with_secret_env(mut self, secret_env: SecretEnvPlan) -> Self {
        self.secret_env = Some(secret_env);
        self
    }

    pub fn with_lifecycle_policy(
        mut self,
        lifecycle_policy: RunnerExecutionLifecyclePolicy,
    ) -> Self {
        self.lifecycle_policy = lifecycle_policy;
        self
    }

    pub fn with_artifact_declarations(
        mut self,
        artifact_declarations: impl IntoIterator<Item = RunnerExecutionArtifactDeclaration>,
    ) -> Self {
        self.artifact_declarations = artifact_declarations.into_iter().collect();
        self
    }

    pub fn with_result_refs(mut self, result_refs: RunnerExecutionResultRefs) -> Self {
        self.result_refs = result_refs;
        self
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn from_runner_workload(workload: RunnerWorkload) -> Self {
        let mutation_policy = RunnerExecutionMutationPolicy {
            capture_patch: workload.mutation_policy.capture_patch,
            mutation_flag: workload.mutation_policy.mutation_flag.clone(),
            allow_dirty_workspace: workload.mutation_policy.allow_dirty_lab_workspace,
        };
        let result_refs = RunnerExecutionResultRefs {
            plan_id: Some(workload.result_refs.plan_id.clone()),
            job_id: workload.result_refs.job_id.clone(),
            run_id: workload.result_refs.proof_id.clone(),
            mirror_run_id: workload.result_refs.mirror_run_id.clone(),
            artifacts: workload.result_refs.artifacts.clone(),
            ..RunnerExecutionResultRefs::default()
        };

        Self {
            schema: RUNNER_EXECUTION_ENVELOPE_SCHEMA.to_string(),
            envelope_id: workload.workload_id.clone(),
            source: RunnerExecutionSource {
                kind: "runner_workload".to_string(),
                ref_id: Some(workload.workload_id.clone()),
            },
            runner_workload: Some(workload),
            agent_task: None,
            secret_env: None,
            lifecycle_policy: RunnerExecutionLifecyclePolicy::default(),
            artifact_declarations: Vec::new(),
            loop_policy: RunnerExecutionLoopPolicy::default(),
            mutation_policy,
            publication_intent: RunnerExecutionPublicationIntent::default(),
            result_refs,
            metadata: Value::Null,
        }
    }

    pub fn from_agent_task_request(request: AgentTaskRequest) -> Self {
        let artifact_declarations = request
            .canonical_artifact_declarations()
            .into_iter()
            .map(RunnerExecutionArtifactDeclaration::from)
            .collect();
        let secret_env = SecretEnvPlan::from_secret_env_names(request.executor.secret_env.clone());
        let result_refs = RunnerExecutionResultRefs {
            task_id: Some(request.task_id.clone()),
            plan_id: request.parent_plan_id.clone(),
            ..RunnerExecutionResultRefs::default()
        };

        Self {
            schema: RUNNER_EXECUTION_ENVELOPE_SCHEMA.to_string(),
            envelope_id: request.task_id.clone(),
            source: RunnerExecutionSource {
                kind: "agent_task".to_string(),
                ref_id: Some(request.task_id.clone()),
            },
            runner_workload: None,
            agent_task: Some(request),
            secret_env: Some(secret_env),
            lifecycle_policy: RunnerExecutionLifecyclePolicy::default(),
            artifact_declarations,
            loop_policy: RunnerExecutionLoopPolicy::default(),
            mutation_policy: RunnerExecutionMutationPolicy::default(),
            publication_intent: RunnerExecutionPublicationIntent::default(),
            result_refs,
            metadata: Value::Null,
        }
    }
}

impl From<AgentTaskArtifactDeclaration> for RunnerExecutionArtifactDeclaration {
    fn from(declaration: AgentTaskArtifactDeclaration) -> Self {
        Self {
            name: declaration.name,
            artifact_type: declaration.artifact_type,
            artifact_schema: declaration.artifact_schema,
            path: declaration.path,
            required: declaration.required,
            description: declaration.description,
            metadata: declaration.metadata,
        }
    }
}

fn runner_execution_envelope_schema() -> String {
    RUNNER_EXECUTION_ENVELOPE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::command_contract::{
        RunnerWorkloadAssignment, RunnerWorkloadCommandFamily, RunnerWorkloadKind,
        RunnerWorkloadMutationPolicy, RunnerWorkloadResultRefs, RunnerWorkloadSecrets,
        RunnerWorkloadState, RunnerWorkloadWorkspaceMappings, RUNNER_WORKLOAD_SCHEMA,
    };
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskPolicy, AgentTaskWorkspace, AgentTaskWorkspaceMode,
        AGENT_TASK_REQUEST_SCHEMA,
    };

    #[test]
    fn runner_workload_compiles_into_versioned_execution_envelope() {
        let workload = RunnerWorkload {
            schema: RUNNER_WORKLOAD_SCHEMA.to_string(),
            workload_id: "plan-1.runner_workload".to_string(),
            kind: RunnerWorkloadKind {
                command_label: "test".to_string(),
                command_family: RunnerWorkloadCommandFamily::Quality,
            },
            workspace_mappings: RunnerWorkloadWorkspaceMappings {
                source_path_mode: "cwd_or_path_flag".to_string(),
                workspace_mode_policy: "git".to_string(),
                mapping_ref: Some("mapping-1".to_string()),
            },
            required_capabilities: Vec::new(),
            required_secrets: RunnerWorkloadSecrets {
                categories: Vec::new(),
            },
            required_extensions: Vec::new(),
            required_extension_revisions: Vec::new(),
            mutation_policy: RunnerWorkloadMutationPolicy {
                capture_patch: true,
                mutation_flag: Some("--apply".to_string()),
                allow_dirty_lab_workspace: false,
            },
            assignment: RunnerWorkloadAssignment {
                runner_id: Some("runner-a".to_string()),
                runner_mode: Some("ssh".to_string()),
                source: Some("default".to_string()),
            },
            state: RunnerWorkloadState {
                status: "assigned".to_string(),
                remote_workspace: Some("/workspace/project".to_string()),
                fallback_reason: None,
            },
            result_refs: RunnerWorkloadResultRefs {
                plan_id: "plan-1".to_string(),
                proof_id: Some("proof-1".to_string()),
                workspace_mapping_ref: Some("mapping-1".to_string()),
                job_id: Some("job-1".to_string()),
                mirror_run_id: None,
                artifacts: vec![RunnerWorkloadArtifactRef {
                    id: "artifact-1".to_string(),
                    name: Some("report".to_string()),
                    path: Some("artifacts/report.json".to_string()),
                    url: None,
                }],
            },
        };

        let envelope = RunnerExecutionEnvelope::from_runner_workload(workload.clone());
        let encoded = serde_json::to_value(&envelope).expect("serialize envelope");
        let decoded: RunnerExecutionEnvelope =
            serde_json::from_value(encoded).expect("decode envelope");

        assert_eq!(decoded.schema, RUNNER_EXECUTION_ENVELOPE_SCHEMA);
        assert_eq!(decoded.runner_workload, Some(workload));
        assert_eq!(decoded.mutation_policy.capture_patch, true);
        assert_eq!(decoded.result_refs.plan_id.as_deref(), Some("plan-1"));
        assert_eq!(decoded.result_refs.job_id.as_deref(), Some("job-1"));
        assert_eq!(decoded.result_refs.artifacts.len(), 1);
    }

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

        let envelope = RunnerExecutionEnvelope::from_agent_task_request(request);

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
            "artifactDeclarations": [
                {
                    "name": "report",
                    "type": "json",
                    "artifactSchema": "example/report/v1",
                    "path": "artifacts/report.json",
                    "required": true
                }
            ]
        }))
        .expect("decode extensions-shaped fixture");

        let selection = request.executor.runtime_selection();
        let envelope = RunnerExecutionEnvelope::from_agent_task_request(request);
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
        assert_eq!(
            decoded
                .agent_task
                .expect("agent task")
                .executor
                .runtime_id(),
            Some("runtime-1")
        );
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
