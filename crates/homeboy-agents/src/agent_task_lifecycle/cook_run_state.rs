//! The one Cook run state and the actions it makes legal (#18436 slices 1-2).
//!
//! Owns execution-owner observations and their shared liveness decision.
//! Discovery, stale annotation and action eligibility project this decision;
//! provider process candidates never substitute for an execution owner.
//! A Cook run's state used to be re-derived by every verb from raw record
//! fields: `status` read the lifecycle state, `recover` read a progress
//! label, `regate` read the same label again, and action eligibility kept its
//! own per-action state tests. When the label and the state disagreed (a
//! terminal `candidate_recoverable` record whose last Cook exit reported
//! `in_flight`), `status` named `recover` and `recover` refused the run as
//! still in flight (#17976, #17384, #17489, #17295).
//!
//! [`CookRunState::of`] is now the single classifier over the authoritative
//! lifecycle record, and [`legal_actions`] is the single eligibility table.
//! Status, recover, regate and the action-eligibility report read both instead
//! of classifying on their own. [`CookRunPhase::transition`] is the phase
//! transition table the record's writers are held to.
//!
//! The execution phase and replay ledger are persisted internally. The fenced
//! store is the only write boundary. Evidence refinements, fences and liveness
//! are classified on top, never cached as durable verdicts. Legacy records seed
//! their execution phase from the record; remove that fallback once all retained
//! records have been rewritten.

use std::collections::BTreeSet;

use homeboy_core::cook_status::CookStatus;
use homeboy_lifecycle_contract::RunExecutionState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AgentTaskRunRecord;
use crate::agent_task_service::AgentTaskLiveness;
use homeboy_core::api_jobs::{JobStatus, JobStore};
use uuid::Uuid;

use crate::agent_task_service::CookContinuationState as Continuation;
/// Read-only continuation facts shared by settlement and recovery admission.
/// Never persisted: the continuation protocol remains the owner of these facts.
pub(crate) struct CookSettlement(pub(crate) Continuation);

impl CookSettlement {
    pub(crate) fn pending(&self) -> bool {
        self.0 == Continuation::Pending
    }
    pub(crate) fn running(&self) -> bool {
        self.0 == Continuation::Claimed
    }
    pub(crate) fn failed(&self) -> bool {
        self.0 == Continuation::Failed
    }
    pub(crate) fn completed(&self) -> bool {
        self.0 == Continuation::Completed
    }

    pub(crate) fn is_settled(&self, record: &AgentTaskRunRecord, driver_held: bool) -> bool {
        record.state().is_terminal()
            && !driver_held
            && !self.pending()
            && !self.running()
            && !(self.0 == Continuation::Absent
                && record.runner_id().is_some()
                && matches!(
                    record.state(),
                    RunExecutionState::Succeeded | RunExecutionState::CandidateRecoverable
                ))
    }
}

pub(crate) fn cook_settlement_in_store(
    store: &crate::agent_task_service::CookRecipeStore,
    cook_id: &str,
    run_id: &str,
) -> homeboy_core::Result<CookSettlement> {
    crate::agent_task_service::continuation_state_in_store(store, cook_id, run_id)
        .map(CookSettlement)
}

pub(crate) fn continuation_child_owner(record: &AgentTaskRunRecord) -> Option<u32> {
    let child_provider_owner_pid = record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|execution| execution["state"] == "running")
        .find_map(|execution| {
            execution["owner_pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|pid| {
                    homeboy_core::process::process_identity_state(
                        *pid,
                        execution["owner_linux_starttime_ticks"].as_u64(),
                    ) == homeboy_core::process::ProcessIdentityState::Live
                })
        });
    let child_gate_owner_pid = (record.metadata["promotion_progress"]["active"] == true)
        .then(|| {
            record.metadata["promotion_progress"]["owner_pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
        })
        .flatten()
        .filter(|pid| homeboy_core::process::pid_is_running(*pid));
    child_provider_owner_pid.or(child_gate_owner_pid)
}

pub(crate) fn continuation_owner_expired(value: &Value) -> bool {
    value["owner_pid"].as_u64().is_some_and(|pid| {
        u32::try_from(pid).map_or(true, |pid| !homeboy_core::process::pid_is_running(pid))
    })
}

pub(crate) fn has_live_execution_step_owner(record: &AgentTaskRunRecord) -> bool {
    let scheduling = &record.metadata["execution_scheduling"];
    if !scheduling["current_intent_id"].is_string()
        || record.lifecycle.execution.state == RunExecutionState::Cancelled
    {
        return false;
    }
    if record.lifecycle.execution.state == RunExecutionState::Queued {
        return true;
    }
    let owner = &record.metadata["execution_step_owner"];
    if owner["intent_id"] != scheduling["current_intent_id"] || !owner["job_id"].is_string() {
        return false;
    }
    let Some(pid) = owner["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        return true;
    };
    let Ok(identity) = serde_json::from_value::<homeboy_core::process::ProcessStartIdentity>(
        owner["process_start_identity"].clone(),
    ) else {
        return true;
    };
    matches!(
        homeboy_core::process::process_identity_state_with_start_identity(
            pid,
            None,
            Some(&identity)
        ),
        homeboy_core::process::ProcessIdentityState::Live
            | homeboy_core::process::ProcessIdentityState::Unverifiable
    )
}

