use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[cfg(test)]
use crate::core::redaction::RedactionPolicy;

pub const AGENT_TASK_REQUEST_SCHEMA: &str = "homeboy/agent-task-request/v1";
pub const AGENT_TASK_OUTCOME_SCHEMA: &str = "homeboy/agent-task-outcome/v1";
pub const AGENT_TASK_ARTIFACT_SCHEMA: &str = "homeboy/agent-task-artifact/v1";
pub const AGENT_TASK_WORKFLOW_SCHEMA: &str = "homeboy/agent-task-workflow/v1";
pub const AGENT_TASK_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-aggregate/v1";

/// Provider-neutral executor adapter contract for agent task backends.
///
/// Core owns the task and outcome schemas plus this lifecycle boundary. Runtime
/// integrations provide concrete adapters that validate capabilities, prepare a
/// workspace, start execution, poll async work, cancel in-flight handles, collect
/// artifacts, and translate provider payloads into [`AgentTaskOutcome`].
/// Extensions register adapters by exposing a value that implements this trait
/// to the scheduler or fan-out coordinator that owns dispatch for a task batch.
pub trait AgentTaskExecutorAdapter {
    fn capabilities(&self) -> AgentTaskExecutorCapabilities;

    fn validate(&self, request: &AgentTaskRequest) -> AgentTaskExecutorResult<()>;

    fn prepare_workspace(
        &mut self,
        request: &AgentTaskRequest,
    ) -> AgentTaskExecutorResult<AgentTaskPreparedWorkspace>;

    fn start_task(
        &mut self,
        request: &AgentTaskRequest,
        workspace: &AgentTaskPreparedWorkspace,
    ) -> AgentTaskExecutorResult<AgentTaskStart>;

    fn poll_progress(
        &mut self,
        handle: &AgentTaskExecutionHandle,
    ) -> AgentTaskExecutorResult<AgentTaskProgress>;

    fn cancel_task(
        &mut self,
        handle: &AgentTaskExecutionHandle,
    ) -> AgentTaskExecutorResult<AgentTaskOutcome>;

    fn collect_artifacts(
        &mut self,
        handle: &AgentTaskExecutionHandle,
    ) -> AgentTaskExecutorResult<Vec<AgentTaskArtifact>>;

    fn normalize_outcome(
        &self,
        request: &AgentTaskRequest,
        handle: &AgentTaskExecutionHandle,
        provider_payload: Value,
    ) -> AgentTaskExecutorResult<AgentTaskOutcome>;
}

pub type AgentTaskExecutorResult<T> = std::result::Result<T, AgentTaskExecutorError>;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorError {
    pub classification: AgentTaskFailureClassification,
    pub message: String,
}

