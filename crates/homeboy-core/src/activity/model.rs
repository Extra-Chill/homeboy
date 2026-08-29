//! Activity data model — the serializable types that make up an activity
//! report, plus the small state predicates over them. Extracted from the
//! `activity` module to keep each file within one responsibility (#9794).

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::run_lifecycle_status::RunLifecycleStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityScope {
    ActiveRecent,
    All,
}

/// How much per-record detail an activity report carries.
///
/// The default coordination view ([`ActivityScope::ActiveRecent`]) answers
/// "what is happening and what can I do next", so each retained record is
/// compacted to its identity, state, timestamps, cross-refs, and a couple of
/// follow-up actions. The diagnostic surface that drops — full
/// artifact/evidence ref rosters, per-store projections, state conflicts,
/// task-identity enumerations, and the raw command line — stays available
/// through [`ActivityScope::All`] (`activity list --all`, `activity show`,
/// `activity watch`) and per-record artifact commands (#13617).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityDetail {
    Compact,
    Full,
}

impl ActivityDetail {
    /// The detail level a report scope implies.
    pub fn for_scope(scope: ActivityScope) -> Self {
        match scope {
            ActivityScope::ActiveRecent => Self::Compact,
            ActivityScope::All => Self::Full,
        }
    }
}

/// Follow-up actions retained per record in the compact default view. Matches
/// the two `next:` lines the human projection renders per row.
pub const COMPACT_NEXT_ACTIONS_PER_ITEM: usize = 2;

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

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ActivityCrossRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Runner-job / execution reference. Not a run-id alias.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
}

impl<'de> Deserialize<'de> for ActivityCrossRefs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            run_id: Option<String>,
            #[serde(default)]
            agent_task_run_id: Option<String>,
            #[serde(default)]
            runner_job_id: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            run_id: raw.run_id.or(raw.agent_task_run_id),
            runner_job_id: raw.runner_job_id,
        })
    }
}

/// Task context carried by sources that know the submitted work.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityContext {
    /// Operator-facing task reference — a link to follow, not a task identity.
    /// [`Self::identities`] is the identity surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Every canonical task/destination identity represented by this activity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<ActivityTaskIdentity>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityTaskIdentity {
    /// Operator-facing task reference attached to this identity, not the identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

impl ActivityContext {
    pub fn is_empty(&self) -> bool {
        self.task_url.is_none()
            && self.repository.is_none()
            && self.worktree.is_none()
            && self.identities.is_empty()
    }
}

/// Selectors for a bounded unified activity lookup.
///
/// `repository` and `worktree` match [`ActivityContext::identities`].
/// `task_url` matches the operator task reference, not a distinct identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityFilter {
    pub task_url: Option<String>,
    pub repository: Option<String>,
    pub worktree: Option<String>,
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
    #[serde(default, skip_serializing_if = "ActivityContext::is_empty")]
    pub context: ActivityContext,
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

/// The `source_store` of the worktree provider projection. Items from this
/// source are open resources, not executing work (#13620).
pub const WORKTREE_RESOURCE_SOURCE_STORE: &str = "worktree.provider";

/// What one activity item *is*, independent of its state.
///
/// Executing work is a unit of work the system runs to completion: an
/// observation run, an agent-task record, a daemon job, or a runner-resident
/// job. An open resource is inventory that work uses — a worktree — whose
/// presence says nothing about whether anything is executing in it. The two
/// classes share the state vocabulary but not its liveness meaning: an open
/// worktree projects `running` because it is held, not because a process is
/// executing, so counts that answer "what is executing right now" must scope
/// to [`ActivityWorkClass::Executing`] (#13620).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityWorkClass {
    Executing,
    OpenResource,
}

impl ActivityItem {
    /// Compact this item for the default coordination view.
    ///
    /// Identity, state, timestamps, runner/cross refs, and
    /// [`COMPACT_NEXT_ACTIONS_PER_ITEM`] follow-up actions survive; the
    /// duplicated diagnostic rosters do not. The report-level `next_actions`
    /// rollup is computed from the full action set *before* compaction, so
    /// the compact view still advertises a follow-up for every retained
    /// record even when that record's own roster was capped (#13617).
    pub(crate) fn compact_for_default_view(&mut self) {
        self.command = None;
        self.cwd = None;
        self.context.identities.clear();
        self.artifacts.clear();
        self.evidence.clear();
        self.source_projections.clear();
        self.state_conflicts.clear();
        self.next_actions.truncate(COMPACT_NEXT_ACTIONS_PER_ITEM);
    }