pub(crate) fn local_owner_liveness(record: &AgentTaskRunRecord) -> super::LocalOwnerLiveness {
    use super::LocalOwnerLiveness;
    let Some(pid) = record
        .metadata
        .get("runner_pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        return LocalOwnerLiveness::Absent;
    };
    let start_identity = record
        .metadata
        .get("runner_process_start_identity")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    match homeboy_core::process::process_identity_state_with_start_identity(
        pid,
        None,
        start_identity.as_ref(),
    ) {
        homeboy_core::process::ProcessIdentityState::Live => LocalOwnerLiveness::Live,
        homeboy_core::process::ProcessIdentityState::Dead
        | homeboy_core::process::ProcessIdentityState::IdentityMismatch => LocalOwnerLiveness::Dead,
        homeboy_core::process::ProcessIdentityState::Unverifiable => {
            LocalOwnerLiveness::Unverifiable
        }
    }
}

pub(crate) fn should_annotate_stale(record: &AgentTaskRunRecord) -> bool {
    if record.runner_job_id().is_some() && record.has_fresh_update() {
        return false;
    }
    classify_liveness(record, chrono::Utc::now(), &BTreeSet::new()).is_reconcilable()
}

pub(crate) fn classify_liveness(
    record: &AgentTaskRunRecord,
    now: chrono::DateTime<chrono::Utc>,
    live: &BTreeSet<Uuid>,
) -> AgentTaskLiveness {
    if record.owner_process_is_running()
        || crate::agent_task_service::live_cook_continuation(record).is_some()
        || record.has_live_execution_step_owner()
        || live_local_cook_retry_supervisor(record, live)
        || live_local_cook_retry_child(record)
    {
        return AgentTaskLiveness::Active;
    }
    if crate::agent_task_lifecycle::detached_cook_admission_is_live(record, now) {
        return AgentTaskLiveness::Active;
    }
    if record.lab_handoff_validation_error().is_some() {
        return AgentTaskLiveness::Unreconciled;
    }
    if durable_queued_retry_is_live(record) {
        return AgentTaskLiveness::Active;
    }
    if record.has_fresh_controller_pre_provider_heartbeat() {
        return AgentTaskLiveness::Active;
    }
    if crate::agent_task_lifecycle::has_expired_pending_runner_submission_intent(record, now) {
        return AgentTaskLiveness::Unreconciled;
    }
    if record.state() != homeboy_lifecycle_contract::RunExecutionState::Running {
        return if record.has_planned_runner_execution() && record.has_fresh_update() {
            AgentTaskLiveness::Active
        } else {
            AgentTaskLiveness::Stale
        };
    }

    if record.has_planned_runner_execution() && record.has_fresh_update() {
        return AgentTaskLiveness::Active;
    }

    match record.local_owner_liveness() {
        crate::agent_task_lifecycle::LocalOwnerLiveness::Live => return AgentTaskLiveness::Active,
        crate::agent_task_lifecycle::LocalOwnerLiveness::Unverifiable => {
            return AgentTaskLiveness::Unreconciled;
        }
        crate::agent_task_lifecycle::LocalOwnerLiveness::Dead => return AgentTaskLiveness::Stale,
        crate::agent_task_lifecycle::LocalOwnerLiveness::Absent => {}
    }

    if record.is_stale_running() {
        return AgentTaskLiveness::Stale;
    }

    let stale_by_age = record
        .updated_at
        .as_deref()
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .is_some_and(|updated| {
            now.signed_duration_since(updated).num_minutes()
                >= homeboy_core::observation::RUNNING_HEARTBEAT_STALE_MINUTES
        });

    let has_owner_signal =
        record.metadata.get("runner_pid").is_some() || record.runner_job_id().is_some();

    match (stale_by_age, has_owner_signal) {
        (true, true) => AgentTaskLiveness::Suspect,
        (true, false) => AgentTaskLiveness::Stale,
        (false, true) => AgentTaskLiveness::Active,
        (false, false) => AgentTaskLiveness::Stale,
    }
}

fn durable_queued_retry_is_live(record: &AgentTaskRunRecord) -> bool {
    record.state() == homeboy_lifecycle_contract::RunExecutionState::Queued
        && record.metadata["retry_of"]
            .as_str()
            .is_some_and(|run_id| !run_id.is_empty())
}

pub(crate) fn live_supervisor_jobs() -> BTreeSet<Uuid> {
    live_supervisor_jobs_at(
        homeboy_core::daemon::read_status(),
        homeboy_core::paths::observation_db(),
    )
}

pub(crate) fn live_supervisor_jobs_at(
    status: homeboy_core::error::Result<homeboy_core::daemon::DaemonStatus>,
    path: homeboy_core::error::Result<std::path::PathBuf>,
) -> BTreeSet<Uuid> {
    let lease = status
        .ok()
        .filter(|daemon| daemon.reachable && daemon.fresh)
        .and_then(|daemon| daemon.state.map(|state| state.lease_id));
    let store = path
        .ok()
        .and_then(|path| JobStore::open_without_reconciliation(path).ok());
    let operation = ["controller.", homeboy_core::daemon::step_job::STEP_JOB_TYPE].concat();
    lease
        .zip(store)
        .map_or_else(BTreeSet::new, |(lease, store)| {
            store
                .list()
                .into_iter()
                .filter(|job| {
                    job.operation == operation
                        && job.daemon_lease_id.as_deref() == Some(lease.as_str())
                        && matches!(job.status, JobStatus::Queued | JobStatus::Running)
                })
                .map(|job| job.id)
                .collect()
        })
}

fn live_local_cook_retry_supervisor(record: &AgentTaskRunRecord, live: &BTreeSet<Uuid>) -> bool {
    let supervisor = &record.metadata["local_cook_supervisor"];
    if supervisor["job_type"].as_str() != Some(homeboy_core::daemon::step_job::STEP_JOB_TYPE) {
        return false;
    }
    let Some(job_id) = supervisor["job_id"].as_str() else {
        return false;
    };
    let Ok(job_id) = Uuid::parse_str(job_id) else {
        return false;
    };
    live.contains(&job_id)
}

