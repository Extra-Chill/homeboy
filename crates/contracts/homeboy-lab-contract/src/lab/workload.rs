//! Runner workload schema carried by Lab offload requests.

use super::labels::{
    BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL, REVIEW_LAB_LABEL,
    RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL, RUNTIME_REFRESH_LAB_LABEL, TEST_LAB_LABEL,
    TRACE_LAB_LABEL, WORKLOAD_LAB_LABEL,
};

pub const LAB_RUNNER_WORKLOAD_SCHEMA: &str = "homeboy/runner-workload/v1";

pub use homeboy_runner_contract::JobArtifactMetadata;

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
    /// Whether the controller mirrors a typed cook/dispatch handoff event.
    /// An absent policy declares no handoff mirroring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_mirror_policy: Option<LabRunnerWorkloadAgentTaskHandoffMirrorPolicy>,
    /// The workload's own declared wall-clock budget, in milliseconds: its
    /// effective provider max timeout plus its gate budget. The controller's
    /// handoff wait derives its deadline from this instead of the generic
    /// runner-exec default; `None` keeps that default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_budget_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabRunnerWorkloadAgentTaskHandoffMirrorPolicy {
    /// This workload has no dispatch handoff to mirror (`run-plan`).
    None,
    /// The runner emits a typed dispatch-handoff event keyed by
    /// [`LabRunnerWorkloadAgentTask::run_id`]; the controller mirrors from it.
    DispatchHandoff,
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

impl LabRunnerWorkloadCommandFamily {
    pub fn from_command_label(label: &str) -> Self {
        match label {
            label if label.starts_with("agent-task") => Self::AgentTask,
            "review lint" | "review test" | "review build" | "review ci" => Self::Quality,
            LINT_LAB_LABEL
            | TEST_LAB_LABEL
            | REVIEW_LAB_LABEL
            | BENCH_LAB_LABEL
            | FUZZ_LAB_LABEL
            | FUZZ_DOCTOR_LAB_LABEL
            | TRACE_LAB_LABEL
            | WORKLOAD_LAB_LABEL
            | RIG_RUN_LAB_LABEL => Self::Quality,
            RIG_CHECK_LAB_LABEL | RUNTIME_REFRESH_LAB_LABEL => Self::Workspace,
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
        assert_eq!(refs[0].path.as_deref(), Some("artifacts/summary.json"));
        assert_eq!(refs[1].name, None);

        // Fields gained in the collapse stay absent from the wire form, so the
        // round trip is byte-identical to the pre-collapse shape.
        let reserialized = serde_json::to_value(&refs).expect("refs serialize");
        assert_eq!(reserialized, payload);
    }

    fn workload_payload(agent_task: Option<serde_json::Value>) -> serde_json::Value {
        let mut payload = json!({
            "schema": LAB_RUNNER_WORKLOAD_SCHEMA,
            "workload_id": "lab-workload",
            "kind": { "command_label": "agent-task cook", "command_family": "agent_task" },
            "workspace_mappings": {
                "source_path_mode": "snapshot",
                "workspace_mode_policy": "snapshot",
                "mapping_ref": null
            },
            "required_capabilities": [],
            "required_secrets": { "categories": [] },
            "required_extensions": [],
            "mutation_policy": {
                "capture_patch": true,
                "mutation_flag": null,
                "allow_dirty_lab_workspace": false
            },
            "assignment": { "runner_id": "homeboy-lab", "runner_mode": null, "source": null },
            "state": { "status": "submitted", "remote_workspace": null, "fallback_reason": null },
            "result_refs": { "plan_id": "lab-workload", "proof_id": null, "workspace_mapping_ref": null }
        });
        if let Some(agent_task) = agent_task {
            payload["agent_task"] = agent_task;
        }
        payload
    }

    /// Workloads written by older peers must still parse: `agent_task` is
    /// optional (non-agent-task commands and binaries that predate it), and
    /// `handoff_mirror_policy` was added later. A relaying controller must
    /// re-serialize them unchanged rather than fabricate a policy the emitter
    /// never declared; `None` selects the retained stdout fallback.
    #[test]
    fn workloads_from_older_peers_parse_and_relay_unchanged() {
        let workload: LabRunnerWorkload =
            serde_json::from_value(workload_payload(None)).expect("workload without agent_task");
        assert!(workload.agent_task.is_none());
        assert!(serde_json::to_value(&workload)
            .expect("workload serializes")
            .get("agent_task")
            .is_none());

        let agent_task = json!({
            "run_id": "cook-7530",
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none"
        });
        let workload: LabRunnerWorkload =
            serde_json::from_value(workload_payload(Some(agent_task.clone())))
                .expect("workload with a pre-typed agent_task section");
        let parsed = workload.agent_task.as_ref().expect("agent_task section");
        assert_eq!(parsed.handoff_mirror_policy, None);
        assert_eq!(
            serde_json::to_value(parsed).expect("agent_task serializes"),
            agent_task
        );
    }

    /// Adding a *spelling* to `handoff_mirror_policy` is a breaking change for
    /// old peers: the unknown spelling fails the entire workload parse, so new
    /// policies must roll out behind another optional field.
    #[test]
    fn an_unknown_handoff_policy_spelling_is_a_breaking_change_for_old_peers() {
        let payload = workload_payload(Some(json!({
            "run_id": "cook-7530",
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none",
            "handoff_mirror_policy": "a_policy_from_the_future"
        })));
        let workload: std::result::Result<LabRunnerWorkload, _> = serde_json::from_value(payload);
        assert!(workload.is_err());
    }
}
