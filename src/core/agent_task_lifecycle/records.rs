use super::*;

pub(crate) mod schemas {
    pub(crate) const RUN: &str = "homeboy/agent-task-run/v1";
    pub(crate) const RUN_LOG: &str = "homeboy/agent-task-run-log/v1";
    pub(crate) const EVENT: &str = "homeboy/agent-task-event/v1";
    pub(crate) const RUN_STATUS: &str = "homeboy/agent-task-run-status/v1";
    pub(crate) const RUN_ARTIFACTS: &str = "homeboy/agent-task-run-artifacts/v1";
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
    pub totals: Option<crate::core::agent_task_scheduler::AgentTaskAggregateTotals>,
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

        let reason = if self.runner_job_id().is_some() {
            "runner_job_unverified_after_daemon_restart"
        } else if self.owner_pid().is_some() {
            "owner_process_not_running"
        } else {
            "missing_runner_pid"
        };
        let metadata = self.ensure_metadata_object();
        metadata.insert("stale_running".to_string(), json!(true));
        metadata.insert("stale_running_reason".to_string(), json!(reason));
        metadata.insert("retryable".to_string(), json!(true));
    }

    pub(crate) fn owner_process_is_running(&self) -> bool {
        self.owner_pid()
            .is_some_and(crate::core::process::pid_is_running)
    }

    pub(crate) fn owner_pid(&self) -> Option<u32> {
        self.metadata
            .get("runner_pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
    }

    pub(crate) fn runner_job_id(&self) -> Option<&str> {
        self.metadata
            .get("runner_job_id")
            .or_else(|| self.metadata.get("job_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub(crate) fn runner_id(&self) -> Option<&str> {
        self.metadata
            .get("runner_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
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
    PartialFailure,
    Failed,
    Cancelled,
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