fn live_local_cook_retry_child(record: &AgentTaskRunRecord) -> bool {
    let supervisor = &record.metadata["local_cook_supervisor"];
    if supervisor["state"].as_str() != Some("supervising")
        || supervisor["pinned_run_id"].as_str() != Some(record.run_id.as_str())
    {
        return false;
    }
    let Some(pid) = supervisor["child_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        return false;
    };
    let Ok(start_identity) = serde_json::from_value::<homeboy_core::process::ProcessStartIdentity>(
        supervisor["child_start_identity"].clone(),
    ) else {
        return false;
    };
    matches!(
        homeboy_core::process::process_identity_state_with_start_identity(
            pid,
            None,
            Some(&start_identity),
        ),
        homeboy_core::process::ProcessIdentityState::Live
    )
}

/// Where one Cook run is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookRunPhase {
    /// Admitted; no execution owner has started it.
    Queued,
    /// An execution owner (local, runner or transport proxy) carries it.
    Running,
    /// The attempt's execution ended, and a live continuation owner is driving
    /// its remaining phases (harvest, gates, promotion, finalization).
    Continuing,
    /// Execution ended and an unpublished candidate is retained.
    CandidateRecoverable,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
    /// A state written by a newer binary; it carries no verdict.
    Unknown,
}

/// A fact reported to the run's lifecycle. Each moves the phase through
/// [`CookRunPhase::transition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookRunEvent {
    /// An execution owner started (or a recovery re-admitted) the run.
    Started,
    /// Execution ended holding a candidate that has not been published.
    CandidateRetained,
    /// A continuation owner claimed the remaining phases.
    ContinuationClaimed,
    /// The continuation owner released (completed, failed or died).
    ContinuationReleased,
    /// The candidate was verified or published.
    Succeeded,
    /// Execution ended with part of the work refused or incomplete.
    PartiallyFailed,
    Failed,
    Cancelled,
    /// Return a stopped or unclaimed attempt to admission.
    Requeued,
    /// Later durable aggregate, harvest, regate or promotion evidence settles
    /// an outcome. Unlike a new execution, this can replace a prior verdict.
    OutcomeRecorded {
        phase: CookRunPhase,
    },
    /// Refresh fence, status and ownership facts without moving phase.
    FactsUpdated,
}

impl CookRunPhase {
    pub const ALL: [CookRunPhase; 9] = [
        Self::Queued,
        Self::Running,
        Self::Continuing,
        Self::CandidateRecoverable,
        Self::Succeeded,
        Self::PartialFailure,
        Self::Failed,
        Self::Cancelled,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Continuing => "continuing",
            Self::CandidateRecoverable => "candidate_recoverable",
            Self::Succeeded => "succeeded",
            Self::PartialFailure => "partial_failure",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// Whether an owner is still carrying the run.
    pub fn is_in_flight(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Continuing)
    }

    /// Compatibility classification for recipe-only evidence without a run
    /// record. Durable records always take precedence over status labels.
    pub(crate) fn status_is_recoverable(status: &CookStatus) -> bool {
        matches!(status, CookStatus::CandidateRecoverable)
    }

    pub(crate) fn of_execution(state: RunExecutionState) -> Self {
        match state {
            RunExecutionState::Queued => Self::Queued,
            RunExecutionState::Running => Self::Running,
            RunExecutionState::CandidateRecoverable => Self::CandidateRecoverable,
            RunExecutionState::Succeeded => Self::Succeeded,
            RunExecutionState::PartialFailure => Self::PartialFailure,
            RunExecutionState::Failed => Self::Failed,
            RunExecutionState::Cancelled => Self::Cancelled,
            RunExecutionState::Unknown => Self::Unknown,
        }
    }

