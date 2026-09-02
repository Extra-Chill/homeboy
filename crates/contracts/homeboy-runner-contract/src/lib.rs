//! Transport-neutral runner execution request and record contracts.
//!
//! These behavior-free values cross controller and runner implementation
//! boundaries. Execution behavior and service ownership remain outside this
//! crate.

mod artifact;
mod capability;
mod discovery;
pub mod env_materialization_plan;
mod execution_context;
mod lifecycle;
pub mod path_materialization;
mod resource;
pub mod secret_env_plan;
mod session;
mod submission;
mod workspace;
mod workspace_authority;

pub use artifact::{RunnerArtifactRef, RunnerMutationArtifacts};
pub use capability::{
    RunnerCapabilityPreflight, RunnerRequiredTool, RunnerToolCapabilityRequirement,
    RunnerToolchainReadinessProbe,
};
pub use discovery::{
    RunnerApiCapabilitiesRequest, RunnerApiCapabilitiesResponse, RunnerApiCompatibility,
    RunnerApiCompatibilityFailure, RunnerApiCompatibilityFailureCode, RunnerApiCompatibilityStatus,
    RunnerApiHandshakeRequest, RunnerApiHandshakeResponse, RunnerApiInspectRequest,
    RunnerApiInspectResponse, RunnerApiListRequest, RunnerApiListResponse,
    RunnerApiOperationFailure, RunnerApiOperationFailureCode, RunnerApiReadinessRequest,
    RunnerApiReadinessResponse, RunnerApiVersion, RunnerCapabilities, RunnerDescriptor,
    RunnerInspection, RunnerKind, RunnerReadiness, RUNNER_API_CAPABILITIES_REQUEST_SCHEMA,
    RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA, RUNNER_API_HANDSHAKE_REQUEST_SCHEMA,
    RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA, RUNNER_API_INSPECT_REQUEST_SCHEMA,
    RUNNER_API_INSPECT_RESPONSE_SCHEMA, RUNNER_API_LIST_REQUEST_SCHEMA,
    RUNNER_API_LIST_RESPONSE_SCHEMA, RUNNER_API_READINESS_REQUEST_SCHEMA,
    RUNNER_API_READINESS_RESPONSE_SCHEMA, RUNNER_API_V1, RUNNER_CAPABILITIES_SCHEMA,
    RUNNER_DESCRIPTOR_SCHEMA, RUNNER_INSPECTION_SCHEMA, RUNNER_READINESS_SCHEMA,
};
pub use execution_context::{
    is_internal_control_env, RUNNER_HOSTED_EXEC_ENV, RUNNER_ID_ENV, RUNNER_PLACEMENT_RESOLVED_ENV,
};
pub use lifecycle::{RunnerJobLifecycleMetadata, RunnerLifecycleOwner};
pub use resource::{
    RunnerResourceGuardLimits, RunnerResourceGuardViolation, RunnerResourceMetrics,
};
pub use session::{
    RunnerProxyForward, RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerTunnelMode,
    RunnerTunnelProcessStartIdentity,
};
pub use submission::{
    RunnerApiSubmitOutcome, RunnerApiSubmitRequest, RunnerApiSubmitResponse,
    RUNNER_API_SUBMIT_REQUEST_SCHEMA, RUNNER_API_SUBMIT_RESPONSE_SCHEMA,
};
pub use workspace::{
    ByteFileCounts, RunnerWorkspaceCurrentSummary, RunnerWorkspaceLease, RunnerWorkspaceSyncMode,
};
pub use workspace_authority::{
    WorkspaceClaim, WorkspaceClaimBinding, WorkspaceClaimProtocol, WorkspaceIdentity,
    WorkspaceOwnerLease, WorkspaceOwnerLeaseProtocol, WORKSPACE_CLAIM_CAPABILITY,
    WORKSPACE_CLAIM_PROTOCOL_VERSION, WORKSPACE_CLAIM_SCHEMA, WORKSPACE_IDENTITY_SCHEMA,
    WORKSPACE_OWNER_LEASE_CAPABILITY, WORKSPACE_OWNER_LEASE_SCHEMA,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::env_materialization_plan::EnvMaterializationPlan;
use crate::secret_env_plan::SecretEnvPlan;
use homeboy_source_snapshot_contract::SourceSnapshot;

/// The one artifact reference carried by runner execution records,
/// projections, lab workloads, and the runner job model.
///
/// This module used to define its own `RunnerExecutionArtifactRef` with the
/// four fields `{id, name, path, url}`, while the same file already imported
/// `JobArtifactMetadata` for `RunnerExecutionResultRefs.artifacts`. Two names,
/// one shape, one file. Collapsed onto this canonical runner type in #10310.
///
/// #11137 then collapsed `LabRunnerWorkloadArtifactRef` -- the last remaining
/// `{id, name, path, url}` twin, and a strict field-subset of this type -- onto
/// it as well, which removed the lossy `job_artifact_refs` rebuild that silently
/// dropped `mime`, `size_bytes` and `sha256`. The serialized shape is unchanged
/// in both collapses: every extra field is `Option` + `skip_serializing_if`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub metadata: Option<Value>,
}

