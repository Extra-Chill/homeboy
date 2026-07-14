//! Runner workload schema carried by Lab offload requests.

use crate::command_contract::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL,
    REFACTOR_LAB_LABEL, REVIEW_LAB_LABEL, RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL,
    RUNTIME_REFRESH_LAB_LABEL, TEST_LAB_LABEL, TRACE_LAB_LABEL,
};

pub const RUNNER_WORKLOAD_SCHEMA: &str = "homeboy/runner-workload/v1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkload {
    pub schema: String,
    pub workload_id: String,
    pub kind: RunnerWorkloadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task: Option<RunnerWorkloadAgentTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_route: Option<crate::core::notification_route::NotificationRoute>,
    pub workspace_mappings: RunnerWorkloadWorkspaceMappings,
    pub required_capabilities: Vec<RunnerWorkloadCapability>,
    pub required_secrets: RunnerWorkloadSecrets,
    pub required_extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_extension_revisions: Vec<RunnerWorkloadExtensionRevision>,
    pub mutation_policy: RunnerWorkloadMutationPolicy,
    pub assignment: RunnerWorkloadAssignment,
    pub state: RunnerWorkloadState,
    pub result_refs: RunnerWorkloadResultRefs,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadExtensionRevision {
    pub extension_id: String,
    pub source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadKind {
    pub command_label: String,
    pub command_family: RunnerWorkloadCommandFamily,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadAgentTask {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_provider_policy:
        Option<crate::core::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy>,
    pub dispatch_kind: RunnerWorkloadAgentTaskDispatchKind,
    pub lifecycle_mirror_policy: RunnerWorkloadAgentTaskLifecycleMirrorPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkloadAgentTaskDispatchKind {
    Cook,
    Dispatch,
    RunPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkloadAgentTaskLifecycleMirrorPolicy {
    None,
    RunPlanAggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkloadCommandFamily {
    AgentTask,
    Quality,
    Workspace,
    Service,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadWorkspaceMappings {
    pub source_path_mode: String,
    pub workspace_mode_policy: String,
    pub mapping_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadCapability {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadSecrets {
    pub categories: Vec<String>,
    #[serde(default)]
    pub secret_env_plan: crate::core::secret_env_plan::SecretEnvPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadMutationPolicy {
    pub capture_patch: bool,
    pub mutation_flag: Option<String>,
    pub allow_dirty_lab_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadAssignment {
    pub runner_id: Option<String>,
    pub runner_mode: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadState {
    pub status: String,
    pub remote_workspace: Option<String>,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadResultRefs {
    pub plan_id: String,
    pub proof_id: Option<String>,
    pub workspace_mapping_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<RunnerWorkloadArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerWorkloadArtifactRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl RunnerWorkloadCommandFamily {
    pub(crate) fn from_command_label(label: &str) -> Self {
        match label {
            label if label.starts_with("agent-task") => Self::AgentTask,
            label
                if matches!(
                    label,
                    "review audit" | "review lint" | "review test" | "review build" | "review ci"
                ) =>
            {
                Self::Quality
            }
            LINT_LAB_LABEL
            | TEST_LAB_LABEL
            | AUDIT_LAB_LABEL
            | REVIEW_LAB_LABEL
            | BENCH_LAB_LABEL
            | FUZZ_LAB_LABEL
            | FUZZ_DOCTOR_LAB_LABEL
            | TRACE_LAB_LABEL
            | RIG_RUN_LAB_LABEL => Self::Quality,
            REFACTOR_LAB_LABEL | RIG_CHECK_LAB_LABEL | RUNTIME_REFRESH_LAB_LABEL => Self::Workspace,
            label if label.starts_with("tunnel") => Self::Service,
            _ => Self::Unknown,
        }
    }
}
