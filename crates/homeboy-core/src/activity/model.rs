//! Activity data model — the serializable types that make up an activity
//! report, plus the small state predicates over them. Extracted from the
//! `activity` module to keep each file within one responsibility (#9794).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::run_lifecycle_status::RunLifecycleStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityScope {
    ActiveRecent,
    All,
}

pub type ActivityState = RunLifecycleStatus;

pub fn is_active(state: ActivityState) -> bool {
    matches!(state, ActivityState::Queued | ActivityState::Running)
}

pub fn is_failure(state: ActivityState) -> bool {
    !is_active(state) && !matches!(state, ActivityState::Succeeded | ActivityState::Cancelled)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityNextAction {
    pub label: String,
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRunnerRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCrossRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_task_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityEvidenceRef {
    pub id: String,
    pub kind: String,
    pub uri: String,
}

/// A store-specific view retained with the canonical activity item so state
/// reconciliation remains inspectable without returning duplicate work items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivitySourceProjection {
    pub source_store: String,
    pub id: String,
    pub state: ActivityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// A non-authoritative source state retained for operators investigating a
/// reconciled activity item. The top-level item state remains authoritative.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityStateConflict {
    pub source_store: String,
    pub id: String,
    pub state: ActivityState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityItem {
    pub id: String,
    pub kind: String,
    pub source_store: String,
    pub state: ActivityState,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub runner: ActivityRunnerRefs,
    #[serde(default)]
    pub refs: ActivityCrossRefs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ActivityEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ActivityEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_projections: Vec<ActivitySourceProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_conflicts: Vec<ActivityStateConflict>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<ActivityNextAction>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCounts {
    pub total: usize,
    pub active: usize,
    pub queued: usize,
    pub running: usize,
    pub succeeded: usize,
    pub partial_failure: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub timed_out: usize,
    pub stale: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub counts: ActivityCounts,
    pub items: Vec<ActivityItem>,
    /// Agent-task record-health summary, carried as JSON so core does not depend
    /// on the agent-task health type. Supplied by the agent-task activity
    /// provider (null when the agent-task subsystem is absent).
    #[serde(default)]
    pub agent_task_record_health: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}
