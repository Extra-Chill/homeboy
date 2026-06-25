use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::schema::outcome_schema;
use super::schema::workflow_schema;
use super::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFollowUp,
    AgentTaskTypedArtifact,
};

#[cfg(test)]
use crate::core::redaction::RedactionPolicy;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutcome {
    #[serde(default = "outcome_schema")]
    pub schema: String,
    pub task_id: String,
    pub status: AgentTaskOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<AgentTaskFailureClassification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<AgentTaskArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub typed_artifacts: Vec<AgentTaskTypedArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskDiagnostic>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub outputs: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<AgentTaskWorkflowEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<AgentTaskFollowUp>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[cfg(test)]
impl AgentTaskOutcome {
    pub(crate) fn redacted(&self) -> Self {
        let policy = RedactionPolicy::default();
        let mut redacted = self.clone();
        redacted.summary = redacted.summary.map(|value| policy.redact_string(&value));
        redacted.artifacts = redacted
            .artifacts
            .into_iter()
            .map(|artifact| artifact.redacted_with(&policy))
            .collect();
        redacted.typed_artifacts = redacted
            .typed_artifacts
            .into_iter()
            .map(|artifact| artifact.redacted_with(&policy))
            .collect();
        redacted.diagnostics = redacted
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.redacted_with(&policy))
            .collect();
        redacted.outputs = policy.redact_json(&redacted.outputs);
        redacted.workflow = redacted
            .workflow
            .map(|workflow| workflow.redacted_with(&policy));
        redacted.metadata = policy.redact_json(&redacted.metadata);
        redacted
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskOutcomeStatus {
    Succeeded,
    NoOp,
    UnableToRemediate,
    ProviderError,
    Timeout,
    Failed,
    FollowUpIssue,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFailureClassification {
    Provider,
    /// Transient provider/network failure (timeouts, connection resets, cURL
    /// error 28, 5xx, temporarily-unavailable). These are safe to retry with
    /// bounded backoff because the same request can succeed on a later attempt.
    Transient,
    Timeout,
    PolicyDenied,
    CapabilityMissing,
    InvalidInput,
    ExecutionFailed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskWorkflowEvidence {
    #[serde(default = "workflow_schema")]
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<AgentTaskWorkflowStepEvidence>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[cfg(test)]
impl AgentTaskWorkflowEvidence {
    fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.label = self.label.map(|value| policy.redact_string(&value));
        self.steps = self
            .steps
            .into_iter()
            .map(|step| step.redacted_with(policy))
            .collect();
        self.metadata = policy.redact_json(&self.metadata);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskWorkflowStepEvidence {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: AgentTaskWorkflowStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metrics: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<AgentTaskWorkflowStepSuggestion>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[cfg(test)]
impl AgentTaskWorkflowStepEvidence {
    fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.label = self.label.map(|value| policy.redact_string(&value));
        self.diagnostics = self
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.redacted_with(policy))
            .collect();
        self.suggestions = self
            .suggestions
            .into_iter()
            .map(|suggestion| suggestion.redacted_with(policy))
            .collect();
        self.metrics = policy.redact_json(&self.metrics);
        self.metadata = policy.redact_json(&self.metadata);
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskWorkflowStepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskWorkflowStepSuggestion {
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[cfg(test)]
impl AgentTaskWorkflowStepSuggestion {
    fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.title = policy.redact_string(&self.title);
        self.body = self.body.map(|value| policy.redact_string(&value));
        self.uri = self.uri.map(|value| policy.redact_url(&value));
        self
    }
}