impl AgentTaskExecutorError {
    pub fn new(classification: AgentTaskFailureClassification, message: impl Into<String>) -> Self {
        Self {
            classification,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentTaskExecutorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for AgentTaskExecutorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub struct AgentTaskSchedulerOptions {
    pub max_polls: usize,
}

#[cfg(test)]
impl Default for AgentTaskSchedulerOptions {
    fn default() -> Self {
        Self { max_polls: 1 }
    }
}

#[cfg(test)]
fn execute_agent_task_with_adapter(
    adapter: &mut dyn AgentTaskExecutorAdapter,
    request: &AgentTaskRequest,
    options: AgentTaskSchedulerOptions,
) -> AgentTaskExecutorResult<AgentTaskOutcome> {
    adapter.validate(request)?;
    let workspace = adapter.prepare_workspace(request)?;
    let start = adapter.start_task(request, &workspace)?;

    if let Some(outcome) = start.outcome {
        return Ok(outcome);
    }

    let mut handle = start.handle;

    for _ in 0..options.max_polls {
        let progress = adapter.poll_progress(&handle)?;
        handle = progress.handle.clone();

        if let Some(outcome) = progress.outcome {
            return Ok(outcome);
        }

        if progress.state.is_terminal() {
            if let Some(provider_payload) = progress.provider_payload {
                let mut outcome = adapter.normalize_outcome(request, &handle, provider_payload)?;
                let mut artifacts = adapter.collect_artifacts(&handle)?;
                outcome.artifacts.append(&mut artifacts);
                return Ok(outcome);
            }

            return Err(AgentTaskExecutorError::new(
                AgentTaskFailureClassification::ExecutionFailed,
                "executor reached a terminal state without an outcome",
            ));
        }
    }

    let outcome = adapter.cancel_task(&handle)?;
    Ok(outcome)
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
    #[serde(default)]
    pub mode: AgentTaskWorkspaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub materialization: Value,
}

impl Default for AgentTaskWorkspace {
    fn default() -> Self {
        Self {
            mode: AgentTaskWorkspaceMode::Ephemeral,
            root: None,
            materialization: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskWorkspaceMode {
    Ephemeral,
    Existing,
    Materialized,
}

impl Default for AgentTaskWorkspaceMode {
    fn default() -> Self {
        Self::Ephemeral
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskAggregateReport {
    #[serde(default = "aggregate_schema")]
    pub schema: String,
    pub summary: AgentTaskAggregateSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskReconciliationItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inventory: Vec<AgentTaskArtifactInventoryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apply_candidates: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_report_candidates: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retry_plan: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_candidates: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matrix: Vec<AgentTaskMatrixRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAggregateSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub no_op: usize,
    pub timed_out: usize,
    pub provider_error: usize,
    pub unable_to_remediate: usize,
    pub follow_up_issue: usize,
    pub cancelled: usize,
    pub apply_candidates: usize,
    pub issue_report_candidates: usize,
    pub retry_candidates: usize,
    pub review_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskReconciliationItem {
    pub task_id: String,
    pub status: AgentTaskOutcomeStatus,
    pub decision: AgentTaskReconciliationDecision,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<AgentTaskArtifactInventoryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<AgentTaskFollowUp>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskReconciliationDecision {
    ApplyCandidate,
    IssueReportCandidate,
    RetryCandidate,
    ReviewCandidate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactInventoryItem {
    pub task_id: String,
    pub artifact_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskDecisionRef {
    pub task_id: String,
    pub decision: AgentTaskReconciliationDecision,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskMatrixRow {
    pub task_id: String,
    pub status: AgentTaskOutcomeStatus,
    pub axes: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metrics: Value,
}

fn aggregate_agent_task_outcomes(outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateReport {
    let mut report = AgentTaskAggregateReport {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        summary: AgentTaskAggregateSummary {
            total: outcomes.len(),
            ..AgentTaskAggregateSummary::default()
        },
        tasks: Vec::with_capacity(outcomes.len()),
        artifact_inventory: Vec::new(),
        apply_candidates: Vec::new(),
        issue_report_candidates: Vec::new(),
        retry_plan: Vec::new(),
        review_candidates: Vec::new(),
        matrix: Vec::new(),
    };

    for outcome in outcomes {
        count_status(&mut report.summary, outcome.status);

        let artifacts: Vec<_> = outcome
            .artifacts
            .iter()
            .map(|artifact| inventory_item(&outcome.task_id, artifact))
            .collect();
        report.artifact_inventory.extend(artifacts.clone());

        let (decision, reason, artifact_ids) = reconcile_outcome(outcome);
        let decision_ref = AgentTaskDecisionRef {
            task_id: outcome.task_id.clone(),
            decision,
            reason: reason.clone(),
            artifact_ids,
        };

        match decision {
            AgentTaskReconciliationDecision::ApplyCandidate => {
                report.summary.apply_candidates += 1;
                report.apply_candidates.push(decision_ref);
            }
            AgentTaskReconciliationDecision::IssueReportCandidate => {
                report.summary.issue_report_candidates += 1;
                report.issue_report_candidates.push(decision_ref);
            }
            AgentTaskReconciliationDecision::RetryCandidate => {
                report.summary.retry_candidates += 1;
                report.retry_plan.push(decision_ref);
            }
            AgentTaskReconciliationDecision::ReviewCandidate => {
                report.summary.review_candidates += 1;
                report.review_candidates.push(decision_ref);
            }
        }

        if let Some(row) = matrix_row(outcome) {
            report.matrix.push(row);
        }

        report.tasks.push(AgentTaskReconciliationItem {
            task_id: outcome.task_id.clone(),
            status: outcome.status,
            decision,
            reason,
            summary: outcome.summary.clone(),
            artifacts,
            evidence_refs: outcome.evidence_refs.clone(),
            diagnostics: outcome.diagnostics.clone(),
            follow_up: outcome.follow_up.clone(),
        });
    }

    report
}

impl From<&[AgentTaskOutcome]> for AgentTaskAggregateReport {
    fn from(outcomes: &[AgentTaskOutcome]) -> Self {
        aggregate_agent_task_outcomes(outcomes)
    }
}

impl From<Vec<AgentTaskOutcome>> for AgentTaskAggregateReport {
    fn from(outcomes: Vec<AgentTaskOutcome>) -> Self {
        aggregate_agent_task_outcomes(&outcomes)
    }
}

impl fmt::Display for AgentTaskAggregateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut markdown = String::new();
        markdown.push_str("## Agent Task Outcomes\n\n");
        markdown.push_str(&format!(
            "- total: {}\n- succeeded: {}\n- failed: {}\n- no-op: {}\n- timed out: {}\n- provider errors: {}\n- apply candidates: {}\n- issue report candidates: {}\n- retry candidates: {}\n- review candidates: {}\n\n",
            self.summary.total,
            self.summary.succeeded,
            self.summary.failed,
            self.summary.no_op,
            self.summary.timed_out,
            self.summary.provider_error,
            self.summary.apply_candidates,
            self.summary.issue_report_candidates,
            self.summary.retry_candidates,
            self.summary.review_candidates
        ));

        markdown.push_str("| Task | Status | Decision | Reason | Artifacts |\n");
        markdown.push_str("| --- | --- | --- | --- | --- |\n");
        for task in &self.tasks {
            let artifacts = task
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            markdown.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                escape_markdown_table_cell(&task.task_id),
                task.status.as_str(),
                task.decision.as_str(),
                escape_markdown_table_cell(&task.reason),
                escape_markdown_table_cell(&artifacts)
            ));
        }

        if !self.matrix.is_empty() {
            markdown.push_str("\n## Matrix\n\n");
            markdown.push_str("| Task | Status | Axes | Metrics |\n");
            markdown.push_str("| --- | --- | --- | --- |\n");
            for row in &self.matrix {
                markdown.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    escape_markdown_table_cell(&row.task_id),
                    row.status.as_str(),
                    escape_markdown_table_cell(&row.axes.to_string()),
                    escape_markdown_table_cell(&row.metrics.to_string())
                ));
            }
        }

        f.write_str(&markdown)
    }
}

impl AgentTaskOutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::NoOp => "no_op",
            Self::UnableToRemediate => "unable_to_remediate",
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::Failed => "failed",
            Self::FollowUpIssue => "follow_up_issue",
            Self::Cancelled => "cancelled",
        }
    }
}

impl AgentTaskReconciliationDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApplyCandidate => "apply_candidate",
            Self::IssueReportCandidate => "issue_report_candidate",
            Self::RetryCandidate => "retry_candidate",
            Self::ReviewCandidate => "review_candidate",
        }
    }
}

fn count_status(summary: &mut AgentTaskAggregateSummary, status: AgentTaskOutcomeStatus) {
    match status {
        AgentTaskOutcomeStatus::Succeeded => summary.succeeded += 1,
        AgentTaskOutcomeStatus::NoOp => summary.no_op += 1,
        AgentTaskOutcomeStatus::UnableToRemediate => summary.unable_to_remediate += 1,
        AgentTaskOutcomeStatus::ProviderError => summary.provider_error += 1,
        AgentTaskOutcomeStatus::Timeout => summary.timed_out += 1,
        AgentTaskOutcomeStatus::Failed => summary.failed += 1,
        AgentTaskOutcomeStatus::FollowUpIssue => summary.follow_up_issue += 1,
        AgentTaskOutcomeStatus::Cancelled => summary.cancelled += 1,
    }
}

fn reconcile_outcome(
    outcome: &AgentTaskOutcome,
) -> (AgentTaskReconciliationDecision, String, Vec<String>) {
    let rejected_artifact_ids = outcome
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact_flag(artifact, "rejected") || artifact_flag(artifact, "false_positive")
        })
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    if !rejected_artifact_ids.is_empty() {
        return (
            AgentTaskReconciliationDecision::IssueReportCandidate,
            "artifact marked rejected or false-positive".to_string(),
            rejected_artifact_ids,
        );
    }

