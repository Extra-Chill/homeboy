use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

use super::schema::request_schema;
use super::{
    AgentTaskArtifactDeclaration, AgentTaskCapabilityEvidence, AgentTaskCapabilityRequirements,
    AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskPolicy,
    AgentTaskRuntimeTool, AgentTaskSourceRef, AgentTaskWorkspace,
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

/// A named value the provider returns in `AgentTaskOutcome.outputs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutputDeclaration {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    /// Versioned identifier for the output's semantic schema.
    pub schema: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub structural_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_relationship: Option<AgentTaskOutputEvidenceRelationship>,
}

impl AgentTaskOutputDeclaration {
    /// Why a present value fails this output's semantic contract, beyond its
    /// structural schema. A provider feeds this back to the session that
    /// authored the value so it can correct it in place.
    pub(crate) fn semantic_error(&self, value: &Value) -> Option<String> {
        if self.schema != crate::agent_task_review_dossier::AI_REVIEW_FORM_OUTPUT_SCHEMA {
            return None;
        }
        let outputs = serde_json::json!({ (self.name.as_str()): value });
        match crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(&outputs) {
            Ok(Some(form)) => form
                .validate(crate::agent_task_review_dossier::PrTitlePolicy::from_declaration(self))
                .err()
                .map(|error| error.message),
            Ok(None) => None,
            Err(error) => Some(error.message),
        }
    }
}

/// Describes how a declared output relates to durable evidence without imposing
/// caller-specific evidence semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutputEvidenceRelationship {
    pub relationship: String,
    pub evidence: AgentTaskEvidenceRef,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_declarations: Vec<AgentTaskOutputDeclaration>,
    /// Provider-neutral runtime processes attached to this task. Runtime adapters
    /// project these declarations into their native configuration format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_tools: Vec<AgentTaskRuntimeTool>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Provider-facing request materialized on the host that executes the task.
#[derive(Debug, Clone)]
pub struct AgentTaskExecutorRequest {
    pub request: AgentTaskRequest,
    pub artifacts_path: PathBuf,
    pub(crate) artifact_store_root: PathBuf,
    pub artifacts_path_provenance: AgentTaskArtifactsPathProvenance,
    pub(crate) artifacts_root_identity:
        crate::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity,
}

impl Serialize for AgentTaskExecutorRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut value = serde_json::to_value(&self.request).map_err(serde::ser::Error::custom)?;
        let object = value.as_object_mut().ok_or_else(|| {
            serde::ser::Error::custom("agent task request must serialize as an object")
        })?;
        // Callers retain declarations for durable planning, but providers do not
        // receive runtime tool declarations.
        object.remove("runtime_tools");
        object.insert(
            "artifacts_path".to_string(),
            serde_json::to_value(&self.artifacts_path).map_err(serde::ser::Error::custom)?,
        );
        object.insert(
            "artifacts_path_provenance".to_string(),
            serde_json::to_value(&self.artifacts_path_provenance)
                .map_err(serde::ser::Error::custom)?,
        );
        value.serialize(serializer)
    }
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
    /// Declare that a controller, rather than the provider attempt, owns remote
    /// publication. This is a deliberate request contract, not an inference
    /// from the attempt workspace's blocked push remote.
    pub fn controller_owns_publication(&mut self) {
        if !self.metadata.is_object() {
            self.metadata = Value::Object(Default::default());
        }
        self.metadata["publication"] = serde_json::json!({
            "capability": "unavailable",
            "owner": "controller",
            "status": "not_attempted"
        });
    }

    pub fn publication_is_controller_owned(&self) -> bool {
        self.metadata
            .pointer("/publication/owner")
            .and_then(Value::as_str)
            == Some("controller")
            && self
                .metadata
                .pointer("/publication/capability")
                .and_then(Value::as_str)
                == Some("unavailable")
    }

    pub fn capability_requirements(&self) -> Result<AgentTaskCapabilityRequirements, String> {
        super::capabilities::requirements_from_metadata(
            &self.metadata,
            &self.executor.required_capabilities,
        )
    }

    pub fn record_capability_evidence(&mut self, evidence: AgentTaskCapabilityEvidence) {
        if !self.metadata.is_object() {
            self.metadata = Value::Object(Default::default());
        }
        self.metadata["capability_evidence"] =
            serde_json::to_value(evidence).unwrap_or(Value::Null);
    }
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
