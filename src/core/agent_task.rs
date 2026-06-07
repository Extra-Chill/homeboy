use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use crate::core::redaction::RedactionPolicy;

pub use super::agent_task_aggregate::{
    AgentTaskAggregateReport, AgentTaskAggregateSummary, AgentTaskArtifactInventoryItem,
    AgentTaskDecisionRef, AgentTaskMatrixRow, AgentTaskReconciliationDecision,
    AgentTaskReconciliationItem, AGENT_TASK_AGGREGATE_SCHEMA,
};
pub use super::agent_task_fanout::{
    AgentTaskFanoutAggregate, AgentTaskFanoutPlan, AgentTaskFanoutPlane, AgentTaskFanoutScheduler,
    AGENT_TASK_FANOUT_AGGREGATE_SCHEMA, AGENT_TASK_FANOUT_PLAN_SCHEMA,
};

pub const AGENT_TASK_REQUEST_SCHEMA: &str = "homeboy/agent-task-request/v1";
pub const AGENT_TASK_OUTCOME_SCHEMA: &str = "homeboy/agent-task-outcome/v1";
pub const AGENT_TASK_ARTIFACT_SCHEMA: &str = "homeboy/agent-task-artifact/v1";
pub const AGENT_TASK_WORKFLOW_SCHEMA: &str = "homeboy/agent-task-workflow/v1";
pub const AGENT_TASK_MATRIX_PLAN_SCHEMA: &str = "homeboy/agent-task-matrix-plan/v1";
pub const AGENT_TASK_MATRIX_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-matrix-aggregate/v1";

mod matrix;

