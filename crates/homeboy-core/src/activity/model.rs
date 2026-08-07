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

/// One runner consulted while federating runner-resident activity.
///
/// A run offloaded to a Lab runner is recorded *on that runner* until it
/// reports back, so a controller-local read cannot see it. This records what
/// the federation actually asked, per runner, so an empty answer is never
/// confused with an unasked question (#W3-15).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRunnerSource {
    pub runner_id: String,
    /// Whether the controller holds a connected session for this runner.
    pub connected: bool,
    /// Whether this runner's active-job view was actually read. `false` for a
    /// runner with no connected session — nothing was asked and no network was
    /// performed, which is not a failure.
    pub queried: bool,
    /// Runner-resident items federated into this report from this runner.
    pub items: usize,
    /// Why a connected runner did not answer. Present only when `connected` is
    /// `true` and `queried` is `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How the runner-resident activity source behaved for one report.
///
/// This is the accountability record for the federation: whether it ran, which
/// runners it consulted, and whether any connected runner failed to answer. A
/// runner outage degrades this to `partial` — it never fails the command.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRunnerFederation {
    /// Whether runner federation was attempted at all. `false` when the caller
    /// opted out, or when no runner layer is registered in this process.
    pub enabled: bool,
    /// `true` when at least one *connected* runner did not return its
    /// active-job view, so runner-resident work may be missing from this report.
    pub partial: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runners: Vec<ActivityRunnerSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub counts: ActivityCounts,
    pub items: Vec<ActivityItem>,
    /// Whether this surface reconciled while reading.
    ///
    /// `activity` is a deliberately **non**-reconciling read model, so this is
    /// always `false` here — while `agent-task status` is "a reconciling read
    /// that writes" and `runs watch` reconciles on purpose. The two surfaces can
    /// therefore legitimately report different states for the same run at the
    /// same instant, *and calling one changes what the other returns*. Emitting
    /// the flag lets a consumer tell which kind of answer it is holding instead
    /// of inferring it from the command name (#W3-15).
    #[serde(default)]
    pub reconciled: bool,
    /// `true` when at least one activity source could not be fully read, so
    /// this report is partial rather than complete. Never an error: every
    /// source that did answer is still returned.
    #[serde(default)]
    pub partial: bool,
    /// Per-runner accounting for the runner-resident source, so a `partial`
    /// report names what it could not reach.
    #[serde(default)]
    pub runner_federation: ActivityRunnerFederation,
    /// Agent-task record-health summary, carried as JSON so core does not depend
    /// on the agent-task health type. Supplied by the agent-task activity
    /// provider (null when the agent-task subsystem is absent).
    #[serde(default)]
    pub agent_task_record_health: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}