    if matches!(
        outcome.status,
        AgentTaskOutcomeStatus::ProviderError | AgentTaskOutcomeStatus::Timeout
    ) || matches!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider | AgentTaskFailureClassification::Timeout)
    ) {
        return (
            AgentTaskReconciliationDecision::RetryCandidate,
            "provider error or timeout is retryable".to_string(),
            artifact_ids(outcome),
        );
    }

    if matches!(outcome.status, AgentTaskOutcomeStatus::FollowUpIssue)
        || outcome
            .follow_up
            .as_ref()
            .is_some_and(|follow_up| follow_up.kind == "issue_report")
    {
        return (
            AgentTaskReconciliationDecision::IssueReportCandidate,
            "outcome requested a follow-up issue report".to_string(),
            artifact_ids(outcome),
        );
    }

    let apply_artifact_ids = outcome
        .artifacts
        .iter()
        .filter(|artifact| is_apply_artifact(artifact))
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    if matches!(outcome.status, AgentTaskOutcomeStatus::Succeeded) && !apply_artifact_ids.is_empty()
    {
        return (
            AgentTaskReconciliationDecision::ApplyCandidate,
            "succeeded with reviewable patch/artifact output".to_string(),
            apply_artifact_ids,
        );
    }

    (
        AgentTaskReconciliationDecision::ReviewCandidate,
        match outcome.status {
            AgentTaskOutcomeStatus::NoOp => "no-op outcome needs review".to_string(),
            AgentTaskOutcomeStatus::UnableToRemediate => {
                "unable-to-remediate outcome needs review".to_string()
            }
            AgentTaskOutcomeStatus::Cancelled => "cancelled task needs review".to_string(),
            AgentTaskOutcomeStatus::Failed => "failed task needs review".to_string(),
            AgentTaskOutcomeStatus::Succeeded => {
                "succeeded without apply-back artifact".to_string()
            }
            AgentTaskOutcomeStatus::ProviderError
            | AgentTaskOutcomeStatus::Timeout
            | AgentTaskOutcomeStatus::FollowUpIssue => unreachable!("handled above"),
        },
        artifact_ids(outcome),
    )
}

