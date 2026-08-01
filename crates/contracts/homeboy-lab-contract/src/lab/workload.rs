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
    pub artifacts: Vec<JobArtifactMetadata>,
}

/// Canonical artifact pointer shared by Lab workloads, runner execution
/// records, and the runner job model.
///
/// This is the single artifact-pointer shape. `JobArtifactMetadata`
/// used to be a separate `{id, name, path, url}` struct here that was a strict
/// field-subset of the api-jobs `JobArtifactMetadata`, so crossing between them
/// meant a field-by-field rebuild that silently dropped `mime`, `size_bytes`
/// and `sha256`. The type lives in this crate because `homeboy-api-jobs-contract`
/// already depends on `homeboy-lab-contract`; defining it the other way round
/// would be a dependency cycle.
///
/// Every added field is `Option` + `skip_serializing_if`, so a value carrying
/// only the old four fields serializes byte-identically to the pre-collapse
/// wire shape, and pre-collapse JSON still deserializes. Neither struct used
/// `deny_unknown_fields`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobArtifactMetadata {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// #11137 collapsed `LabRunnerWorkloadArtifactRef` onto
    /// `JobArtifactMetadata`. The former was a strict field-subset of the
    /// latter, so records written by an older binary must still deserialize and
    /// re-serialize byte-identically. This matters because the type is embedded
    /// in two schemas published to external cross-language consumers via the
    /// contract schema catalog: `homeboy/runner-workload/v1` and
    /// `homeboy/runner-exec-handoff/v1`.
    #[test]
    fn artifact_refs_keep_the_pre_collapse_wire_shape() {
        let payload = json!([
            {
                "id": "report",
                "name": "summary",
                "path": "artifacts/summary.json",
                "url": "https://example.test/summary.json"
            },
            { "id": "bare" }
        ]);

        let refs: Vec<JobArtifactMetadata> =
            serde_json::from_value(payload.clone()).expect("legacy refs deserialize");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "report");
        assert_eq!(refs[0].name.as_deref(), Some("summary"));
        assert_eq!(refs[0].path.as_deref(), Some("artifacts/summary.json"));
        assert_eq!(refs[1].id, "bare");
        assert_eq!(refs[1].name, None);

        // Fields gained in the collapse stay absent from the wire form, so the
        // round trip is byte-identical to the pre-collapse shape.
        assert_eq!(refs[0].mime, None);
        assert_eq!(refs[0].size_bytes, None);
        assert_eq!(refs[0].sha256, None);

        let reserialized = serde_json::to_value(&refs).expect("refs serialize");
        assert_eq!(reserialized, payload);
    }

    /// The added fields are additive: when they are populated they serialize
    /// alongside the original four, and a consumer that ignores them still sees
    /// the pre-collapse shape.
    #[test]
    fn artifact_ref_added_fields_are_additive() {
        let artifact = JobArtifactMetadata {
            id: "report".to_string(),
            path: Some("artifacts/summary.json".to_string()),
            sha256: Some("abc123".to_string()),
            size_bytes: Some(42),
            ..Default::default()
        };

        let value = serde_json::to_value(&artifact).expect("artifact serializes");
        assert_eq!(
            value,
            json!({
                "id": "report",
                "path": "artifacts/summary.json",
                "sha256": "abc123",
                "size_bytes": 42
            })
        );

        let round_tripped: JobArtifactMetadata =
            serde_json::from_value(value).expect("artifact deserializes");
        assert_eq!(round_tripped, artifact);
    }
}
