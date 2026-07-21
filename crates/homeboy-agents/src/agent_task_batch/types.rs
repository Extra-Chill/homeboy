use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task::{AgentTaskArtifact, AgentTaskEvidenceRef};
use crate::agent_task_lifecycle::{AgentTaskRunArtifacts, AgentTaskRunState};

pub const AGENT_TASK_BATCH_SCHEMA: &str = "homeboy/agent-task-batch/v1";
pub const AGENT_TASK_BATCH_STATUS_SCHEMA: &str = "homeboy/agent-task-batch-status/v1";
pub const AGENT_TASK_BATCH_ARTIFACTS_SCHEMA: &str = "homeboy/agent-task-batch-artifacts/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskBatchRecord {
    pub schema: String,
    pub batch_id: String,
    pub plan_id: String,
    pub state: AgentTaskBatchState,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub task_count: usize,
    pub child_runs: Vec<AgentTaskBatchChildRun>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskBatchChildRun {
    pub task_id: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskBatchState {
    Queued,
    Running,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskBatchStatusReport {
    pub schema: &'static str,
    pub batch: AgentTaskBatchRecord,
    pub totals: AgentTaskBatchTotals,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_child_runs: Vec<AgentTaskBatchChildIssue>,
    /// Terminal children whose provider attempt finished but whose promotion,
    /// gates, and PR finalization were never completed (typically because the
    /// synchronous coordinator exited). These can be idempotently harvested with
    /// `commands.resume` (#9525).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resumable_child_runs: Vec<AgentTaskBatchResumableChild>,
    /// True when one or more children are resumable and the batch can be carried
    /// to PR-ready finalization by re-running the coordinator.
    pub resumable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    pub commands: AgentTaskBatchCommands,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskBatchResumableChild {
    pub task_id: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
    /// Why this child is resumable — e.g. terminal with a patch but no PR.
    pub reason: String,
}
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AgentTaskBatchTotals {
    pub queued: usize,
    pub running: usize,
    pub succeeded: usize,
    pub partial_failure: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub unavailable: usize,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskBatchCommands {
    pub status: String,
    pub artifacts: String,
    pub run_next: String,
    /// Idempotently harvest terminal-but-unfinalized children through promotion,
    /// gates, commit, push, and PR finalization after coordinator loss (#9525).
    pub resume: String,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskBatchArtifactsReport {
    pub schema: &'static str,
    pub batch_id: String,
    pub summary: AgentTaskBatchArtifactsSummary,
    pub manifest: AgentTaskBatchArtifactsManifest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_child_runs: Vec<AgentTaskBatchChildIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    pub child_runs: Vec<AgentTaskBatchChildArtifacts>,
}
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AgentTaskBatchArtifactsSummary {
    pub child_runs: usize,
    pub artifacts: usize,
    pub evidence_refs: usize,
}
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct AgentTaskBatchArtifactsManifest {
    pub artifacts: Vec<AgentTaskBatchArtifactEntry>,
    pub evidence_refs: Vec<AgentTaskBatchEvidenceRefEntry>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskBatchArtifactEntry {
    pub task_id: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
    pub artifact: AgentTaskArtifact,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskBatchEvidenceRefEntry {
    pub task_id: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
    pub evidence_ref: AgentTaskEvidenceRef,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskBatchChildArtifacts {
    pub task_id: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
    pub artifact_count: usize,
    pub evidence_ref_count: usize,
    pub artifacts: AgentTaskRunArtifacts,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskBatchChildIssue {
    pub task_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_state: Option<AgentTaskRunState>,
    pub status_command: String,
    pub artifacts_command: String,
    pub problem: String,
}