fn artifact_ids(outcome: &AgentTaskOutcome) -> Vec<String> {
    outcome
        .artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect()
}

fn inventory_item(task_id: &str, artifact: &AgentTaskArtifact) -> AgentTaskArtifactInventoryItem {
    AgentTaskArtifactInventoryItem {
        task_id: task_id.to_string(),
        artifact_id: artifact.id.clone(),
        kind: artifact.kind.clone(),
        name: artifact.name.clone(),
        path: artifact.path.clone(),
        url: artifact.url.clone(),
        sha256: artifact.sha256.clone(),
    }
}

fn is_apply_artifact(artifact: &AgentTaskArtifact) -> bool {
    matches!(
        artifact.kind.as_str(),
        "patch" | "diff" | "change_artifact" | "workspace_patch" | "artifact"
    ) || artifact_flag(artifact, "approved")
}

fn artifact_flag(artifact: &AgentTaskArtifact, key: &str) -> bool {
    artifact
        .metadata
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn matrix_row(outcome: &AgentTaskOutcome) -> Option<AgentTaskMatrixRow> {
    let axes = outcome.metadata.get("matrix_axes")?.clone();
    Some(AgentTaskMatrixRow {
        task_id: outcome.task_id.clone(),
        status: outcome.status,
        axes,
        metrics: outcome
            .metadata
            .get("metrics")
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn escape_markdown_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
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

fn aggregate_schema() -> String {
    AGENT_TASK_AGGREGATE_SCHEMA.to_string()
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
                mode: AgentTaskWorkspaceMode::Materialized,
                root: Some("/workspace/repo".to_string()),
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

    #[test]
    fn core_scheduler_uses_fake_adapter_for_async_task_completion() {
        struct FakeAdapter {
            polls: usize,
        }

        impl AgentTaskExecutorAdapter for FakeAdapter {
            fn capabilities(&self) -> AgentTaskExecutorCapabilities {
                AgentTaskExecutorCapabilities {
                    backend: "fake".to_string(),
                    selector: None,
                    capabilities: vec!["workspace_write".to_string()],
                    supports_sync_completion: false,
                    supports_async_polling: true,
                    supports_streaming: false,
                    supports_cancel: true,
                }
            }

            fn validate(&self, request: &AgentTaskRequest) -> AgentTaskExecutorResult<()> {
                if request.executor.backend == self.capabilities().backend {
                    Ok(())
                } else {
                    Err(AgentTaskExecutorError::new(
                        AgentTaskFailureClassification::InvalidInput,
                        "request targets a different backend",
                    ))
                }
            }

            fn prepare_workspace(
                &mut self,
                request: &AgentTaskRequest,
            ) -> AgentTaskExecutorResult<AgentTaskPreparedWorkspace> {
                Ok(AgentTaskPreparedWorkspace {
                    mode: request.workspace.mode,
                    root: request.workspace.root.clone(),
                    metadata: json!({ "prepared": true }),
                })
            }

            fn start_task(
                &mut self,
                request: &AgentTaskRequest,
                _workspace: &AgentTaskPreparedWorkspace,
            ) -> AgentTaskExecutorResult<AgentTaskStart> {
                Ok(AgentTaskStart {
                    handle: AgentTaskExecutionHandle {
                        task_id: request.task_id.clone(),
                        backend: request.executor.backend.clone(),
                        run_id: "fake-run".to_string(),
                        stream_uri: None,
                        metadata: json!({}),
                    },
                    outcome: None,
                })
            }

            fn poll_progress(
                &mut self,
                handle: &AgentTaskExecutionHandle,
            ) -> AgentTaskExecutorResult<AgentTaskProgress> {
                self.polls += 1;
                let state = if self.polls == 1 {
                    AgentTaskExecutionState::Running
                } else {
                    AgentTaskExecutionState::Succeeded
                };

                Ok(AgentTaskProgress {
                    handle: handle.clone(),
                    state,
                    events: Vec::new(),
                    provider_payload: state.is_terminal().then(|| json!({ "summary": "fixed" })),
                    outcome: None,
                })
            }

            fn cancel_task(
                &mut self,
                handle: &AgentTaskExecutionHandle,
            ) -> AgentTaskExecutorResult<AgentTaskOutcome> {
                Ok(AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: handle.task_id.clone(),
                    status: AgentTaskOutcomeStatus::Cancelled,
                    summary: Some("cancelled after poll budget".to_string()),
                    failure_classification: None,
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    workflow: None,
                    follow_up: None,
                    metadata: json!({}),
                })
            }

            fn collect_artifacts(
                &mut self,
                _handle: &AgentTaskExecutionHandle,
            ) -> AgentTaskExecutorResult<Vec<AgentTaskArtifact>> {
                Ok(vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("changes.patch".to_string()),
                    path: Some("artifacts/changes.patch".to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: Some(42),
                    sha256: None,
                    metadata: json!({}),
                }])
            }

            fn normalize_outcome(
                &self,
                request: &AgentTaskRequest,
                _handle: &AgentTaskExecutionHandle,
                provider_payload: Value,
            ) -> AgentTaskExecutorResult<AgentTaskOutcome> {
                Ok(AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: request.task_id.clone(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: provider_payload["summary"].as_str().map(str::to_string),
                    failure_classification: None,
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    workflow: None,
                    follow_up: None,
                    metadata: json!({}),
                })
            }
        }

        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-async".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "fake".to_string(),
                selector: None,
                required_capabilities: Vec::new(),
                model: None,
                config: json!({}),
            },
            instructions: "Make the scoped change.".to_string(),
            inputs: json!({}),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            metadata: json!({}),
        };

        let mut adapter = FakeAdapter { polls: 0 };
        let outcome = execute_agent_task_with_adapter(
            &mut adapter,
            &request,
            AgentTaskSchedulerOptions { max_polls: 2 },
        )
        .expect("task executes");

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert_eq!(outcome.summary.as_deref(), Some("fixed"));
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(adapter.polls, 2);
    }

    #[test]
    fn core_scheduler_cancels_when_async_poll_budget_is_exhausted() {
        struct NeverFinishesAdapter;

        impl AgentTaskExecutorAdapter for NeverFinishesAdapter {
            fn capabilities(&self) -> AgentTaskExecutorCapabilities {
                AgentTaskExecutorCapabilities {
                    backend: "fake".to_string(),
                    selector: None,
                    capabilities: Vec::new(),
                    supports_sync_completion: false,
                    supports_async_polling: true,
                    supports_streaming: false,
                    supports_cancel: true,
                }
            }

            fn validate(&self, _request: &AgentTaskRequest) -> AgentTaskExecutorResult<()> {
                Ok(())
            }

            fn prepare_workspace(
                &mut self,
                _request: &AgentTaskRequest,
            ) -> AgentTaskExecutorResult<AgentTaskPreparedWorkspace> {
                Ok(AgentTaskPreparedWorkspace {
                    mode: AgentTaskWorkspaceMode::Ephemeral,
                    root: None,
                    metadata: json!({}),
                })
            }

            fn start_task(
                &mut self,
                request: &AgentTaskRequest,
                _workspace: &AgentTaskPreparedWorkspace,
            ) -> AgentTaskExecutorResult<AgentTaskStart> {
                Ok(AgentTaskStart {
                    handle: AgentTaskExecutionHandle {
                        task_id: request.task_id.clone(),
                        backend: request.executor.backend.clone(),
                        run_id: "run".to_string(),
                        stream_uri: None,
                        metadata: json!({}),
                    },
                    outcome: None,
                })
            }

            fn poll_progress(
                &mut self,
                handle: &AgentTaskExecutionHandle,
            ) -> AgentTaskExecutorResult<AgentTaskProgress> {
                Ok(AgentTaskProgress {
                    handle: handle.clone(),
                    state: AgentTaskExecutionState::Running,
                    events: Vec::new(),
                    provider_payload: None,
                    outcome: None,
                })
            }

            fn cancel_task(
                &mut self,
                handle: &AgentTaskExecutionHandle,
            ) -> AgentTaskExecutorResult<AgentTaskOutcome> {
                Ok(AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: handle.task_id.clone(),
                    status: AgentTaskOutcomeStatus::Cancelled,
                    summary: Some("cancelled".to_string()),
                    failure_classification: None,
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    workflow: None,
                    follow_up: None,
                    metadata: json!({}),
                })
            }

            fn collect_artifacts(
                &mut self,
                _handle: &AgentTaskExecutionHandle,
            ) -> AgentTaskExecutorResult<Vec<AgentTaskArtifact>> {
                Ok(Vec::new())
            }

            fn normalize_outcome(
                &self,
                _request: &AgentTaskRequest,
                _handle: &AgentTaskExecutionHandle,
                _provider_payload: Value,
            ) -> AgentTaskExecutorResult<AgentTaskOutcome> {
                unreachable!("running progress should not normalize")
            }
        }

        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-timeout".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "fake".to_string(),
                selector: None,
                required_capabilities: Vec::new(),
                model: None,
                config: json!({}),
            },
            instructions: "Run until cancelled.".to_string(),
            inputs: json!({}),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            metadata: json!({}),
        };

        let mut adapter = NeverFinishesAdapter;
        let outcome = execute_agent_task_with_adapter(
            &mut adapter,
            &request,
            AgentTaskSchedulerOptions { max_polls: 1 },
        )
        .expect("task cancels");

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Cancelled);
    }

    #[test]
    fn aggregate_outcomes_classifies_apply_retry_issue_and_review_candidates() {
        let outcomes = vec![
            outcome(
                "apply",
                AgentTaskOutcomeStatus::Succeeded,
                vec![artifact("patch-1", "patch", json!({ "approved": true }))],
            ),
            AgentTaskOutcome {
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                ..outcome("retry", AgentTaskOutcomeStatus::ProviderError, Vec::new())
            },
            AgentTaskOutcome {
                follow_up: Some(AgentTaskFollowUp {
                    kind: "issue_report".to_string(),
                    title: "Needs issue".to_string(),
                    body: None,
                    uri: Some("https://example.test/issues/1".to_string()),
                }),
                ..outcome(
                    "issue",
                    AgentTaskOutcomeStatus::FollowUpIssue,
                    vec![artifact("report", "report", json!({}))],
                )
            },
            outcome(
                "review",
                AgentTaskOutcomeStatus::UnableToRemediate,
                Vec::new(),
            ),
        ];

        let report = aggregate_agent_task_outcomes(&outcomes);

        assert_eq!(report.schema, AGENT_TASK_AGGREGATE_SCHEMA);
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.succeeded, 1);
        assert_eq!(report.summary.provider_error, 1);
        assert_eq!(report.summary.unable_to_remediate, 1);
        assert_eq!(report.summary.follow_up_issue, 1);
        assert_eq!(report.summary.apply_candidates, 1);
        assert_eq!(report.summary.retry_candidates, 1);
        assert_eq!(report.summary.issue_report_candidates, 1);
        assert_eq!(report.summary.review_candidates, 1);
        assert_eq!(report.apply_candidates[0].task_id, "apply");
        assert_eq!(report.apply_candidates[0].artifact_ids, vec!["patch-1"]);
        assert_eq!(report.retry_plan[0].task_id, "retry");
        assert_eq!(report.issue_report_candidates[0].task_id, "issue");
        assert_eq!(report.review_candidates[0].task_id, "review");
    }

    #[test]
    fn aggregate_outcomes_preserves_artifacts_evidence_and_matrix_metrics() {
        let mut item = outcome(
            "matrix-task",
            AgentTaskOutcomeStatus::Succeeded,
            vec![artifact("diff", "diff", json!({}))],
        );
        item.evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "log".to_string(),
            uri: "artifact://matrix-task/log".to_string(),
            label: Some("runner log".to_string()),
        }];
        item.metadata = json!({
            "matrix_axes": { "model": "fast", "scenario": "audit" },
            "metrics": { "duration_ms": 42 }
        });

        let report = aggregate_agent_task_outcomes(&[item]);

        assert_eq!(report.artifact_inventory.len(), 1);
        assert_eq!(report.artifact_inventory[0].task_id, "matrix-task");
        assert_eq!(
            report.tasks[0].evidence_refs[0].uri,
            "artifact://matrix-task/log"
        );
        assert_eq!(report.matrix.len(), 1);
        assert_eq!(report.matrix[0].axes["model"], json!("fast"));
        assert_eq!(report.matrix[0].metrics["duration_ms"], json!(42));
    }

    #[test]
    fn aggregate_outcomes_routes_rejected_artifacts_to_issue_reports() {
        let report = aggregate_agent_task_outcomes(&[outcome(
            "false-positive",
            AgentTaskOutcomeStatus::Succeeded,
            vec![artifact(
                "candidate",
                "patch",
                json!({ "false_positive": true }),
            )],
        )]);

        assert!(report.apply_candidates.is_empty());
        assert_eq!(report.issue_report_candidates[0].task_id, "false-positive");
        assert_eq!(
            report.issue_report_candidates[0].reason,
            "artifact marked rejected or false-positive"
        );
    }

    #[test]
    fn aggregate_report_renders_pr_comment_markdown() {
        let report = aggregate_agent_task_outcomes(&[outcome(
            "task|one",
            AgentTaskOutcomeStatus::NoOp,
            Vec::new(),
        )]);

        let markdown = report.to_string();

        assert!(markdown.contains("## Agent Task Outcomes"));
        assert!(markdown.contains("- total: 1"));
        assert!(markdown.contains("| Task | Status | Decision | Reason | Artifacts |"));
        assert!(markdown.contains("| task\\|one | no_op | review_candidate |"));
        assert!(markdown.contains("no-op outcome needs review"));
    }

    #[test]
    fn aggregate_report_renders_matrix_markdown() {
        let mut item = outcome("matrix", AgentTaskOutcomeStatus::Succeeded, Vec::new());
        item.metadata = json!({
            "matrix_axes": { "model": "fast" },
            "metrics": { "duration_ms": 42 }
        });
        let report = aggregate_agent_task_outcomes(&[item]);

        let markdown = report.to_string();

        assert!(markdown.contains("## Matrix"));
        assert!(markdown.contains("| Task | Status | Axes | Metrics |"));
        assert!(markdown.contains("matrix | succeeded"));
        assert!(markdown.contains("duration_ms"));
    }

    fn outcome(
        task_id: &str,
        status: AgentTaskOutcomeStatus,
        artifacts: Vec<AgentTaskArtifact>,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            status,
            summary: Some(format!("{task_id} summary")),
            failure_classification: None,
            artifacts,
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    fn artifact(id: &str, kind: &str, metadata: Value) -> AgentTaskArtifact {
        AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: id.to_string(),
            kind: kind.to_string(),
            name: Some(format!("{id}.txt")),
            path: Some(format!("artifacts/{id}.txt")),
            url: None,
            mime: None,
            size_bytes: Some(12),
            sha256: Some(format!("sha256:{id}")),
            metadata,
        }
    }
}
