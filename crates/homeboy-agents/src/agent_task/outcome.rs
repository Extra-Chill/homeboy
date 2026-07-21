use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::schema::outcome_schema;
use super::schema::workflow_schema;
use super::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFollowUp,
    AgentTaskTypedArtifact,
};

#[cfg(test)]
use homeboy_core::redaction::RedactionPolicy;

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

impl Default for AgentTaskOutcome {
    /// A defaulted outcome carries the current schema, an empty `task_id`, and
    /// `Failed` status, with every optional/collection field empty. `Failed` is
    /// the deliberate status default: a not-yet-populated outcome must never
    /// read as `Succeeded`. Construct with `..Default::default()` and override
    /// `task_id`/`status` (and whatever else is meaningful) to drop the ~10
    /// lines of `None`/`Vec::new()`/`Value::Null` boilerplate every call site
    /// otherwise repeats.
    fn default() -> Self {
        Self {
            schema: outcome_schema(),
            task_id: String::new(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: None,
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

impl AgentTaskOutcome {
    /// The concrete provider model this outcome recorded, if any.
    ///
    /// This is the single authoritative reader for "what model did this run
    /// actually execute under" — the value lives in `metadata.model` and every
    /// lifecycle reconciliation path (aggregate → task, provider handle
    /// backfill, terminal-record repair) needs it. Previously each site
    /// re-implemented `metadata.get("model") → as_str → trim → non-empty` with
    /// subtly different handling (one path forgot the trim), which is why the
    /// same "reconcile the terminal provider model" fix landed three times
    /// (#9405/#9415/#9423). Centralizing the read here gives one definition:
    /// present, non-blank after trimming, or `None`.
    pub fn selected_model(&self) -> Option<&str> {
        self.metadata
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
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

// These enums now live in the homeboy-lab-contract crate (the lab-contract
// `agent_task_config` policy types reference them). Re-exported here so all
// existing `agent_task::{AgentTaskOutcomeStatus, AgentTaskFailureClassification}`
// call sites across core are unchanged.
pub use homeboy_lab_contract::agent_task_outcome::{
    AgentTaskFailureClassification, AgentTaskOutcomeStatus,
};

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

#[cfg(test)]
mod default_construction_tests {
    use super::*;

    #[test]
    fn default_matches_the_fully_spelled_out_empty_outcome() {
        let via_default = AgentTaskOutcome {
            task_id: "cook".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            ..Default::default()
        };
        let verbose = AgentTaskOutcome {
            schema: outcome_schema(),
            task_id: "cook".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };
        assert_eq!(via_default, verbose);
    }

    #[test]
    fn default_status_is_failed_never_succeeded() {
        assert_eq!(
            AgentTaskOutcome::default().status,
            AgentTaskOutcomeStatus::Failed
        );
    }
}