    /// The phase `event` moves this phase to, or `None` when the transition is
    /// illegal. Transitions are idempotent: replaying the event that produced
    /// a phase leaves it unchanged, so a retried write converges.
    pub fn transition(self, event: CookRunEvent) -> Option<Self> {
        use CookRunEvent as E;
        use CookRunPhase as P;
        // Replays of the event that produced the current phase.
        let replay = matches!(
            (self, event),
            (P::Running, E::Started)
                | (P::CandidateRecoverable, E::CandidateRetained)
                | (P::Continuing, E::ContinuationClaimed)
                | (P::Succeeded, E::Succeeded)
                | (P::PartialFailure, E::PartiallyFailed)
                | (P::Failed, E::Failed)
                | (P::Cancelled, E::Cancelled)
        );
        if replay {
            return Some(self);
        }
        match (self, event) {
            (_, E::FactsUpdated) => Some(self),
            (P::Unknown, E::Requeued) => None,
            (_, E::Requeued) => Some(P::Queued),
            (from, E::OutcomeRecorded { phase }) => {
                let execution = |phase| match phase {
                    P::Queued => RunExecutionState::Queued,
                    P::Running => RunExecutionState::Running,
                    P::Continuing | P::CandidateRecoverable => {
                        RunExecutionState::CandidateRecoverable
                    }
                    P::Succeeded => RunExecutionState::Succeeded,
                    P::PartialFailure => RunExecutionState::PartialFailure,
                    P::Failed => RunExecutionState::Failed,
                    P::Cancelled => RunExecutionState::Cancelled,
                    P::Unknown => RunExecutionState::Unknown,
                };
                (phase != P::Continuing && execution(from).can_transition_to(execution(phase)))
                    .then_some(phase)
            }
            // A newer binary's state carries no verdict to transition from.
            (P::Unknown, _) => None,
            // A cancelled attempt's Cook may still hand its remaining phases
            // (successor admission, finalization) to a continuation owner;
            // the attempt's own cancellation outcome is otherwise final.
            (P::Cancelled, E::ContinuationClaimed) => Some(P::Continuing),
            (P::Cancelled, E::ContinuationReleased) => Some(P::Cancelled),
            // Cancellation is final, and any live phase can be cancelled.
            (P::Cancelled, _) => None,
            (_, E::Cancelled) => Some(P::Cancelled),
            (P::Queued, E::Started) => Some(P::Running),
            (P::Queued | P::Running, E::Failed) => Some(P::Failed),
            (P::Running, E::CandidateRetained) => Some(P::CandidateRecoverable),
            (P::Running, E::Succeeded) => Some(P::Succeeded),
            (P::Running, E::PartiallyFailed) => Some(P::PartialFailure),
            // Recovery re-admits a stopped attempt's execution.
            (P::CandidateRecoverable | P::PartialFailure | P::Failed, E::Started) => {
                Some(P::Running)
            }
            // Only a terminal attempt hands its remaining phases to a
            // continuation owner; the owner reports the outcome it reached.
            (
                P::CandidateRecoverable | P::Succeeded | P::PartialFailure | P::Failed,
                E::ContinuationClaimed,
            ) => Some(P::Continuing),
            // Releasing an already-released continuation changes nothing.
            (
                P::CandidateRecoverable | P::Succeeded | P::PartialFailure | P::Failed,
                E::ContinuationReleased,
            ) => Some(self),
            (P::Continuing, E::ContinuationReleased | E::CandidateRetained) => {
                Some(P::CandidateRecoverable)
            }
            (P::Continuing, E::Succeeded) => Some(P::Succeeded),
            (P::Continuing, E::PartiallyFailed) => Some(P::PartialFailure),
            (P::Continuing, E::Failed) => Some(P::Failed),
            // Regate and replacement proof settle a retained candidate.
            (P::CandidateRecoverable, E::Succeeded) => Some(P::Succeeded),
            (P::CandidateRecoverable, E::Failed) => Some(P::Failed),
            (P::CandidateRecoverable, E::PartiallyFailed) => Some(P::PartialFailure),
            _ => None,
        }
    }
}

/// A Cook verb whose legality the state decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookRunAction {
    /// `agent-task recover`: plan and perform the one next recovery step.
    Recover,
    /// `agent-task recover --regate`: re-verify a retained candidate.
    Regate,
    /// `agent-task cancel`.
    Cancel,
    /// Re-enter the Cook continuation from its durable recipe.
    Continue,
    /// Promote a retained candidate into a target worktree.
    Promote,
}

impl CookRunAction {
    pub const ALL: [CookRunAction; 5] = [
        Self::Recover,
        Self::Regate,
        Self::Cancel,
        Self::Continue,
        Self::Promote,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recover => "recover",
            Self::Regate => "regate",
            Self::Cancel => "cancel",
            Self::Continue => "continue",
            Self::Promote => "promote",
        }
    }
}

/// The one classification of a Cook run, read by every surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookRunState {
    #[serde(skip)]
    pub(crate) liveness: AgentTaskLiveness,
    /// Internal replay ledger; never part of public state JSON.
    #[serde(skip_serializing)]
    applied_events: Vec<(String, CookRunEvent)>,
    pub phase: CookRunPhase,
    /// The record's execution ended. A [`CookRunPhase::Continuing`] run is
    /// terminal but still in flight: its continuation owner carries it.
    pub terminal: bool,
    /// A fresh successor attempt may replace this one.
    pub retryable: bool,
    /// An unpublished candidate is retained for recovery.
    pub recoverable: bool,
    /// The Cook mission was fenced or its candidate retired.
    pub fenced: bool,
    /// The recorded controller process is still alive, even after execution
    /// ended, so the run can still be cancelled.
    pub live_owner: bool,
    /// The Cook status label this state reports. A label that reads as in
    /// flight never survives on a terminal record: the record's execution
    /// state outranks a stale label from an earlier exit.
    pub status: String,
}

impl CookRunState {
    /// Classify `record`. This is the only place a Cook run's phase,
    /// terminality and recoverability are decided.
    pub fn of(record: &AgentTaskRunRecord) -> Self {
        let execution_phase = record
            .persisted_cook_state()
            .map(|state| state.phase)
            .unwrap_or_else(|| CookRunPhase::of_execution(record.state()));
        Self::classify(record, execution_phase)
    }

    /// Refine the transition-owned execution phase with current evidence. This
    /// is shared by legacy and persisted records: delivery, fences and live
    /// owners are observations, not durable phase-transition facts.
    fn classify(record: &AgentTaskRunRecord, execution_phase: CookRunPhase) -> Self {
        let liveness = classify_liveness(record, chrono::Utc::now(), &BTreeSet::new());
        let terminal = !matches!(
            execution_phase,
            CookRunPhase::Queued | CookRunPhase::Running
        );
        let phase =
            if terminal && crate::agent_task_service::live_cook_continuation(record).is_some() {
                CookRunPhase::Continuing
            } else {
                record.observed_cook_phase.unwrap_or(execution_phase)
            };
        let fenced = record.metadata.get("cancellation_fence").is_some()
            || record.metadata.get("candidate_retirement").is_some();
        let status = match phase {
            CookRunPhase::Queued => "queued".to_string(),
            CookRunPhase::Running => "running".to_string(),
            CookRunPhase::Continuing => "in_flight".to_string(),
            _ => terminal_status_label(record, phase),
        };
        Self {
            liveness,
            applied_events: record
                .persisted_cook_state()
                .map(|state| state.applied_events.clone())
                .unwrap_or_default(),
            phase,
            terminal,
            retryable: matches!(
                phase,
                CookRunPhase::Failed | CookRunPhase::Cancelled | CookRunPhase::PartialFailure
            ),
            recoverable: phase == CookRunPhase::CandidateRecoverable && !fenced,
            fenced,
            live_owner: record.owner_process_is_running(),
            status,
        }
    }