pub(crate) use matrix::expand_agent_task_matrix;
pub use matrix::{
    AgentTaskMatrixAggregate, AgentTaskMatrixAggregateCell, AgentTaskMatrixAxis,
    AgentTaskMatrixCell, AgentTaskMatrixError, AgentTaskMatrixPlan,
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
    pub mode: AgentTaskWorkspaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskExecutionHandle {
    pub task_id: String,
    pub backend: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskStart {
    pub handle: AgentTaskExecutionHandle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentTaskOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskProgress {
    pub handle: AgentTaskExecutionHandle,
    pub state: AgentTaskExecutionState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentTaskProgressEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentTaskOutcome>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskExecutionState {
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}

impl AgentTaskExecutionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskProgressEvent {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
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
    #[serde(default)]
    pub policy: AgentTaskPolicy,
    #[serde(default)]
    pub limits: AgentTaskLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[cfg(test)]
impl AgentTaskRequest {
    pub(crate) fn redacted(&self) -> Self {
        let policy = RedactionPolicy::default();
        let mut redacted = self.clone();
        redacted.instructions = policy.redact_string(&redacted.instructions);
        redacted.inputs = policy.redact_json(&redacted.inputs);
        redacted.executor.config = policy.redact_json(&redacted.executor.config);
        redacted.workspace.materialization =
            policy.redact_json(&redacted.workspace.materialization);
        redacted.metadata = policy.redact_json(&redacted.metadata);
        redacted
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskExecutor {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskSourceRef {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskWorkspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub mode: AgentTaskWorkspaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub materialization: Value,
}

impl Default for AgentTaskWorkspace {
    fn default() -> Self {
        Self {
            kind: None,
            mode: AgentTaskWorkspaceMode::Ephemeral,
            root: None,
            slug: None,
            component_id: None,
            branch: None,
            base_ref: None,
            task_url: None,
            cleanup: None,
            materialization: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AgentTaskWorkspaceMode {
    #[default]
    Ephemeral,
    Existing,
    Materialized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPolicy {
    #[serde(default = "default_read_policy")]
    pub read: String,
    #[serde(default = "default_write_policy")]
    pub write: String,
    #[serde(default = "default_apply_policy")]
    pub apply: String,
}

impl Default for AgentTaskPolicy {
    fn default() -> Self {
        Self {
            read: default_read_policy(),
            write: default_write_policy(),
            apply: default_apply_policy(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<u64>,
}

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
    Timeout,
    PolicyDenied,
    CapabilityMissing,
    InvalidInput,
    ExecutionFailed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskArtifact {
    #[serde(default = "artifact_schema")]
    pub schema: String,
    pub id: String,
    pub kind: String,
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
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[cfg(test)]
impl AgentTaskArtifact {
    fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.name = self.name.map(|value| policy.redact_string(&value));
        self.path = self.path.map(|value| policy.redact_string(&value));
        self.url = self.url.map(|value| policy.redact_url(&value));
        self.metadata = policy.redact_json(&self.metadata);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskEvidenceRef {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskDiagnostic {
    pub class: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

#[cfg(test)]
impl AgentTaskDiagnostic {
    fn redacted_with(mut self, policy: &RedactionPolicy) -> Self {
        self.message = policy.redact_string(&self.message);
        self.data = policy.redact_json(&self.data);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskFollowUp {
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
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

fn request_schema() -> String {
    AGENT_TASK_REQUEST_SCHEMA.to_string()
}

fn outcome_schema() -> String {
    AGENT_TASK_OUTCOME_SCHEMA.to_string()
}

fn artifact_schema() -> String {
    AGENT_TASK_ARTIFACT_SCHEMA.to_string()
}

fn workflow_schema() -> String {
    AGENT_TASK_WORKFLOW_SCHEMA.to_string()
}

fn default_read_policy() -> String {
    "workspace".to_string()
}

fn default_write_policy() -> String {
    "artifacts_only".to_string()
}

fn default_apply_policy() -> String {
    "propose_only".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trips_generic_agent_task_shape() {
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            group_key: Some("audit-batch".to_string()),
            parent_plan_id: Some("plan-1".to_string()),
            executor: AgentTaskExecutor {
                backend: "browser_sandbox".to_string(),
                selector: Some("lab-a".to_string()),
                required_capabilities: vec!["structured_output".to_string()],
                secret_env: Vec::new(),
                model: Some("quality-model".to_string()),
                config: json!({ "account": "team-a" }),
            },
            instructions: "Fix the scoped finding and return artifacts.".to_string(),
            inputs: json!({ "finding_id": "finding-1" }),
            source_refs: vec![AgentTaskSourceRef {
                kind: "git".to_string(),
                uri: "https://example.test/repo.git".to_string(),
                revision: Some("abc123".to_string()),
            }],
            workspace: AgentTaskWorkspace {
                kind: None,
                mode: AgentTaskWorkspaceMode::Materialized,
                root: Some("/workspace/repo".to_string()),
                slug: Some("repo".to_string()),
                component_id: None,
                branch: None,
                base_ref: None,
                task_url: None,
                cleanup: None,
                materialization: json!({ "component": "repo" }),
            },
            policy: AgentTaskPolicy {
                read: "workspace".to_string(),
                write: "workspace".to_string(),
                apply: "propose_only".to_string(),
            },
            limits: AgentTaskLimits {
                timeout_ms: Some(300_000),
                max_runtime_ms: Some(240_000),
                max_output_bytes: Some(1_000_000),
            },
            expected_artifacts: vec!["patch".to_string(), "report".to_string()],
            metadata: json!({ "batch": 1 }),
        };

        let encoded = serde_json::to_string(&request).expect("serialize request");
        let decoded: AgentTaskRequest = serde_json::from_str(&encoded).expect("decode request");

        assert_eq!(decoded, request);
        assert_eq!(decoded.schema, AGENT_TASK_REQUEST_SCHEMA);
    }

    #[test]
    fn outcome_round_trips_success_noop_timeout_and_follow_up_shapes() {
        let statuses = [
            AgentTaskOutcomeStatus::Succeeded,
            AgentTaskOutcomeStatus::NoOp,
            AgentTaskOutcomeStatus::UnableToRemediate,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskOutcomeStatus::Timeout,
            AgentTaskOutcomeStatus::FollowUpIssue,
        ];

        for status in statuses {
            let outcome = AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                status,
                summary: Some("completed".to_string()),
                failure_classification: match status {
                    AgentTaskOutcomeStatus::ProviderError => {
                        Some(AgentTaskFailureClassification::Provider)
                    }
                    AgentTaskOutcomeStatus::Timeout => {
                        Some(AgentTaskFailureClassification::Timeout)
                    }
                    _ => None,
                },
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "artifact-1".to_string(),
                    kind: "patch".to_string(),
                    name: Some("fix.patch".to_string()),
                    path: Some("artifacts/fix.patch".to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: Some(128),
                    sha256: Some("sha256:abc".to_string()),
                    metadata: json!({}),
                }],
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "log".to_string(),
                    uri: "artifact://run/log".to_string(),
                    label: Some("runner log".to_string()),
                }],
                diagnostics: vec![AgentTaskDiagnostic {
                    class: "provider".to_string(),
                    message: "provider returned retryable error".to_string(),
                    data: json!({}),
                }],
                outputs: json!({ "issue_number": 3447 }),
                workflow: None,
                follow_up: Some(AgentTaskFollowUp {
                    kind: "issue_report".to_string(),
                    title: "Needs human decision".to_string(),
                    body: Some("The requested fix needs product direction.".to_string()),
                    uri: None,
                }),
                metadata: json!({}),
            };

            let value = serde_json::to_value(&outcome).expect("serialize outcome");
            let decoded: AgentTaskOutcome = serde_json::from_value(value).expect("decode outcome");

            assert_eq!(decoded, outcome);
            assert_eq!(decoded.schema, AGENT_TASK_OUTCOME_SCHEMA);
            assert_eq!(decoded.artifacts[0].schema, AGENT_TASK_ARTIFACT_SCHEMA);
        }
    }

    #[test]
    fn outcome_round_trips_nested_workflow_step_evidence() {
        let outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "model-kimi-site-a".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("diagnose step failed".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "screenshot-1".to_string(),
                kind: "screenshot".to_string(),
                name: Some("homepage.png".to_string()),
                path: Some("artifacts/homepage.png".to_string()),
                url: None,
                mime: Some("image/png".to_string()),
                size_bytes: Some(2048),
                sha256: Some("sha256:def".to_string()),
                metadata: json!({ "viewport": "desktop" }),
            }],
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: Some(AgentTaskWorkflowEvidence {
                schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
                id: "site-build".to_string(),
                label: Some("Site build".to_string()),
                steps: vec![
                    AgentTaskWorkflowStepEvidence {
                        id: "generate".to_string(),
                        label: Some("Generate artifact".to_string()),
                        status: AgentTaskWorkflowStepStatus::Succeeded,
                        depends_on: Vec::new(),
                        started_at: Some("2026-05-31T23:00:00Z".to_string()),
                        finished_at: Some("2026-05-31T23:00:03Z".to_string()),
                        duration_ms: Some(3_000),
                        metrics: json!({ "tokens": 1200 }),
                        artifact_refs: Vec::new(),
                        diagnostics: Vec::new(),
                        suggestions: Vec::new(),
                        metadata: json!({}),
                    },
                    AgentTaskWorkflowStepEvidence {
                        id: "diagnose".to_string(),
                        label: Some("Diagnose imported site".to_string()),
                        status: AgentTaskWorkflowStepStatus::Failed,
                        depends_on: vec!["generate".to_string(), "screenshot".to_string()],
                        started_at: Some("2026-05-31T23:00:04Z".to_string()),
                        finished_at: Some("2026-05-31T23:00:05Z".to_string()),
                        duration_ms: Some(1_000),
                        metrics: json!({ "fallback_blocks": 2 }),
                        artifact_refs: vec![AgentTaskEvidenceRef {
                            kind: "artifact".to_string(),
                            uri: "artifact://screenshot-1".to_string(),
                            label: Some("Desktop screenshot".to_string()),
                        }],
                        diagnostics: vec![AgentTaskDiagnostic {
                            class: "visual_regression".to_string(),
                            message: "fallback blocks remain".to_string(),
                            data: json!({ "count": 2 }),
                        }],
                        suggestions: vec![AgentTaskWorkflowStepSuggestion {
                            kind: "repair".to_string(),
                            title: "Run import repair".to_string(),
                            body: Some("Repair unsupported fallback blocks.".to_string()),
                            uri: Some("homeboy://tasks/model-kimi-site-a/repair".to_string()),
                        }],
                        metadata: json!({ "phase": "diagnostics" }),
                    },
                ],
                metadata: json!({ "executor": "wp-codebox" }),
            }),
            follow_up: None,
            metadata: json!({}),
        };

        let value = serde_json::to_value(&outcome).expect("serialize outcome");
        let decoded: AgentTaskOutcome = serde_json::from_value(value).expect("decode outcome");

        assert_eq!(decoded, outcome);
        let workflow = decoded.workflow.expect("workflow evidence");
        assert_eq!(workflow.schema, AGENT_TASK_WORKFLOW_SCHEMA);
        assert_eq!(
            workflow.steps[0].status,
            AgentTaskWorkflowStepStatus::Succeeded
        );
        assert_eq!(workflow.steps[1].depends_on, vec!["generate", "screenshot"]);
        assert_eq!(
            workflow.steps[1].artifact_refs[0].uri,
            "artifact://screenshot-1"
        );
    }

    #[test]
    fn redacted_request_removes_sensitive_fields() {
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-secret".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "cli_agent".to_string(),
                selector: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: json!({ "api_key": "secret-value" }),
            },
            instructions: "Use token=abc123 while testing.".to_string(),
            inputs: json!({ "authorization": "Bearer abc123", "safe": "value" }),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            metadata: json!({ "refresh_token": "secret-refresh" }),
        };

        let redacted = serde_json::to_value(request.redacted()).expect("redacted json");

        assert!(!redacted.to_string().contains("secret-value"));
        assert!(!redacted.to_string().contains("abc123"));
        assert!(!redacted.to_string().contains("secret-refresh"));
        assert_eq!(redacted["inputs"]["safe"], json!("value"));
    }

    #[test]
    fn redacted_outcome_removes_sensitive_artifact_and_diagnostic_data() {
        let outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-secret".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("failed with password=hunter2".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "log".to_string(),
                kind: "log".to_string(),
                name: None,
                path: None,
                url: Some("https://example.test/log?token=abc123".to_string()),
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: json!({ "cookie": "session=secret" }),
            }],
            evidence_refs: Vec::new(),
            diagnostics: vec![AgentTaskDiagnostic {
                class: "provider".to_string(),
                message: "Authorization: Bearer abc123".to_string(),
                data: json!({ "client_secret": "secret" }),
            }],
            workflow: Some(AgentTaskWorkflowEvidence {
                schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
                id: "secret-workflow".to_string(),
                label: Some("Use token=abc123".to_string()),
                steps: vec![AgentTaskWorkflowStepEvidence {
                    id: "diagnose".to_string(),
                    label: Some("Inspect password=hunter2".to_string()),
                    status: AgentTaskWorkflowStepStatus::Failed,
                    depends_on: Vec::new(),
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    metrics: json!({ "api_key": "secret-value" }),
                    artifact_refs: Vec::new(),
                    diagnostics: vec![AgentTaskDiagnostic {
                        class: "workflow".to_string(),
                        message: "Authorization: Bearer abc123".to_string(),
                        data: json!({ "password": "hunter2" }),
                    }],
                    suggestions: vec![AgentTaskWorkflowStepSuggestion {
                        kind: "repair".to_string(),
                        title: "Use token=abc123".to_string(),
                        body: Some("password=hunter2".to_string()),
                        uri: Some("https://example.test/repair?token=abc123".to_string()),
                    }],
                    metadata: json!({ "refresh_token": "secret-refresh" }),
                }],
                metadata: json!({ "client_secret": "secret" }),
            }),
            follow_up: None,
            outputs: json!({ "api_key": "secret-value", "safe": "value" }),
            metadata: json!({ "safe": "value", "password": "hunter2" }),
        };

        let redacted = serde_json::to_value(outcome.redacted()).expect("redacted json");

        assert!(!redacted.to_string().contains("hunter2"));
        assert!(!redacted.to_string().contains("abc123"));
        assert!(!redacted.to_string().contains("session=secret"));
        assert_eq!(redacted["metadata"]["safe"], json!("value"));
    }

    #[test]
    fn executor_contract_round_trips_provider_neutral_lifecycle_shapes() {
        let capabilities = AgentTaskExecutorCapabilities {
            backend: "local_session".to_string(),
            selector: Some("default".to_string()),
            capabilities: vec!["workspace_write".to_string(), "artifacts".to_string()],
            supports_sync_completion: true,
            supports_async_polling: true,
            supports_streaming: true,
            supports_cancel: true,
        };

        let handle = AgentTaskExecutionHandle {
            task_id: "task-1".to_string(),
            backend: capabilities.backend.clone(),
            run_id: "run-1".to_string(),
            stream_uri: Some("event://run-1".to_string()),
            metadata: json!({ "attempt": 1 }),
        };

        let progress = AgentTaskProgress {
            handle,
            state: AgentTaskExecutionState::Running,
            events: vec![AgentTaskProgressEvent {
                kind: "log".to_string(),
                message: "started".to_string(),
                data: json!({ "sequence": 1 }),
            }],
            provider_payload: None,
            outcome: None,
        };

        let encoded = serde_json::to_value((&capabilities, &progress)).expect("serialize");
        let (decoded_capabilities, decoded_progress): (
            AgentTaskExecutorCapabilities,
            AgentTaskProgress,
        ) = serde_json::from_value(encoded).expect("decode");

        assert_eq!(decoded_capabilities, capabilities);
        assert_eq!(decoded_progress, progress);
        assert!(!decoded_progress.state.is_terminal());
    }
}
