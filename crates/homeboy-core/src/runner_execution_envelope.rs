use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::agent_task::{AgentTaskArtifactDeclaration, AgentTaskRequest};
use crate::env_materialization_plan::EnvMaterializationPlan;
use crate::lab_contract::{LabRunnerWorkload, LabRunnerWorkloadArtifactRef};
use crate::secret_env_plan::SecretEnvPlan;
use crate::source_snapshot::SourceSnapshot;

pub const RUNNER_EXECUTION_ENVELOPE_SCHEMA: &str = "homeboy/runner-execution-envelope/v1";
pub const RUNNER_EXECUTION_RECORD_SCHEMA: &str = "homeboy/runner-execution-record/v1";
pub const ORCHESTRATION_TARGET_PROVENANCE_SCHEMA: &str =
    "homeboy/orchestration-target-provenance/v1";

// Path materialization types live in the leaf `core::path_materialization`
// module so the lab-contract type layer can hold a `PathMaterializationPlan`
// field without pulling in this envelope's runner machinery. Re-exported here to
// keep existing `runner_execution_envelope::PathMaterialization*` call sites stable.
pub use crate::path_materialization::{
    PathMaterializationEntry, PathMaterializationMode, PathMaterializationPathRemap,
    PathMaterializationPlan, PathMaterializationPlanProjection, PathMaterializationProjection,
    PATH_MATERIALIZATION_MODE_EXISTING_REMOTE, PATH_MATERIALIZATION_MODE_GIT,
    PATH_MATERIALIZATION_MODE_SNAPSHOT, PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
    PATH_MATERIALIZATION_OWNER_LAB_PROVIDER_CONFIG,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT, PATH_MATERIALIZATION_PLAN_SCHEMA,
    PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE, PATH_MATERIALIZATION_ROLE_REQUIRED_PATH,
    PATH_MATERIALIZATION_STATUS_MATERIALIZED, PATH_MATERIALIZATION_STATUS_VALIDATED,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunnerExecutionEnvelope {
    #[serde(default = "runner_execution_envelope_schema")]
    pub schema: String,
    pub envelope_id: String,
    #[serde(default)]
    pub source: RunnerExecutionSource,
    #[serde(
        rename = "runner_workload",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub lab_runner_workload: Option<LabRunnerWorkload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task: Option<AgentTaskRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_env: Option<SecretEnvPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_materialization: Option<EnvMaterializationPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<RunnerExecutionDispatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RunnerExecutionLifecycle>,
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
pub struct RunnerExecutionDispatch {
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub operation: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionLifecycle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_cell_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionRecord {
    #[serde(default = "runner_execution_record_schema")]
    pub schema: String,
    pub execution_id: String,
    pub runner_id: String,
    pub transport: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_materialization_plan: Option<PathMaterializationPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_provenance: Option<OrchestrationTargetProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<RunnerExecutionArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<RunnerExecutionNextAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionProjection {
    pub schema: String,
    pub execution_id: String,
    pub runner_id: String,
    pub transport: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materialized_paths: Vec<PathMaterializationProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<RunnerExecutionArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<RunnerExecutionNextAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationTargetProvenance {
    #[serde(default = "orchestration_target_provenance_schema")]
    pub schema: String,
    pub selected_runner_id: String,
    pub controller_binary: BinaryProvenance,
    pub runner_daemon_binary: BinaryProvenance,
    pub runner_command_binary: BinaryProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot_identity: Option<SourceSnapshotIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinaryProvenance {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshotIdentity {
    pub snapshot_hash: String,
    pub sync_mode: String,
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionProvenance {
    pub extension_id: String,
    pub path: String,
    pub install_mode: String,
    pub manifest_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

impl OrchestrationTargetProvenance {
    pub fn new(
        selected_runner_id: impl Into<String>,
        controller_binary: BinaryProvenance,
        runner_daemon_binary: BinaryProvenance,
        runner_command_binary: BinaryProvenance,
    ) -> Self {
        Self {
            schema: ORCHESTRATION_TARGET_PROVENANCE_SCHEMA.to_string(),
            selected_runner_id: selected_runner_id.into(),
            controller_binary,
            runner_daemon_binary,
            runner_command_binary,
            source_snapshot_identity: None,
            extensions: Vec::new(),
        }
    }

    pub fn with_source_snapshot_identity(
        mut self,
        identity: Option<SourceSnapshotIdentity>,
    ) -> Self {
        self.source_snapshot_identity = identity;
        self
    }

    pub fn with_extensions(mut self, extensions: Vec<ExtensionProvenance>) -> Self {
        self.extensions = extensions;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionArtifactRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExecutionNextAction {
    pub label: String,
    pub command: Vec<String>,
}

impl RunnerExecutionRecord {
    pub fn planned(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
    ) -> Self {
        Self {
            schema: RUNNER_EXECUTION_RECORD_SCHEMA.to_string(),
            execution_id: execution_id.into(),
            runner_id: runner_id.into(),
            transport: transport.into(),
            status: "planned".to_string(),
            job_id: None,
            local_run_id: None,
            remote_run_id: None,
            agent_task_run_id: None,
            mirror_run_id: None,
            path_materialization_plan: None,
            orchestration_provenance: None,
            artifact_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }

    pub fn terminal(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
        exit_code: i32,
    ) -> Self {
        Self {
            schema: RUNNER_EXECUTION_RECORD_SCHEMA.to_string(),
            execution_id: execution_id.into(),
            runner_id: runner_id.into(),
            transport: transport.into(),
            status: if exit_code == 0 {
                "succeeded"
            } else {
                "failed"
            }
            .to_string(),
            job_id: None,
            local_run_id: None,
            remote_run_id: None,
            agent_task_run_id: None,
            mirror_run_id: None,
            path_materialization_plan: None,
            orchestration_provenance: None,
            artifact_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }

    pub fn in_flight(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
    ) -> Self {
        Self {
            schema: RUNNER_EXECUTION_RECORD_SCHEMA.to_string(),
            execution_id: execution_id.into(),
            runner_id: runner_id.into(),
            transport: transport.into(),
            status: "running".to_string(),
            job_id: None,
            local_run_id: None,
            remote_run_id: None,
            agent_task_run_id: None,
            mirror_run_id: None,
            path_materialization_plan: None,
            orchestration_provenance: None,
            artifact_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }

    pub fn with_job_id(mut self, job_id: impl Into<String>) -> Self {
        self.job_id = Some(job_id.into());
        self
    }

    pub fn with_mirror_run_id(mut self, mirror_run_id: Option<String>) -> Self {
        self.mirror_run_id = mirror_run_id.clone();
        self.remote_run_id = mirror_run_id;
        self
    }

    pub fn with_agent_task_run_id(mut self, agent_task_run_id: impl Into<String>) -> Self {
        self.agent_task_run_id = Some(agent_task_run_id.into());
        self
    }

    pub fn with_artifact_refs(
        mut self,
        artifact_refs: impl IntoIterator<Item = RunnerExecutionArtifactRef>,
    ) -> Self {
        self.artifact_refs = artifact_refs.into_iter().collect();
        self
    }

    pub fn with_path_materialization_plan(
        mut self,
        path_materialization_plan: Option<PathMaterializationPlan>,
    ) -> Self {
        self.path_materialization_plan = path_materialization_plan;
        self
    }

    pub fn with_orchestration_provenance(
        mut self,
        provenance: Option<OrchestrationTargetProvenance>,
    ) -> Self {
        self.orchestration_provenance = provenance;
        self
    }

    pub fn with_next_actions(
        mut self,
        next_actions: impl IntoIterator<Item = RunnerExecutionNextAction>,
    ) -> Self {
        self.next_actions = next_actions.into_iter().collect();
        self
    }

    pub fn projection(&self) -> RunnerExecutionProjection {
        RunnerExecutionProjection {
            schema: self.schema.clone(),
            execution_id: self.execution_id.clone(),
            runner_id: self.runner_id.clone(),
            transport: self.transport.clone(),
            status: self.status.clone(),
            job_id: self.job_id.clone(),
            local_run_id: self.local_run_id.clone(),
            remote_run_id: self.remote_run_id.clone(),
            agent_task_run_id: self.agent_task_run_id.clone(),
            mirror_run_id: self.mirror_run_id.clone(),
            materialized_paths: self
                .path_materialization_plan
                .as_ref()
                .map(PathMaterializationPlan::projection_entries)
                .unwrap_or_default(),
            artifact_refs: self.artifact_refs.clone(),
            next_actions: self.next_actions.clone(),
        }
    }
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
    pub artifacts: Vec<LabRunnerWorkloadArtifactRef>,
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
            lab_runner_workload: None,
            agent_task: None,
            secret_env: None,
            env_materialization: None,
            dispatch: None,
            lifecycle: None,
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

    pub fn with_dispatch(mut self, dispatch: RunnerExecutionDispatch) -> Self {
        self.dispatch = Some(dispatch);
        self
    }

    pub fn with_lifecycle(mut self, lifecycle: Option<RunnerExecutionLifecycle>) -> Self {
        self.lifecycle = lifecycle;
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

    pub fn from_lab_runner_workload(workload: LabRunnerWorkload) -> Self {
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
            lab_runner_workload: Some(workload),
            agent_task: None,
            secret_env: None,
            env_materialization: None,
            dispatch: None,
            lifecycle: None,
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
            lab_runner_workload: None,
            agent_task: Some(request),
            secret_env: Some(secret_env),
            env_materialization: None,
            dispatch: None,
            lifecycle: None,
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

fn runner_execution_record_schema() -> String {
    RUNNER_EXECUTION_RECORD_SCHEMA.to_string()
}

fn orchestration_target_provenance_schema() -> String {
    ORCHESTRATION_TARGET_PROVENANCE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskPolicy, AgentTaskWorkspace, AgentTaskWorkspaceMode,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::lab_contract::{
        LabRunnerWorkloadAssignment, LabRunnerWorkloadCommandFamily, LabRunnerWorkloadKind,
        LabRunnerWorkloadMutationPolicy, LabRunnerWorkloadResultRefs, LabRunnerWorkloadSecrets,
        LabRunnerWorkloadState, LabRunnerWorkloadWorkspaceMappings, LAB_RUNNER_WORKLOAD_SCHEMA,
    };

    #[test]
    fn lab_runner_workload_compiles_into_versioned_execution_envelope() {
        let workload = LabRunnerWorkload {
            schema: LAB_RUNNER_WORKLOAD_SCHEMA.to_string(),
            workload_id: "plan-1.runner_workload".to_string(),
            kind: LabRunnerWorkloadKind {
                command_label: "test".to_string(),
                command_family: LabRunnerWorkloadCommandFamily::Quality,
            },
            agent_task: None,
            notification_route: None,
            workspace_mappings: LabRunnerWorkloadWorkspaceMappings {
                source_path_mode: "cwd_or_path_flag".to_string(),
                workspace_mode_policy: "git".to_string(),
                mapping_ref: Some("mapping-1".to_string()),
            },
            required_capabilities: Vec::new(),
            required_secrets: LabRunnerWorkloadSecrets {
                categories: Vec::new(),
                secret_env_plan: SecretEnvPlan::default(),
            },
            required_extensions: Vec::new(),
            required_extension_revisions: Vec::new(),
            mutation_policy: LabRunnerWorkloadMutationPolicy {
                capture_patch: true,
                mutation_flag: Some("--apply".to_string()),
                allow_dirty_lab_workspace: false,
            },
            assignment: LabRunnerWorkloadAssignment {
                runner_id: Some("runner-a".to_string()),
                runner_mode: Some("ssh".to_string()),
                source: Some("default".to_string()),
            },
            state: LabRunnerWorkloadState {
                status: "assigned".to_string(),
                remote_workspace: Some("/workspace/project".to_string()),
                fallback_reason: None,
            },
            result_refs: LabRunnerWorkloadResultRefs {
                plan_id: "plan-1".to_string(),
                proof_id: Some("proof-1".to_string()),
                workspace_mapping_ref: Some("mapping-1".to_string()),
                job_id: Some("job-1".to_string()),
                mirror_run_id: None,
                artifacts: vec![LabRunnerWorkloadArtifactRef {
                    id: "artifact-1".to_string(),
                    name: Some("report".to_string()),
                    path: Some("artifacts/report.json".to_string()),
                    url: None,
                }],
            },
        };

        let envelope = RunnerExecutionEnvelope::from_lab_runner_workload(workload.clone());
        let encoded = serde_json::to_value(&envelope).expect("serialize envelope");
        let decoded: RunnerExecutionEnvelope =
            serde_json::from_value(encoded).expect("decode envelope");

        assert_eq!(decoded.schema, RUNNER_EXECUTION_ENVELOPE_SCHEMA);
        assert_eq!(decoded.lab_runner_workload, Some(workload));
        assert_eq!(decoded.mutation_policy.capture_patch, true);
        assert_eq!(decoded.result_refs.plan_id.as_deref(), Some("plan-1"));
        assert_eq!(decoded.result_refs.job_id.as_deref(), Some("job-1"));
        assert_eq!(decoded.result_refs.artifacts.len(), 1);
    }

    #[test]
    fn runner_execution_record_captures_durable_identity_and_actions() {
        let record = RunnerExecutionRecord::terminal("job-1", "lab-a", "daemon", 0)
            .with_job_id("job-1")
            .with_mirror_run_id(Some("run-1".to_string()))
            .with_artifact_refs(vec![RunnerExecutionArtifactRef {
                id: "artifact-1".to_string(),
                name: Some("report".to_string()),
                path: Some("artifacts/report.json".to_string()),
                url: None,
            }])
            .with_path_materialization_plan(Some(PathMaterializationPlan {
                schema: PATH_MATERIALIZATION_PLAN_SCHEMA.to_string(),
                entries: vec![PathMaterializationEntry::primary_workspace_materialized(
                    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT,
                    Some("/local/project".to_string()),
                    "/runner/project",
                    PathMaterializationMode::Snapshot.to_string(),
                )],
            }))
            .with_next_actions(vec![RunnerExecutionNextAction {
                label: "runner_job_logs".to_string(),
                command: vec![
                    "homeboy".to_string(),
                    "runner".to_string(),
                    "job".to_string(),
                    "logs".to_string(),
                    "lab-a".to_string(),
                    "job-1".to_string(),
                ],
            }]);

        let value = serde_json::to_value(&record).expect("serialize record");
        assert_eq!(value["schema"], RUNNER_EXECUTION_RECORD_SCHEMA);
        assert_eq!(value["execution_id"], "job-1");
        assert_eq!(value["runner_id"], "lab-a");
        assert_eq!(value["transport"], "daemon");
        assert_eq!(value["status"], "succeeded");
        assert_eq!(value["job_id"], "job-1");
        assert_eq!(value["remote_run_id"], "run-1");
        assert_eq!(
            value["path_materialization_plan"]["schema"],
            PATH_MATERIALIZATION_PLAN_SCHEMA
        );
        assert_eq!(
            value["path_materialization_plan"]["entries"][0]["remote_path"],
            "/runner/project"
        );
        assert_eq!(value["artifact_refs"][0]["id"], "artifact-1");
        assert_eq!(value["next_actions"][0]["label"], "runner_job_logs");
    }

    #[test]
    fn runner_execution_record_planned_captures_non_terminal_execution() {
        let record = RunnerExecutionRecord::planned("plan-1", "lab-a", "refresh_plan");

        assert_eq!(record.schema, RUNNER_EXECUTION_RECORD_SCHEMA);
        assert_eq!(record.execution_id, "plan-1");
        assert_eq!(record.runner_id, "lab-a");
        assert_eq!(record.transport, "refresh_plan");
        assert_eq!(record.status, "planned");
        assert!(record.job_id.is_none());
        assert!(record.artifact_refs.is_empty());
    }

    #[test]
    fn path_materialization_entry_parses_known_modes_without_rejecting_unknowns() {
        let entry = PathMaterializationEntry::primary_workspace_materialized(
            PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT,
            None,
            "/runner/project",
            PathMaterializationMode::Snapshot.to_string(),
        );
        let provider_owned = PathMaterializationEntry {
            materialization_mode: "provider_owned_mode".to_string(),
            ..entry.clone()
        };

        assert_eq!(entry.mode(), Some(PathMaterializationMode::Snapshot));
        assert_eq!(provider_owned.mode(), None);
    }

    #[test]
    fn path_materialization_plan_helpers_emit_canonical_shape() {
        let plan = PathMaterializationPlan::non_empty(vec![
            PathMaterializationEntry::required_existing_remote("/runner/cache"),
        ])
        .expect("non-empty plan");

        assert_eq!(plan.schema, PATH_MATERIALIZATION_PLAN_SCHEMA);
        assert_eq!(
            plan.entries[0].role,
            PATH_MATERIALIZATION_ROLE_REQUIRED_PATH
        );
        assert_eq!(
            plan.entries[0].owner,
            PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS
        );
        assert_eq!(plan.entries[0].remote_path, "/runner/cache");
        assert_eq!(
            plan.entries[0].mode(),
            Some(PathMaterializationMode::ExistingRemote)
        );
        assert_eq!(
            plan.entries[0].validation_status,
            PATH_MATERIALIZATION_STATUS_VALIDATED
        );
        assert!(PathMaterializationPlan::non_empty(Vec::new()).is_none());
    }

    #[test]
    fn path_materialization_plan_projection_exports_runtime_path_remaps() {
        let plan = PathMaterializationPlan::new(vec![
            PathMaterializationEntry::primary_workspace_materialized(
                PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT,
                Some(" /local/project ".to_string()),
                " /runner/project ",
                PathMaterializationMode::Snapshot.to_string(),
            ),
            PathMaterializationEntry::required_existing_remote("/runner/cache"),
            PathMaterializationEntry::primary_workspace_materialized(
                PATH_MATERIALIZATION_OWNER_LAB_PROVIDER_CONFIG,
                Some("".to_string()),
                "/runner/empty-local",
                PathMaterializationMode::Snapshot.to_string(),
            ),
        ]);

        let projection = plan.projection();

        assert_eq!(projection.schema, PATH_MATERIALIZATION_PLAN_SCHEMA);
        assert_eq!(projection.entries.len(), 3);
        assert_eq!(projection.path_remaps.len(), 1);
        assert_eq!(projection.path_remaps[0].local_path, "/local/project");
        assert_eq!(projection.path_remaps[0].remote_path, "/runner/project");
    }

    #[test]
    fn runner_execution_projection_flattens_materialized_paths() {
        let record = RunnerExecutionRecord::terminal("job-1", "lab-a", "daemon", 0)
            .with_job_id("job-1")
            .with_mirror_run_id(Some("run-1".to_string()))
            .with_path_materialization_plan(Some(PathMaterializationPlan::new(vec![
                PathMaterializationEntry::primary_workspace_materialized(
                    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT,
                    Some("/local/project".to_string()),
                    "/runner/project",
                    PathMaterializationMode::Snapshot.to_string(),
                ),
                PathMaterializationEntry::required_existing_remote("/runner/cache"),
            ])));

        let projection = record.projection();

        assert_eq!(projection.execution_id, "job-1");
        assert_eq!(projection.runner_id, "lab-a");
        assert_eq!(projection.job_id.as_deref(), Some("job-1"));
        assert_eq!(projection.remote_run_id.as_deref(), Some("run-1"));
        assert_eq!(projection.materialized_paths.len(), 2);
        assert_eq!(
            projection.materialized_paths[0].remote_path,
            "/runner/project"
        );
        assert_eq!(
            projection.materialized_paths[1].role,
            PATH_MATERIALIZATION_ROLE_REQUIRED_PATH
        );
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