    /// Apply a keyed event. An exact replay is a no-op even after later events;
    /// reusing a key for a different event or taking an illegal edge is refused.
    pub fn apply(&self, event: CookRunEvent, key: &str) -> Result<Self, CookRunTransitionError> {
        if let Some((_, previous)) = self.applied_events.iter().find(|(seen, _)| seen == key) {
            return if *previous == event {
                Ok(self.clone())
            } else {
                Err(CookRunTransitionError::ReplayKeyConflict {
                    key: key.to_owned(),
                })
            };
        }
        let phase =
            self.phase
                .transition(event)
                .ok_or(CookRunTransitionError::IllegalTransition {
                    from: self.phase,
                    event,
                })?;
        let mut next = self.clone();
        next.phase = phase;
        next.terminal = !matches!(phase, CookRunPhase::Queued | CookRunPhase::Running);
        next.retryable = matches!(
            phase,
            CookRunPhase::Failed | CookRunPhase::Cancelled | CookRunPhase::PartialFailure
        );
        next.recoverable = phase == CookRunPhase::CandidateRecoverable && !next.fenced;
        next.status = if phase == CookRunPhase::Continuing {
            "in_flight"
        } else {
            phase.as_str()
        }
        .to_owned();
        next.applied_events.push((key.to_owned(), event));
        Ok(next)
    }

    /// The same state seen through the Cook index's mission fence.
    pub fn with_cook_index_fence(mut self, fenced: bool) -> Self {
        if fenced {
            self.fenced = true;
            self.recoverable = false;
        }
        self
    }

    pub fn allows(&self, action: CookRunAction) -> bool {
        legal_actions(self).contains(&action)
    }
}

/// A refused state write. Neither variant mutates the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookRunTransitionError {
    IllegalTransition {
        from: CookRunPhase,
        event: CookRunEvent,
    },
    ReplayKeyConflict {
        key: String,
    },
}

/// Only transition-owned facts are durable. In particular, status, liveness,
/// fencing and delivery verification must never be cached here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedCookState {
    phase: CookRunPhase,
    #[serde(default)]
    applied_events: Vec<(String, CookRunEvent)>,
}

/// Attach an evidence-only refinement to a read view, without touching the
/// durable phase or replay receipts. Only this module owns Cook phase writes.
pub(crate) fn observe_phase(record: &mut AgentTaskRunRecord, state: RunExecutionState) {
    record.observed_cook_phase = Some(CookRunPhase::of_execution(state));
}

#[derive(Clone, Copy)]
pub(crate) struct CookStateWriteAuthority(());

/// Adapt existing lifecycle writers at their common fenced commit boundary.
/// Use the stored state, never a caller's stale projection, as the source.
/// The next record revision is a stable replay identity within this run.
pub(crate) fn transition_record(
    record: &mut AgentTaskRunRecord,
    stored: Option<&AgentTaskRunRecord>,
) -> homeboy_core::Result<()> {
    // A caller may commit a previously observed view. Its refinement is not
    // part of the durable state and must not influence transition selection.
    record.observed_cook_phase = None;
    let phase = CookRunPhase::of_execution(record.state());
    if let Some(state) = stored.and_then(AgentTaskRunRecord::persisted_cook_state) {
        if state.phase == phase {
            record.write_cook_state(state.clone(), CookStateWriteAuthority(()));
            return Ok(());
        }
    }
    let facts = CookRunState::classify(record, phase);
    let mut source = stored
        .map(CookRunState::of)
        .unwrap_or_else(|| facts.clone());
    // The transition source is the durable execution phase, not its current
    // evidence refinement (e.g. a live continuation).
    source.phase = stored
        .and_then(AgentTaskRunRecord::persisted_cook_state)
        .map(|state| state.phase)
        .or_else(|| stored.map(|record| CookRunPhase::of_execution(record.state())))
        .unwrap_or(phase);
    let event = if source.phase == phase {
        CookRunEvent::FactsUpdated
    } else {
        match phase {
            CookRunPhase::Queued => CookRunEvent::Requeued,
            CookRunPhase::Running if source.phase != CookRunPhase::Cancelled => {
                CookRunEvent::Started
            }
            CookRunPhase::Continuing => CookRunEvent::ContinuationClaimed,
            phase => CookRunEvent::OutcomeRecorded { phase },
        }
    };
    let next = source
        .apply(event, &format!("record:{}", record.revision))
        .map_err(|error| {
            homeboy_core::Error::validation_invalid_argument(
                "cook_run_state",
                format!("Cook transition refused: {error:?}"),
                Some(record.run_id.clone()),
                None,
            )
        })?;
    record.write_cook_state(
        PersistedCookState {
            phase: next.phase,
            applied_events: next.applied_events,
        },
        CookStateWriteAuthority(()),
    );
    Ok(())
}