pub const RUNNER_EXECUTION_ENVELOPE_SCHEMA: &str = "homeboy/runner-execution-envelope/v1";
pub const RUNNER_EXECUTION_RECORD_SCHEMA: &str = "homeboy/runner-execution-record/v1";
pub const ORCHESTRATION_TARGET_PROVENANCE_SCHEMA: &str =
    "homeboy/orchestration-target-provenance/v1";

// Path materialization belongs to the canonical runner contract. Re-export it
// here to keep existing `runner_execution_envelope::PathMaterialization*` call
// sites stable.
pub use crate::path_materialization::{
    PathMaterializationEntry, PathMaterializationMode, PathMaterializationPathRemap,
    PathMaterializationPlan, PathMaterializationPlanProjection, PathMaterializationProjection,
    PATH_MATERIALIZATION_MODE_EXISTING_REMOTE, PATH_MATERIALIZATION_MODE_GIT,
    PATH_MATERIALIZATION_MODE_SNAPSHOT, PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS,
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
    pub runner_workload: Option<Value>,
    /// The originating agent-task request, carried opaquely as JSON so core does
    /// not depend on the agent-task subsystem. The agent-task layer owns
    /// deserialization back into its request type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task: Option<Value>,
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
    /// Ordered IDs of runner-installed extensions that contribute runtime env.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_env_providers: Vec<String>,
}

/// Lifecycle facts carried by a runner execution envelope.
///
/// This was a second declaration of
/// [`RunnerJobLifecycleMetadata`] -- the same five fields with byte-identical
/// serde attributes on each.
pub type RunnerExecutionLifecycle = RunnerJobLifecycleMetadata;

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
    /// Flattened runtime view of `path_materialization_plan`, populated only by
    /// [`RunnerExecutionRecord::projection`]. Always empty on a stored record,
    /// so `homeboy/runner-execution-record/v1` emits exactly the fields it
    /// emitted before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materialized_paths: Vec<PathMaterializationProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_materialization_plan: Option<PathMaterializationPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_provenance: Option<OrchestrationTargetProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<JobArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<RunnerExecutionNextAction>,
}

/// The inspection view of a runner execution record: the stored
/// `path_materialization_plan` swapped for its flattened runtime projection,
/// and `orchestration_provenance` dropped.
///
/// This used to be a second struct repeating ten of the record's fields
/// verbatim, rebuilt field-by-field by `projection()`. Every field the two
/// states disagree on is `skip_serializing_if`, so "record" and "projection"
/// are two *values* of one type, not two types -- the same collapse #10310 and
/// #11137 applied to the artifact-ref twins above.
pub type RunnerExecutionProjection = RunnerExecutionRecord;

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
    pub prepared_workspace_original_snapshot_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepared_workspace_update_lineage: Vec<String>,
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
pub struct RunnerExecutionNextAction {
    pub label: String,
    pub command: Vec<String>,
}

impl RunnerExecutionRecord {
    /// The one empty-record constructor. `planned`, `terminal` and `in_flight`
    /// differed only in the `status` string they wrote.
    fn at_status(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            schema: RUNNER_EXECUTION_RECORD_SCHEMA.to_string(),
            execution_id: execution_id.into(),
            runner_id: runner_id.into(),
            transport: transport.into(),
            status: status.into(),
            job_id: None,
            local_run_id: None,
            remote_run_id: None,
            agent_task_run_id: None,
            mirror_run_id: None,
            materialized_paths: Vec::new(),
            path_materialization_plan: None,
            orchestration_provenance: None,
            artifact_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }

    pub fn planned(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
    ) -> Self {
        Self::at_status(execution_id, runner_id, transport, "planned")
    }

    pub fn terminal(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
        exit_code: i32,
    ) -> Self {
        Self::at_status(
            execution_id,
            runner_id,
            transport,
            if exit_code == 0 {
                "succeeded"
            } else {
                "failed"
            },
        )
    }

