use super::*;

pub(crate) mod schemas {
    pub(crate) const RUN: &str = "homeboy/agent-task-run/v1";
    pub(crate) const RUN_LOG: &str = "homeboy/agent-task-run-log/v1";
    pub(crate) const EVENT: &str = "homeboy/agent-task-event/v1";
    pub(crate) const RUN_STATUS: &str = "homeboy/agent-task-run-status/v1";
    pub(crate) const RUN_ARTIFACTS: &str = "homeboy/agent-task-run-artifacts/v1";
    pub(crate) const COOK_INDEX: &str = "homeboy/agent-task-cook-index/v1";
}

pub const AGENT_TASK_RECORD_HEALTH_SCHEMA: &str = "homeboy/agent-task-record-health/v1";
pub const AGENT_TASK_RECORD_RECONCILIATION_SCHEMA: &str =
    "homeboy/agent-task-record-reconciliation/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskRecordHealthReason {
    MissingMetadata,
    MalformedMetadata,
    LegacySchema,
    ConflictingProjections,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordHealthItem {
    pub run_id: String,
    pub reason: AgentTaskRecordHealthReason,
    pub quarantined: bool,
    pub remediation: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordHealthSummary {
    pub schema: String,
    pub healthy: usize,
    pub malformed: usize,
    pub legacy: usize,
    pub conflicting: usize,
    pub quarantined: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<AgentTaskRecordHealthItem>,
}

impl AgentTaskRecordHealthSummary {
    pub(crate) fn healthy() -> Self {
        Self {
            schema: AGENT_TASK_RECORD_HEALTH_SCHEMA.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordReconciliationItem {
    pub run_id: String,
    pub reason: AgentTaskRecordHealthReason,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordReconciliationReport {
    pub schema: String,
    pub dry_run: bool,
    pub considered: usize,
    pub migrated: usize,
    pub quarantined: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<AgentTaskRecordReconciliationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunRecord {
    pub schema: String,
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub plan_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<crate::agent_task_scheduler::AgentTaskAggregateTotals>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskRunTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_handles: Vec<AgentTaskRunProviderHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_executor_evidence: Option<AgentTaskLatestExecutorEvidence>,
    #[serde(default)]
    pub lifecycle: RunLifecycleRecord,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookIndex {
    #[serde(default = "cook_index_schema")]
    pub schema: String,
    pub cook_id: String,
    pub latest_run_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<AgentTaskCookIndexAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookIndexAttempt {
    pub attempt: u32,
    pub run_id: String,
    pub recorded_at: String,
}

fn cook_index_schema() -> String {
    schemas::COOK_INDEX.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLatestExecutorEvidence {
    pub task_id: String,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub input_ref: AgentTaskEvidenceRef,
    pub normalized_output_ref: AgentTaskEvidenceRef,
    pub outcome_ref: AgentTaskEvidenceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_component_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub typed_artifact_expectations: Vec<String>,
}

impl AgentTaskLatestExecutorEvidence {
    pub(crate) fn refs(&self) -> [AgentTaskEvidenceRef; 3] {
        [
            self.input_ref.clone(),
            self.normalized_output_ref.clone(),
            self.outcome_ref.clone(),
        ]
    }
}

impl AgentTaskRunRecord {
    pub(crate) fn record_runner_metadata(&mut self, reclaimed_stale: bool) {
        let metadata = self.ensure_metadata_object();
        metadata.insert("runner_pid".to_string(), json!(std::process::id()));
        metadata.insert("runner_started_at".to_string(), json!(now_timestamp()));
        if reclaimed_stale {
            metadata.insert("reclaimed_stale_running".to_string(), json!(true));
        } else {
            metadata.remove("reclaimed_stale_running");
        }
        metadata.remove("stale_running");
        metadata.remove("stale_running_reason");
    }

    pub(crate) fn annotate_stale_running(&mut self) {
        if self.state != AgentTaskRunState::Running || self.owner_process_is_running() {
            return;
        }

        if self.runner_job_id().is_some() && self.has_fresh_update() {
            return;
        }

        let reason = if self.runner_job_id().is_some() {
            "runner_job_unverified_after_daemon_restart"
        } else if self.owner_pid().is_some() {
            "owner_process_not_running"
        } else {
            "missing_runner_pid"
        };
        let provider_handle_count = self.provider_handles.len();
        let metadata = self.ensure_metadata_object();
        metadata.insert("stale_running".to_string(), json!(true));
        metadata.insert("stale_running_reason".to_string(), json!(reason));
        metadata.insert(
            "provider_boundary".to_string(),
            json!({
                "status": if provider_handle_count == 0 { "absent" } else { "recorded" },
                "provider_handle_count": provider_handle_count,
            }),
        );
        metadata.insert("retryable".to_string(), json!(true));
    }

    /// A controller cannot infer progress from a runner connection that is no
    /// longer available. Preserve the last confirmed heartbeat as evidence,
    /// while making its liveness explicitly unknown.
    pub(crate) fn annotate_runner_disconnected(&mut self) {
        if self.state != AgentTaskRunState::Running || !self.is_runner_backed() {
            return;
        }
        let metadata = self.ensure_metadata_object();
        metadata.insert("runner_liveness".to_string(), json!("disconnected"));
        metadata.insert("stale_running".to_string(), json!(true));
        metadata.insert(
            "stale_running_reason".to_string(),
            json!("runner_disconnected"),
        );
        metadata.insert("retryable".to_string(), json!(true));
    }

    /// A reachable runner job is authoritative liveness evidence. Clear only
    /// the controller's disconnected/stale projection, not unrelated metadata.
    pub(crate) fn record_runner_reachable(&mut self) {
        let metadata = self.ensure_metadata_object();
        metadata.insert("runner_liveness".to_string(), json!("reachable"));
        metadata.remove("stale_running");
        metadata.remove("stale_running_reason");
        metadata.remove("retryable");
    }

    pub(crate) fn owner_process_is_running(&self) -> bool {
        self.owner_pid().is_some_and(crate::process::pid_is_running)
    }

    pub(crate) fn owner_pid(&self) -> Option<u32> {
        self.metadata
            .get("runner_pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
    }

    pub fn runner_job_id(&self) -> Option<&str> {
        self.metadata
            .get("runner_job_id")
            .or_else(|| self.metadata.get("job_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn runner_id(&self) -> Option<&str> {
        self.metadata
            .get("runner_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    fn has_fresh_update(&self) -> bool {
        self.updated_at
            .as_deref()
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
            .is_some_and(|updated_at| {
                chrono::Utc::now()
                    .signed_duration_since(updated_at.with_timezone(&chrono::Utc))
                    .num_minutes()
                    < 30
            })
    }

    /// A run is runner-backed when its durable record carries a runner id or
    /// runner job id. For these, the recorded owner pid lives on the *runner*
    /// host, not this controller, so local pid signalling cannot reach the
    /// provider process tree.
    pub(crate) fn is_runner_backed(&self) -> bool {
        self.runner_id().is_some() || self.runner_job_id().is_some()
    }

    pub(crate) fn ensure_metadata_object(&mut self) -> &mut serde_json::Map<String, Value> {
        if !self.metadata.is_object() {
            self.metadata = json!({});
        }
        self.metadata.as_object_mut().expect("metadata object")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskRunState {
    Queued,
    Running,
    Succeeded,
    /// Backward-compatible projection for older durable records.
    CandidateRecoverable,
    PartialRecoverable,
    PartialFailure,
    Failed,
    Cancelled,
}

/// Canonical projection of a run's aggregate state onto the generic
/// `RunExecutionState` carried by `RunLifecycleRecord`. The two enums share
/// every agent-task variant 1:1 (`RunExecutionState` additionally models the
/// generic `Unknown` default), so this single `From` keeps `record.state` and
/// `record.lifecycle.execution.state` in lockstep instead of a hand-synced map.
impl From<AgentTaskRunState> for RunExecutionState {
    fn from(state: AgentTaskRunState) -> Self {
        match state {
            AgentTaskRunState::Queued => RunExecutionState::Queued,
            AgentTaskRunState::Running => RunExecutionState::Running,
            AgentTaskRunState::Succeeded => RunExecutionState::Succeeded,
            AgentTaskRunState::CandidateRecoverable => RunExecutionState::PartialFailure,
            AgentTaskRunState::PartialRecoverable => RunExecutionState::PartialFailure,
            AgentTaskRunState::PartialFailure => RunExecutionState::PartialFailure,
            AgentTaskRunState::Failed => RunExecutionState::Failed,
            AgentTaskRunState::Cancelled => RunExecutionState::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRunTask {
    pub task_id: String,
    pub state: AgentTaskState,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AgentTaskArtifactRef {
    pub task_id: String,
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunProviderHandle {
    #[serde(
        default,
        skip_serializing_if = "AgentTaskExecutionHandleKind::is_provider_run"
    )]
    pub kind: AgentTaskExecutionHandleKind,
    pub task_id: String,
    pub backend: String,
    pub provider_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentTaskState>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunLog {
    pub schema: String,
    pub run_id: String,
    pub events: Vec<AgentTaskProgressEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalized_events: Vec<AgentTaskEventEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskEventEnvelope {
    pub schema: String,
    pub run_id: String,
    pub task_id: String,
    pub sequence: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub status: AgentTaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub progress: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunStatus {
    pub schema: String,
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub totals: AgentTaskAggregateTotals,
    pub latest_event_cursor: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalized_events: Vec<AgentTaskEventEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunArtifacts {
    pub schema: String,
    pub run_id: String,
    pub artifacts: Vec<AgentTaskArtifact>,
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
}