/// Internal store encoding; public record serialization deliberately omits it.
pub(crate) fn persist_state(
    record: &AgentTaskRunRecord,
    value: &mut Value,
) -> homeboy_core::Result<()> {
    if let Some(state) = record.persisted_cook_state() {
        let encoded = serde_json::to_value(state)
            .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))?;
        value["cook_run_state"] = encoded;
    }
    Ok(())
}

/// The label of a terminal attempt: delivery's typed read model first, then
/// the attempt's own failure and terminal evidence, then its execution state.
/// Labels that read as in flight are stale on a terminal record and ignored.
fn terminal_status_label(record: &AgentTaskRunRecord, phase: CookRunPhase) -> String {
    let settled = |status: CookStatus| (!status.is_in_flight()).then_some(status);
    let delivery = crate::agent_task_service::CookDelivery::from_run_metadata(&record.metadata)
        .and_then(|delivery| delivery.lifecycle().cook_status)
        .and_then(settled);
    if let Some(status) = delivery {
        return status.as_str().to_string();
    }
    if record
        .metadata
        .get("pre_execution_failure")
        .is_some_and(|failure| !failure.is_null())
    {
        return CookStatus::PreExecutionFailure.as_str().to_string();
    }
    if let Some(status) = record
        .metadata
        .pointer("/cook_progress/terminal_status")
        .and_then(Value::as_str)
        .map(CookStatus::from_status)
        .and_then(settled)
    {
        return status.as_str().to_string();
    }
    match phase {
        CookRunPhase::PartialFailure => "failed",
        phase => phase.as_str(),
    }
    .to_string()
}