    pub fn work_class(&self) -> ActivityWorkClass {
        if self.source_store == WORKTREE_RESOURCE_SOURCE_STORE {
            ActivityWorkClass::OpenResource
        } else {
            ActivityWorkClass::Executing
        }
    }

    pub fn is_executing_work(&self) -> bool {
        self.work_class() == ActivityWorkClass::Executing
    }

    pub fn is_open_resource(&self) -> bool {
        self.work_class() == ActivityWorkClass::OpenResource
    }
}

impl ActivityFilter {
    pub fn matches(&self, item: &ActivityItem) -> bool {
        self.is_empty()
            || item
                .context
                .identities
                .iter()
                .any(|identity| self.matches_identity(identity))
            || self.matches_identity(&ActivityTaskIdentity {
                task_url: item.context.task_url.clone(),
                repository: item.context.repository.clone(),
                worktree: item.context.worktree.clone().or_else(|| item.cwd.clone()),
            })
    }

    pub fn is_empty(&self) -> bool {
        self.task_url.is_none() && self.repository.is_none() && self.worktree.is_none()
    }

    fn matches_identity(&self, identity: &ActivityTaskIdentity) -> bool {
        self.task_url
            .as_deref()
            .is_none_or(|value| identity.task_url.as_deref() == Some(value))
            && self
                .repository
                .as_deref()
                .is_none_or(|value| identity.repository.as_deref() == Some(value))
            && self
                .worktree
                .as_deref()
                .is_none_or(|value| identity.worktree.as_deref() == Some(value))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCounts {
    /// Every record in this report across both work classes: executing work
    /// plus open resources.
    pub total: usize,
    /// Executing work (jobs, controllers, agent tasks, providers) currently
    /// queued or running. Open resources are excluded: an open worktree is
    /// held inventory, not executing work, and must not inflate execution
    /// liveness (#13620).
    pub active: usize,
    /// Executing work waiting to start. Open resources are excluded.
    pub queued: usize,
    /// Executing work currently running. Open resources are excluded.
    pub running: usize,
    /// Executing work that succeeded. Terminal worktree dispositions are
    /// resource history, not execution outcomes.
    pub succeeded: usize,
    /// Stopped with a promotable candidate. Counted separately from
    /// `partial_failure` since #6761 — before that both, plus
    /// `partial_recoverable`, were summed into `partial_failure`, so an
    /// operator could not see how many runs were waiting on a promotion.
    pub candidate_recoverable: usize,
    /// Stopped with resumable partial work. See `candidate_recoverable`.
    pub partial_recoverable: usize,
    pub partial_failure: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub timed_out: usize,
    /// Executing work whose claimed progress is no longer verifiable. A
    /// missing worktree is a degraded *resource* and counts under
    /// `open_resources`, not here.
    pub stale: usize,
    pub unknown: usize,
    /// Open resource inventory held for work: worktrees without a terminal
    /// disposition, including degraded-but-held ones. Presence of inventory
    /// never implies execution (#13620).
    pub open_resources: usize,
}

/// Records deliberately omitted from the default coordination view.
///
/// `activity list --all` retains every collected item and action. The default
/// view instead keeps current work readable when stale projections accumulate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityTruncation {
    pub items_omitted: usize,
    pub stale_items_omitted: usize,
    pub next_actions_omitted: usize,
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
    /// Machine-readable maintenance precondition: `true` only when this
    /// report shows no queued or running executing work **and** is not
    /// `partial` (a connected runner that did not answer could be holding
    /// executing work this report cannot see). Open resources do not affect
    /// it: an operator may hold worktrees open while zero work executes.
    /// Assert on a `list` report — a `show` report's counts describe only the
    /// resolved item (#13620).
    #[serde(default)]
    pub zero_executing_work: bool,
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
    /// Explicit accounting for default-view compaction. Zero values mean this
    /// report retained every collected record and next action.
    #[serde(default)]
    pub truncation: ActivityTruncation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}

impl ActivityReport {
    /// Recompute [`ActivityReport::zero_executing_work`] from the current
    /// counts and partiality. Report assembly calls this once the final
    /// `partial` value is known, because a partial report cannot prove the
    /// absence of runner-resident executing work.
    pub(crate) fn sync_zero_executing_work(&mut self) {
        self.zero_executing_work = self.counts.active == 0 && !self.partial;
    }
}