    pub fn in_flight(
        execution_id: impl Into<String>,
        runner_id: impl Into<String>,
        transport: impl Into<String>,
    ) -> Self {
        Self::at_status(execution_id, runner_id, transport, "running")
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
        artifact_refs: impl IntoIterator<Item = JobArtifactMetadata>,
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

    /// Inspection view: the stored plan is replaced by its flattened runtime
    /// entries and the orchestration provenance is dropped, so both
    /// `skip_serializing_if` themselves out of the emitted shape.
    pub fn projection(&self) -> RunnerExecutionProjection {
        Self {
            materialized_paths: self
                .path_materialization_plan
                .as_ref()
                .map(PathMaterializationPlan::projection_entries)
                .unwrap_or_default(),
            path_materialization_plan: None,
            orchestration_provenance: None,
            ..self.clone()
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
    pub artifacts: Vec<JobArtifactMetadata>,
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
    #[test]
    fn opaque_runner_workload_round_trips_without_interpreting_extension_policy() {
        let workload = json!({
            "schema": "homeboy/runner-workload/v1",
            "workload_id": "plan-1.runner_workload",
            "lab_policy": { "required_extensions": ["example"] }
        });
        let mut envelope = RunnerExecutionEnvelope::planned("plan-1", "runner_workload");
        envelope.runner_workload = Some(workload.clone());

        let encoded = serde_json::to_value(&envelope).expect("serialize envelope");
        let decoded: RunnerExecutionEnvelope =
            serde_json::from_value(encoded).expect("decode envelope");

        assert_eq!(decoded.schema, RUNNER_EXECUTION_ENVELOPE_SCHEMA);
        assert_eq!(decoded.runner_workload, Some(workload));
    }

    #[test]
    fn runner_execution_record_captures_durable_identity_and_actions() {
        let record = RunnerExecutionRecord::terminal("job-1", "lab-a", "daemon", 0)
            .with_job_id("job-1")
            .with_mirror_run_id(Some("run-1".to_string()))
            .with_artifact_refs(vec![JobArtifactMetadata {
                id: "artifact-1".to_string(),
                name: Some("report".to_string()),
                path: Some("artifacts/report.json".to_string()),
                url: None,
                ..Default::default()
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
                "test.provider_config",
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

    /// #10310 collapsed `RunnerExecutionArtifactRef` onto
    /// `JobArtifactMetadata`. Both were `{id, name, path, url}` with
    /// identical serde attributes, so records written by an older binary must
    /// still deserialize and re-serialize byte-identically.
    #[test]
    fn runner_execution_record_artifact_refs_keep_the_pre_collapse_wire_shape() {
        let payload = json!({
            "schema": RUNNER_EXECUTION_RECORD_SCHEMA,
            "execution_id": "job-1",
            "runner_id": "lab-a",
            "transport": "daemon",
            "status": "succeeded",
            "artifact_refs": [
                {
                    "id": "report",
                    "name": "summary",
                    "path": "artifacts/summary.json",
                    "url": "https://example.test/summary.json"
                },
                { "id": "bare" }
            ]
        });

        let record: RunnerExecutionRecord =
            serde_json::from_value(payload.clone()).expect("legacy record deserializes");
        assert_eq!(record.artifact_refs.len(), 2);
        assert_eq!(record.artifact_refs[0].id, "report");
        assert_eq!(record.artifact_refs[0].name.as_deref(), Some("summary"));
        assert_eq!(
            record.artifact_refs[0].path.as_deref(),
            Some("artifacts/summary.json")
        );
        assert_eq!(record.artifact_refs[1].id, "bare");
        assert_eq!(record.artifact_refs[1].name, None);

        let reserialized = serde_json::to_value(&record).expect("record serializes");
        assert_eq!(reserialized["artifact_refs"], payload["artifact_refs"]);
    }

    /// The lab workload result refs and the runner execution record refs are
    /// now literally the same type, so a value crosses between them without a
    /// field-by-field rebuild.
    #[test]
    fn workload_result_refs_and_execution_record_refs_share_one_type() {
        let artifact = JobArtifactMetadata {
            id: "report".to_string(),
            name: None,
            path: Some("artifacts/summary.json".to_string()),
            url: None,
            ..Default::default()
        };
        let result_refs = RunnerExecutionResultRefs {
            artifacts: vec![artifact.clone()],
            ..Default::default()
        };
        let record = RunnerExecutionRecord::terminal("job-1", "lab-a", "daemon", 0)
            .with_artifact_refs(result_refs.artifacts.iter().cloned());

        assert_eq!(record.artifact_refs, vec![artifact]);
    }
}