/// Every action `state` makes legal. Verbs refuse exactly what is absent here,
/// so `status` can never advertise a verb that refuses the same run.
pub fn legal_actions(state: &CookRunState) -> BTreeSet<CookRunAction> {
    use CookRunAction as A;
    use CookRunPhase as P;
    let mut actions = BTreeSet::new();
    if !state.terminal || state.live_owner {
        actions.insert(A::Cancel);
    }
    if state.fenced || state.live_owner || (!state.terminal && !state.liveness.is_reconcilable()) {
        return actions;
    }
    match state.phase {
        // Admission or a live continuation owns every transition.
        P::Queued | P::Continuing => {}
        // Recovery first reconciles a running record whose owner is gone.
        P::Running => {
            actions.extend([A::Recover, A::Continue]);
        }
        P::CandidateRecoverable | P::Succeeded => {
            actions.extend([A::Recover, A::Regate, A::Continue, A::Promote]);
        }
        P::PartialFailure | P::Failed | P::Cancelled | P::Unknown => {
            actions.extend([A::Recover, A::Regate, A::Continue]);
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use CookRunAction as A;
    use CookRunPhase as P;

    fn record(state: RunExecutionState, metadata: Value) -> AgentTaskRunRecord {
        let mut record: AgentTaskRunRecord = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-run/v1", "run_id": "run", "plan_id": "plan",
            "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
        }))
        .expect("record");
        record.metadata = metadata;
        record
    }

    fn state(phase: CookRunPhase, fenced: bool, live_owner: bool) -> CookRunState {
        CookRunState {
            liveness: AgentTaskLiveness::Stale,
            applied_events: Vec::new(),
            phase,
            terminal: !matches!(phase, P::Queued | P::Running),
            retryable: false,
            recoverable: phase == P::CandidateRecoverable,
            fenced,
            live_owner,
            status: phase.as_str().to_string(),
        }
    }

    #[test]
    fn provider_never_launched_and_controller_restart_are_reconcilable() {
        for metadata in [
            serde_json::json!({}),
            serde_json::json!({"runner_pid": 99_999_999}),
        ] {
            let run = record(RunExecutionState::Running, metadata);
            let state = CookRunState::of(&run);
            assert_eq!(state.liveness, AgentTaskLiveness::Stale);
            assert!(state.allows(A::Cancel));
            assert!(state.allows(A::Recover));
            assert!(state.allows(A::Continue));
        }
    }

    #[test]
    fn live_owner_outranks_expired_heartbeat_and_stale_projection() {
        let run = record(
            RunExecutionState::Running,
            serde_json::json!({
                "runner_pid": std::process::id(), "stale_running": true,
            }),
        );
        let state = CookRunState::of(&run);
        assert_eq!(state.liveness, AgentTaskLiveness::Active);
        assert!(state.allows(A::Cancel));
        assert!(!state.allows(A::Recover));
        assert!(!state.allows(A::Continue));
    }

    #[test]
    fn discovery_and_record_annotation_delegate_liveness() {
        let discovery = include_str!("../agent_task_service/discovery.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!discovery.contains("fn classify_liveness("));
        assert!(!discovery.contains("process_identity_state_with_start_identity("));
        let records = include_str!("records.rs");
        assert!(!records.contains("process_identity_state_with_start_identity(\n            pid,"));
    }

    /// The exhaustive eligibility table: every phase, with and without a fence
    /// and a live owner, against every action.
    #[test]
    fn keyed_events_replay_after_later_events_and_refuse_illegal_edges() {
        let initial = state(P::Queued, false, false);
        let running = initial.apply(CookRunEvent::Started, "start-1").unwrap();
        assert_eq!(
            running.apply(CookRunEvent::Started, "start-1").unwrap(),
            running
        );
        let candidate = running
            .apply(CookRunEvent::CandidateRetained, "harvest-1")
            .unwrap();
        assert_eq!(
            candidate.apply(CookRunEvent::Started, "start-1").unwrap(),
            candidate
        );
        assert!(matches!(
            candidate.apply(CookRunEvent::Failed, "start-1"),
            Err(CookRunTransitionError::ReplayKeyConflict { .. })
        ));
        assert!(matches!(
            initial.apply(CookRunEvent::Succeeded, "bad"),
            Err(CookRunTransitionError::IllegalTransition { .. })
        ));
        assert_eq!(initial.phase, P::Queued);
    }

    #[test]
    fn persisted_state_wins_over_legacy_derivation_and_is_not_public_json() {
        let mut legacy = record(RunExecutionState::Running, serde_json::json!({}));
        assert!(legacy.persisted_cook_state().is_none());
        assert_eq!(CookRunState::of(&legacy).phase, P::Running);
        legacy.revision = 1;
        transition_record(&mut legacy, None).unwrap();
        let mut internal = serde_json::to_value(&legacy).unwrap();
        assert!(internal.get("cook_run_state").is_none());
        persist_state(&legacy, &mut internal).unwrap();
        let mut restored: AgentTaskRunRecord = serde_json::from_value(internal).unwrap();
        assert_eq!(CookRunState::of(&restored), CookRunState::of(&legacy));
        let replayed = CookRunState::of(&restored)
            .apply(CookRunEvent::FactsUpdated, "record:1")
            .unwrap();
        assert_eq!(replayed, CookRunState::of(&restored));
        restored.set_state_for_test(RunExecutionState::Failed);
        assert_eq!(CookRunState::of(&restored).phase, P::Running);
    }

    #[test]
    fn persisted_phase_and_legacy_records_share_evidence_refinements() {
        let mut legacy = record(RunExecutionState::Succeeded, serde_json::json!({}));
        let mut durable = legacy.clone();
        durable.revision = 1;
        transition_record(&mut durable, None).unwrap();
        let persisted = durable.persisted_cook_state().cloned().unwrap();
        for refinement in [
            RunExecutionState::CandidateRecoverable,
            RunExecutionState::PartialFailure,
            RunExecutionState::Succeeded,
        ] {
            let legacy_view =
                super::super::terminal_transition::observed_with_run_state(&legacy, refinement);
            let durable_view =
                super::super::terminal_transition::observed_with_run_state(&durable, refinement);
            let mut expected = CookRunState::of(&legacy_view);
            expected.applied_events = persisted.applied_events.clone();
            let actual = CookRunState::of(&durable_view);
            assert_eq!(actual, expected);
            assert_eq!(actual.phase, P::of_execution(refinement));
            assert_eq!(durable_view.persisted_cook_state(), Some(&persisted));
            assert!(serde_json::to_value(&durable_view)
                .unwrap()
                .get("observed_cook_phase")
                .is_none());
        }
        // Fence evidence is also fresh without a phase transition.
        legacy.metadata["cancellation_fence"] = serde_json::json!({});
        durable.metadata = legacy.metadata.clone();
        assert!(CookRunState::of(&durable).fenced);
        assert_eq!(durable.persisted_cook_state(), Some(&persisted));
    }

    #[test]
    fn cook_state_write_authority_is_exclusive() {
        fn scan(path: &std::path::Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let name = path.file_name().unwrap().to_str().unwrap();
                if name != "cook_run_state.rs" {
                    assert!(
                        !source.contains("CookStateWriteAuthority(()"),
                        "{}",
                        path.display()
                    );
                    assert!(!source.contains(".write_cook_state("), "{}", path.display());
                }
                if name != "records.rs" && name != "cook_run_state.rs" {
                    assert!(!source.contains(".cook_run_state ="), "{}", path.display());
                    assert!(
                        !source.contains(".observed_cook_phase ="),
                        "{}",
                        path.display()
                    );
                }
            }
        }
        scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    }

    #[test]
    fn legal_actions_table_is_exhaustive() {
        let all = |actions: &[A]| actions.iter().copied().collect::<BTreeSet<_>>();
        for phase in P::ALL {
            for fenced in [false, true] {
                for live_owner in [false, true] {
                    let cancel = matches!(phase, P::Queued | P::Running) || live_owner;
                    let mut expected = match (fenced, phase) {
                        (_, _) if fenced || live_owner => all(&[]),
                        (true, _) | (false, P::Queued | P::Continuing) => all(&[]),
                        (false, P::Running) => all(&[A::Recover, A::Continue]),
                        (false, P::CandidateRecoverable | P::Succeeded) => {
                            all(&[A::Recover, A::Regate, A::Continue, A::Promote])
                        }
                        (false, P::PartialFailure | P::Failed | P::Cancelled | P::Unknown) => {
                            all(&[A::Recover, A::Regate, A::Continue])
                        }
                    };
                    if cancel {
                        expected.insert(A::Cancel);
                    }
                    let actual = legal_actions(&state(phase, fenced, live_owner));
                    assert_eq!(
                        actual, expected,
                        "{phase:?} fenced={fenced} live={live_owner}"
                    );
                    for action in A::ALL {
                        assert_eq!(
                            state(phase, fenced, live_owner).allows(action),
                            expected.contains(&action),
                            "{phase:?} {action:?}"
                        );
                    }
                }
            }
        }
    }

    /// The exhaustive transition table. Every (phase, event) pair is listed,
    /// and every legal transition is idempotent under replay.
    #[test]
    fn transition_table_is_exhaustive_and_idempotent() {
        use CookRunEvent as E;
        const EVENTS: [E; 8] = [
            E::Started,
            E::CandidateRetained,
            E::ContinuationClaimed,
            E::ContinuationReleased,
            E::Succeeded,
            E::PartiallyFailed,
            E::Failed,
            E::Cancelled,
        ];
        let table: &[(P, [Option<P>; 8])] = &[
            (
                P::Queued,
                [
                    Some(P::Running),
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(P::Failed),
                    Some(P::Cancelled),
                ],
            ),
            (
                P::Running,
                [
                    Some(P::Running),
                    Some(P::CandidateRecoverable),
                    None,
                    None,
                    Some(P::Succeeded),
                    Some(P::PartialFailure),
                    Some(P::Failed),
                    Some(P::Cancelled),
                ],
            ),
            (
                P::Continuing,
                [
                    None,
                    Some(P::CandidateRecoverable),
                    Some(P::Continuing),
                    Some(P::CandidateRecoverable),
                    Some(P::Succeeded),
                    Some(P::PartialFailure),
                    Some(P::Failed),
                    Some(P::Cancelled),
                ],
            ),
            (
                P::CandidateRecoverable,
                [
                    Some(P::Running),
                    Some(P::CandidateRecoverable),
                    Some(P::Continuing),
                    Some(P::CandidateRecoverable),
                    Some(P::Succeeded),
                    Some(P::PartialFailure),
                    Some(P::Failed),
                    Some(P::Cancelled),
                ],
            ),
            (
                P::Succeeded,
                [
                    None,
                    None,
                    Some(P::Continuing),
                    Some(P::Succeeded),
                    Some(P::Succeeded),
                    None,
                    None,
                    Some(P::Cancelled),
                ],
            ),
            (
                P::PartialFailure,
                [
                    Some(P::Running),
                    None,
                    Some(P::Continuing),
                    Some(P::PartialFailure),
                    None,
                    Some(P::PartialFailure),
                    None,
                    Some(P::Cancelled),
                ],
            ),
            (
                P::Failed,
                [
                    Some(P::Running),
                    None,
                    Some(P::Continuing),
                    Some(P::Failed),
                    None,
                    None,
                    Some(P::Failed),
                    Some(P::Cancelled),
                ],
            ),
            (
                P::Cancelled,
                [
                    None,
                    None,
                    Some(P::Continuing),
                    Some(P::Cancelled),
                    None,
                    None,
                    None,
                    Some(P::Cancelled),
                ],
            ),
            (P::Unknown, [None; 8]),
        ];
        assert_eq!(table.len(), P::ALL.len());
        for (phase, row) in table {
            for (event, expected) in EVENTS.iter().zip(row) {
                let next = phase.transition(*event);
                assert_eq!(next, *expected, "{phase:?} --{event:?}-->");
                if let Some(next) = next {
                    assert_eq!(
                        next.transition(*event),
                        Some(next),
                        "{next:?} replays {event:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn classifier_reads_execution_state_fence_and_continuation_owner() {
        for (execution, phase) in [
            (RunExecutionState::Queued, P::Queued),
            (RunExecutionState::Running, P::Running),
            (
                RunExecutionState::CandidateRecoverable,
                P::CandidateRecoverable,
            ),
            (RunExecutionState::Succeeded, P::Succeeded),
            (RunExecutionState::PartialFailure, P::PartialFailure),
            (RunExecutionState::Failed, P::Failed),
            (RunExecutionState::Cancelled, P::Cancelled),
            (RunExecutionState::Unknown, P::Unknown),
        ] {
            let classified = CookRunState::of(&record(execution, serde_json::json!({})));
            assert_eq!(classified.phase, phase);
            assert_eq!(classified.terminal, execution.is_terminal());
            assert_eq!(classified.recoverable, phase == P::CandidateRecoverable);
            assert!(!classified.fenced);
        }

        let continuing = CookRunState::of(&record(
            RunExecutionState::CandidateRecoverable,
            serde_json::json!({
                "cook_continuation": {
                    "state": "claimed",
                    "owner_pid": std::process::id(),
                },
            }),
        ));
        assert_eq!(continuing.phase, P::Continuing);
        assert_eq!(continuing.status, "in_flight");
        assert!(!continuing.allows(A::Recover));

        let fenced = CookRunState::of(&record(
            RunExecutionState::CandidateRecoverable,
            serde_json::json!({ "cancellation_fence": {} }),
        ));
        assert!(fenced.fenced && !fenced.recoverable);
        assert!(legal_actions(&fenced).is_empty());
        assert!(
            CookRunState::of(&record(RunExecutionState::Failed, serde_json::json!({})))
                .with_cook_index_fence(true)
                .fenced
        );
    }

    #[test]
    fn verbs_do_not_reclassify_candidate_or_in_flight_status() {
        for source in [
            include_str!("action_eligibility.rs"),
            include_str!("../agent_task_service/cook_recovery.rs"),
        ] {
            let production = source.split("#[cfg(test)]").next().expect("source");
            assert!(!production.contains("CookStatus::CandidateRecoverable"));
            assert!(!production.contains(".is_in_flight()"));
        }
    }

    #[test]
    fn a_stale_in_flight_label_never_classifies_a_terminal_record() {
        for label in ["queued", "in_flight", "awaiting_ci"] {
            let classified = CookRunState::of(&record(
                RunExecutionState::CandidateRecoverable,
                serde_json::json!({ "cook_progress": { "phase": "terminal", "terminal_status": label } }),
            ));
            assert_eq!(classified.status, "candidate_recoverable", "{label}");
            assert!(classified.allows(A::Recover), "{label}");
        }
        let settled = CookRunState::of(&record(
            RunExecutionState::Failed,
            serde_json::json!({ "cook_progress": { "terminal_status": "gate_failed" } }),
        ));
        assert_eq!(settled.status, "gate_failed");
    }
}
