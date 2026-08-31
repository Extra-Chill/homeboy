//! Runner workload schema carried by Lab offload requests.

use super::labels::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL,
    REFACTOR_LAB_LABEL, REVIEW_LAB_LABEL, RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL,
    RUNTIME_REFRESH_LAB_LABEL, TEST_LAB_LABEL, TRACE_LAB_LABEL,
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
    /// How the controller should recover a cook/dispatch handoff document.
    ///
    /// `lifecycle_mirror_policy` only covers the `run-plan` aggregate replay.
    /// The cook/dispatch handoff took a different route entirely: the
    /// controller scraped stdout and stderr for a
    /// `homeboy/agent-task-lab-handoff/v1` (or legacy
    /// `homeboy/agent-task-dispatch/v1`) document, so any change to what the
    /// remote command printed silently broke failure-evidence mirroring even
    /// though the workload itself carried the run identity all along.
    ///
    /// This is `Option` rather than a new `lifecycle_mirror_policy` variant on
    /// purpose. `lifecycle_mirror_policy` is a required field, so a peer that
    /// predates a new variant would fail to deserialize the *entire* workload.
    /// A new optional field is ignored by such a peer instead, and `None` here
    /// unambiguously means "the emitting side predates the typed handoff
    /// event", which is exactly when the stdout fallback must still run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_mirror_policy: Option<LabRunnerWorkloadAgentTaskHandoffMirrorPolicy>,
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
            "review audit" | "review lint" | "review test" | "review build" | "review ci" => {
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

    /// `agent_task` is an optional section. A workload written by a peer that
    /// never populates it (any non-agent-task command, and every binary that
    /// predates the section) must still deserialize.
    #[test]
    fn workload_without_an_agent_task_section_still_parses() {
        let workload: LabRunnerWorkload =
            serde_json::from_value(workload_payload(None)).expect("workload without agent_task");

        assert!(workload.agent_task.is_none());

        // ... and the absent section stays absent on the wire.
        let reserialized = serde_json::to_value(&workload).expect("workload serializes");
        assert!(reserialized.get("agent_task").is_none());
    }

    /// `handoff_mirror_policy` was added after `agent_task` shipped. A workload
    /// emitted by a peer that predates the field must deserialize with the
    /// field absent — and re-serialize byte-identically, so a controller that
    /// merely relays a workload does not fabricate a policy the emitter never
    /// declared. `None` is what selects the retained stdout fallback.
    #[test]
    fn agent_task_section_without_a_handoff_mirror_policy_round_trips_unchanged() {
        let agent_task = json!({
            "run_id": "cook-7530",
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none"
        });
        let workload: LabRunnerWorkload =
            serde_json::from_value(workload_payload(Some(agent_task.clone())))
                .expect("workload with a pre-typed agent_task section");

        let parsed = workload.agent_task.as_ref().expect("agent_task section");
        assert_eq!(parsed.run_id, "cook-7530");
        assert_eq!(parsed.plan_ref, None);
        assert_eq!(
            parsed.dispatch_kind,
            LabRunnerWorkloadAgentTaskDispatchKind::Cook
        );
        assert_eq!(
            parsed.lifecycle_mirror_policy,
            LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::None
        );
        assert_eq!(parsed.handoff_mirror_policy, None);

        let reserialized = serde_json::to_value(parsed).expect("agent_task serializes");
        assert_eq!(reserialized, agent_task);
    }

    /// The added field is additive: populated, it serializes alongside the
    /// original four, and a consumer that ignores it still sees the pre-typed
    /// shape.
    #[test]
    fn handoff_mirror_policy_is_additive_and_snake_case() {
        let agent_task = LabRunnerWorkloadAgentTask {
            run_id: "cook-7530".to_string(),
            plan_ref: None,
            resolved_provider_policy: None,
            dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind::Dispatch,
            lifecycle_mirror_policy: LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
            handoff_mirror_policy: Some(
                LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::DispatchHandoff,
            ),
        };

        let value = serde_json::to_value(&agent_task).expect("agent_task serializes");
        assert_eq!(
            value,
            json!({
                "run_id": "cook-7530",
                "dispatch_kind": "dispatch",
                "lifecycle_mirror_policy": "none",
                "handoff_mirror_policy": "dispatch_handoff"
            })
        );

        let round_tripped: LabRunnerWorkloadAgentTask =
            serde_json::from_value(value).expect("agent_task deserializes");
        assert_eq!(round_tripped, agent_task);

        let run_plan = json!({
            "run_id": "run-plan-7530",
            "plan_ref": "@plan.json",
            "dispatch_kind": "run_plan",
            "lifecycle_mirror_policy": "run_plan_aggregate",
            "handoff_mirror_policy": "none"
        });
        let parsed: LabRunnerWorkloadAgentTask =
            serde_json::from_value(run_plan).expect("run-plan agent_task deserializes");
        assert_eq!(
            parsed.handoff_mirror_policy,
            Some(LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::None)
        );
    }

    /// Adding a *spelling* to `handoff_mirror_policy` is not free the way
    /// adding the field was. An old peer rejects the unknown spelling, and
    /// because `agent_task` is deserialized as a whole that rejection fails the
    /// entire workload parse — not just the section.
    ///
    /// This test pins that so the blast radius is visible at review time: a new
    /// policy spelling is a breaking change for old peers and must be rolled
    /// out behind another new optional field instead, exactly as this field was
    /// rolled out rather than as a `lifecycle_mirror_policy` variant.
    #[test]
    fn an_unknown_handoff_policy_spelling_is_a_breaking_change_for_old_peers() {
        let payload = workload_payload(Some(json!({
            "run_id": "cook-7530",
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none",
            "handoff_mirror_policy": "a_policy_from_the_future"
        })));

        let section: std::result::Result<LabRunnerWorkloadAgentTask, _> =
            serde_json::from_value(payload["agent_task"].clone());
        assert!(section.is_err(), "unknown spelling is rejected");

        let workload: std::result::Result<LabRunnerWorkload, _> = serde_json::from_value(payload);
        assert!(
            workload.is_err(),
            "and that rejection propagates to the whole workload"
        );
    }
}
