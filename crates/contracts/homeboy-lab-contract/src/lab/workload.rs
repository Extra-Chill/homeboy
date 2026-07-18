//! Runner workload schema carried by Lab offload requests.

use super::labels::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL,
    REFACTOR_LAB_LABEL, REVIEW_LAB_LABEL, RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL,
    RUNTIME_REFRESH_LAB_LABEL, TEST_LAB_LABEL, TRACE_LAB_LABEL,
};

pub const LAB_RUNNER_WORKLOAD_SCHEMA: &str = "homeboy/runner-workload/v1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkload {
    pub schema: String,
    pub workload_id: String,
    pub kind: LabRunnerWorkloadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task: Option<LabRunnerWorkloadAgentTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_route: Option<crate::notification_route::NotificationRoute>,
    pub workspace_mappings: LabRunnerWorkloadWorkspaceMappings,
    pub required_capabilities: Vec<LabRunnerWorkloadCapability>,
    pub required_secrets: LabRunnerWorkloadSecrets,
    pub required_extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_extension_revisions: Vec<LabRunnerWorkloadExtensionRevision>,
    pub mutation_policy: LabRunnerWorkloadMutationPolicy,
    pub assignment: LabRunnerWorkloadAssignment,
    pub state: LabRunnerWorkloadState,
    pub result_refs: LabRunnerWorkloadResultRefs,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadExtensionRevision {
    pub extension_id: String,
    pub source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadKind {
    pub command_label: String,
    pub command_family: LabRunnerWorkloadCommandFamily,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadAgentTask {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_provider_policy: Option<crate::agent_task_config::ResolvedAgentTaskProviderPolicy>,
    pub dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind,
    pub lifecycle_mirror_policy: LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabRunnerWorkloadAgentTaskDispatchKind {
    Cook,
    Dispatch,
    RunPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy {
    None,
    RunPlanAggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabRunnerWorkloadCommandFamily {
    AgentTask,
    Quality,
    Workspace,
    Service,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadWorkspaceMappings {
    pub source_path_mode: String,
    pub workspace_mode_policy: String,
    pub mapping_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadCapability {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadSecrets {
    pub categories: Vec<String>,
    #[serde(default)]
    pub secret_env_plan: crate::secret_env_plan::SecretEnvPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadMutationPolicy {
    pub capture_patch: bool,
    pub mutation_flag: Option<String>,
    pub allow_dirty_lab_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadAssignment {
    pub runner_id: Option<String>,
    pub runner_mode: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadState {
    pub status: String,
    pub remote_workspace: Option<String>,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadResultRefs {
    pub plan_id: String,
    pub proof_id: Option<String>,
    pub workspace_mapping_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<LabRunnerWorkloadArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerWorkloadArtifactRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl LabRunnerWorkloadCommandFamily {
    pub fn from_command_label(label: &str) -> Self {
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
