//! The one Cook run state and the actions it makes legal (#18436 slices 1-2).
//!
//! Owns execution-owner observations and their shared liveness decision.
//! Discovery, stale annotation and action eligibility project this decision;
//! provider process candidates never substitute for an execution owner.
//! [`CookRunState::of`] is the single classifier over the authoritative
//! lifecycle record, and [`legal_actions`] is the single eligibility table.
//! Status, recover, regate and the action-eligibility report read both rather
//! than classifying from raw record fields, so they cannot disagree about a
//! run (#17976, #17384, #17489, #17295). [`CookRunPhase::transition`] is the phase
//! transition table the record's writers are held to.
//!
//! The record's `RunExecutionState` is the only owner of the execution phase;
//! the fenced store refuses commits whose phase change the table forbids.
//! Evidence refinements, fences and liveness are classified on top, never
//! cached as durable verdicts.

use std::collections::BTreeSet;

use homeboy_core::cook_status::{CookStatus, GateVerdict};
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

/// How recently a gate owner must have heartbeat for its gate phase to count
/// as live (#19108). Gate heartbeats default to every five seconds; a window
/// of several minutes tolerates a slow store write without letting a stalled
/// owner hide a recoverable candidate for long.
pub const LIVE_GATE_HEARTBEAT_WINDOW_SECONDS: i64 = 180;

/// A deterministic gate phase whose owner is still running it (#19108).
///
/// A promotion latches `candidate_recoverable` before its gates start so a
/// controller that dies mid-gate leaves a recoverable candidate. While the
/// gate owner is alive and heartbeating, that latch is not the run's verdict:
/// read surfaces report it as gating and recovery refuses to start a second
/// gate run on top of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveGateOwner {
    pub owner_pid: u32,
    /// The gate the owner last reported, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// Seconds since the gate phase started, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    /// The owner's latest gate heartbeat.
    pub heartbeat_at: String,
}

impl LiveGateOwner {
    /// `gate `cargo test` running 3.4m (owner pid 42)`
    pub fn describe(&self) -> String {
        let mut line = match self.gate.as_deref() {
            Some(gate) => format!("gate `{gate}` running"),
            None => "gates running".to_string(),
        };
        if let Some(elapsed) = self.elapsed_seconds {
            line.push_str(&format!(" {:.1}m", elapsed as f64 / 60.0));
        }
        line.push_str(&format!(" (owner pid {})", self.owner_pid));
        line
    }
}

/// The live owner of `record`'s gate phase, if any: the promoted candidate
/// still awaits its gate verdict (`verification_pending`), the durable
/// promotion progress is active in its `gate` phase, its owner process is
/// running and its latest heartbeat is within
/// [`LIVE_GATE_HEARTBEAT_WINDOW_SECONDS`]. A recorded gate verdict ends the
/// gate phase whatever its progress record last said.
pub fn live_gate_owner(record: &AgentTaskRunRecord) -> Option<LiveGateOwner> {
    live_gate_owner_at(record, chrono::Utc::now())
}

pub(crate) fn live_gate_owner_at(
    record: &AgentTaskRunRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<LiveGateOwner> {
    let progress = record.metadata.get("promotion_progress")?;
    if progress["active"] != true || progress["phase"] != "gate" {
        return None;
    }
    if !crate::agent_task_service::CookDelivery::from_run_metadata(&record.metadata)
        .is_some_and(|delivery| !delivery.published() && delivery.awaits_verification())
    {
        return None;
    }
    let owner_pid = progress["owner_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| homeboy_core::process::pid_is_running(*pid))?;
    let heartbeat_at = progress["updated_at"].as_str()?.to_string();
    let heartbeat = chrono::DateTime::parse_from_rfc3339(&heartbeat_at).ok()?;
    if (now - heartbeat.with_timezone(&chrono::Utc)).num_seconds()
        > LIVE_GATE_HEARTBEAT_WINDOW_SECONDS
    {
        return None;
    }
    let elapsed_seconds = progress["started_at"]
        .as_str()
        .and_then(|started| chrono::DateTime::parse_from_rfc3339(started).ok())
        .map(|started| {
            (now - started.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0) as u64
        });
    Some(LiveGateOwner {
        owner_pid,
        gate: progress["gate"]
            .as_str()
            .map(str::trim)
            .filter(|gate| !gate.is_empty())
            .map(str::to_string),
        elapsed_seconds,
        heartbeat_at,
    })
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

/// Resolve a local cancellation target using verified process-start evidence.
/// A recorded runner takes precedence; never fall back from an invalid runner
/// identity to a different execution-step owner.
pub(crate) fn local_cancellable_owner(
    record: &AgentTaskRunRecord,
) -> Option<(u32, homeboy_core::process::ProcessStartIdentity)> {
    let (pid, identity) = if let Some(pid) = record.metadata.get("runner_pid") {
        (pid, &record.metadata["runner_process_start_identity"])
    } else {
        let owner = &record.metadata["execution_step_owner"];
        let intent = &record.metadata["execution_scheduling"]["current_intent_id"];
        if !intent.is_string() || owner["intent_id"] != *intent {
            return None;
        }
        (&owner["pid"], &owner["process_start_identity"])
    };
    let pid = u32::try_from(pid.as_u64()?).ok()?;
    if pid == std::process::id() {
        return None;
    }
    let identity = serde_json::from_value(identity.clone()).ok()?;
    (homeboy_core::process::process_identity_state_with_start_identity(pid, None, Some(&identity))
        == homeboy_core::process::ProcessIdentityState::Live)
        .then_some((pid, identity))
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

pub(crate) fn reads_stale(record: &AgentTaskRunRecord) -> bool {
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

    if record.runner_disconnected() {
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
    LabHandoffAccepted,
    LabHandoffExpired,
    LabHandoffRejected,
    DeliveryRecorded {
        phase: CookRunPhase,
    },
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
            (_, E::LabHandoffAccepted) => self.transition(E::OutcomeRecorded { phase: P::Running }),
            (_, E::LabHandoffExpired) => self.transition(E::Cancelled),
            (_, E::LabHandoffRejected) => self.transition(E::Failed),
            (_, E::DeliveryRecorded { phase }) => self.transition(E::OutcomeRecorded { phase }),
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

/// End-to-end outcome from durable Cook evidence, without recipe reads: the
/// stored `RunExecutionState` plus the refinements its `cook_delivery` record
/// (delivery facts and the Cook's recorded [`CookOutcome`]) defines. Live
/// `cook_progress` is never read as a verdict (#18920).
///
/// [`CookOutcome`]: crate::agent_task_service::CookOutcome
fn observed_execution_state(record: &AgentTaskRunRecord) -> RunExecutionState {
    let stored = record.state();
    let delivery = crate::agent_task_service::CookDelivery::from_run_metadata(&record.metadata);
    // Publication is the end-to-end receipt, not a second verdict derived
    // from a possibly older promotion snapshot.
    if delivery.as_ref().is_some_and(|d| d.published()) {
        return RunExecutionState::Succeeded;
    }
    if stored != RunExecutionState::Succeeded || record.metadata["cook_id"].as_str().is_none() {
        return stored;
    }
    let outcome = crate::agent_task_service::CookOutcome::from_run_metadata(&record.metadata);
    let status = outcome.and_then(|outcome| outcome.status);
    if status == Some(CookStatus::IntentionalNoChange)
        && outcome.is_some_and(|outcome| outcome.exit_code == 0)
    {
        return stored;
    }
    // Delivery finished without a change: there is no candidate to recover,
    // so it must not read as `candidate_recoverable` (#15294).
    if outcome.is_none() && delivery.as_ref().is_some_and(|d| d.is_no_change()) {
        return RunExecutionState::PartialFailure;
    }
    let verified =
        crate::agent_task_service::persisted_promotion_from_record(&record.run_id, record)
            .ok()
            .flatten()
            .is_some_and(|promotion| {
                matches!(
                    promotion.status,
                    crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
                        | crate::agent_task_promotion::AgentTaskPromotionStatus::Gate(
                            GateVerdict::GateFailed
                        )
                        | crate::agent_task_promotion::AgentTaskPromotionStatus::VerifiedNoChanges
                ) && promotion.finalization_eligible()
            });
    if !(verified && status == Some(CookStatus::GreenNoFinalize)) {
        return RunExecutionState::CandidateRecoverable;
    }
    stored
}

/// The status label a [`CookRunState`] reports: a Cook status when the run's
/// evidence settles one, otherwise the bare phase. Serialized as the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CookRunStatus {
    Cook(CookStatus),
    Phase(CookRunPhase),
}

impl CookRunStatus {
    /// The status of a run whose phase is the only evidence. Phases that share
    /// a Cook status label report that status.
    fn of_phase(phase: CookRunPhase) -> Self {
        match phase {
            CookRunPhase::Queued => Self::Cook(CookStatus::Queued),
            CookRunPhase::Continuing => Self::Cook(CookStatus::InFlight),
            CookRunPhase::CandidateRecoverable => Self::Cook(CookStatus::CandidateRecoverable),
            CookRunPhase::Failed => Self::Cook(CookStatus::Failed),
            CookRunPhase::Cancelled => Self::Cook(CookStatus::Cancelled),
            phase => Self::Phase(phase),
        }
    }

    /// The Cook status this label reports, when it is one.
    pub fn cook_status(self) -> Option<CookStatus> {
        match self {
            Self::Cook(status) => Some(status),
            Self::Phase(_) => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Cook(status) => status.as_str(),
            Self::Phase(phase) => phase.as_str(),
        }
    }
}

impl std::fmt::Display for CookRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The one classification of a Cook run, read by every surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookRunState {
    #[serde(skip)]
    pub(crate) liveness: AgentTaskLiveness,
    pub phase: CookRunPhase,
    /// End-to-end execution outcome, distinct from the stored provider outcome.
    pub execution_state: RunExecutionState,
    /// The record's execution ended. A [`CookRunPhase::Continuing`] run is
    /// terminal but still in flight: its continuation owner carries it.
    pub terminal: bool,
    /// A fresh successor attempt may replace this one.
    pub retryable: bool,
    /// An unpublished candidate is retained for recovery.
    pub recoverable: bool,
    /// The Cook mission was fenced. The fence lives on the Cook index, so
    /// [`CookRunState::of`] never sets it; a store-aware caller applies
    /// [`super::cook_cancellation_fence`] through [`CookRunState::with_cook_index_fence`].
    pub fenced: bool,
    /// The recorded controller process is still alive, even after execution
    /// ended, so the run can still be cancelled. A live gate owner counts.
    pub live_owner: bool,
    /// A gate phase still owned by a live, heartbeating process (#19108).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_gate: Option<LiveGateOwner>,
    /// The Cook status label this state reports. A label that reads as in
    /// flight never survives on a terminal record: the record's execution
    /// state outranks a stale label from an earlier exit.
    pub status: CookRunStatus,
}

impl CookRunState {
    pub(crate) fn accepted_lab_handoff(record: &AgentTaskRunRecord) -> bool {
        record.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.is_valid() && handoff.state == super::AgentTaskLabHandoffState::Accepted
        })
    }

    /// Classify `record`. This is the only place a Cook run's phase,
    /// terminality and recoverability are decided.
    pub fn of(record: &AgentTaskRunRecord) -> Self {
        let execution_state = observed_execution_state(record);
        let execution_phase = CookRunPhase::of_execution(execution_state);
        // Delivery, fences and live owners refine the execution phase; they
        // are observations, not durable phase-transition facts.
        let liveness = classify_liveness(record, chrono::Utc::now(), &BTreeSet::new());
        let terminal = !matches!(
            execution_phase,
            CookRunPhase::Queued | CookRunPhase::Running
        );
        let phase =
            if terminal && crate::agent_task_service::live_cook_continuation(record).is_some() {
                CookRunPhase::Continuing
            } else {
                execution_phase
            };
        let status = match phase {
            CookRunPhase::Queued | CookRunPhase::Running | CookRunPhase::Continuing => {
                CookRunStatus::of_phase(phase)
            }
            _ => settled_status(record, phase),
        };
        let live_gate = live_gate_owner(record);
        Self {
            liveness,
            phase,
            execution_state,
            terminal,
            retryable: matches!(
                phase,
                CookRunPhase::Failed | CookRunPhase::Cancelled | CookRunPhase::PartialFailure
            ),
            recoverable: phase == CookRunPhase::CandidateRecoverable,
            fenced: false,
            live_owner: record.owner_process_is_running() || live_gate.is_some(),
            live_gate,
            status,
        }
    }

    /// Apply an event, refusing an illegal edge.
    pub fn apply(&self, event: CookRunEvent) -> Result<Self, CookRunTransitionError> {
        let phase =
            self.phase
                .transition(event)
                .ok_or(CookRunTransitionError::IllegalTransition {
                    from: self.phase,
                    event,
                })?;
        let mut next = self.clone();
        next.phase = phase;
        next.execution_state = match phase {
            CookRunPhase::Continuing => self.execution_state,
            CookRunPhase::Queued => RunExecutionState::Queued,
            CookRunPhase::Running => RunExecutionState::Running,
            CookRunPhase::CandidateRecoverable => RunExecutionState::CandidateRecoverable,
            CookRunPhase::Succeeded => RunExecutionState::Succeeded,
            CookRunPhase::PartialFailure => RunExecutionState::PartialFailure,
            CookRunPhase::Failed => RunExecutionState::Failed,
            CookRunPhase::Cancelled => RunExecutionState::Cancelled,
            CookRunPhase::Unknown => RunExecutionState::Unknown,
        };
        next.terminal = !matches!(phase, CookRunPhase::Queued | CookRunPhase::Running);
        next.retryable = matches!(
            phase,
            CookRunPhase::Failed | CookRunPhase::Cancelled | CookRunPhase::PartialFailure
        );
        next.recoverable = phase == CookRunPhase::CandidateRecoverable && !next.fenced;
        next.status = CookRunStatus::of_phase(phase);
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

    pub fn allows_regate(&self) -> bool {
        self.phase != CookRunPhase::Running
            && self.allows(homeboy_control_plane_contract::ControlPlaneAction::Recover)
    }

    pub fn allows(&self, action: homeboy_control_plane_contract::ControlPlaneAction) -> bool {
        legal_actions(self).contains(&action)
    }

    /// Why a run in this state has no pull request (#18817). Read surfaces
    /// project this instead of re-deriving it from delivery facts. `None`
    /// while an owner may still publish.
    pub fn no_pull_request_reason(&self) -> Option<&'static str> {
        if !self.terminal {
            return None;
        }
        Some(match self.phase {
            CookRunPhase::Continuing => "continuation owner is still delivering the candidate",
            CookRunPhase::CandidateRecoverable if self.fenced => {
                "candidate retained but fenced; publication not attempted"
            }
            CookRunPhase::CandidateRecoverable => "publication not attempted",
            CookRunPhase::Succeeded => "candidate verified; publication not requested",
            CookRunPhase::PartialFailure | CookRunPhase::Failed => {
                "run ended before a candidate was published"
            }
            CookRunPhase::Cancelled => "run was cancelled before publication",
            CookRunPhase::Unknown => "state written by a newer binary; publication unknown",
            CookRunPhase::Queued | CookRunPhase::Running => "publication not reached",
        })
    }
}

/// A refused state write. It never mutates the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookRunTransitionError {
    IllegalTransition {
        from: CookRunPhase,
        event: CookRunEvent,
    },
}

/// Refuse a commit whose phase change is illegal, at the fenced commit
/// boundary. The source is the stored record's state, never a caller's stale
/// projection; an unfenced first write seeds its own phase.
pub(crate) fn transition_record(
    record: &AgentTaskRunRecord,
    stored: Option<&AgentTaskRunRecord>,
) -> homeboy_core::Result<()> {
    let phase = CookRunPhase::of_execution(record.state());
    let source = stored.map_or(phase, |stored| CookRunPhase::of_execution(stored.state()));
    let delivery_changed = stored.is_some_and(|stored| {
        stored.metadata.get("cook_delivery") != record.metadata.get("cook_delivery")
    });
    let event = if let Some(event) = super::lab_offload::handoff_event(record, stored) {
        event
    } else if delivery_changed {
        CookRunEvent::DeliveryRecorded { phase }
    } else if source == phase {
        return Ok(());
    } else {
        match phase {
            CookRunPhase::Queued => CookRunEvent::Requeued,
            CookRunPhase::Running if source != CookRunPhase::Cancelled => CookRunEvent::Started,
            phase => CookRunEvent::OutcomeRecorded { phase },
        }
    };
    source.transition(event).map(|_| ()).ok_or_else(|| {
        homeboy_core::Error::validation_invalid_argument(
            "cook_run_state",
            format!(
                "Cook transition refused: {:?}",
                CookRunTransitionError::IllegalTransition {
                    from: source,
                    event
                }
            ),
            Some(record.run_id.clone()),
            None,
        )
    })
}

/// The status of a terminal attempt: delivery's typed read model first, then
/// the attempt's own failure evidence and recorded Cook outcome, then its
/// execution state.
/// Labels that read as in flight are stale on a terminal record and ignored.
fn settled_status(record: &AgentTaskRunRecord, phase: CookRunPhase) -> CookRunStatus {
    let settled = |status: CookStatus| (!status.is_in_flight()).then_some(status);
    let delivery = crate::agent_task_service::CookDelivery::from_run_metadata(&record.metadata)
        .and_then(|delivery| delivery.lifecycle().cook_status)
        .and_then(settled);
    if let Some(status) = delivery {
        return CookRunStatus::Cook(status);
    }
    if record
        .metadata
        .get("pre_execution_failure")
        .is_some_and(|failure| !failure.is_null())
    {
        return CookRunStatus::Cook(CookStatus::PreExecutionFailure);
    }
    if let Some(status) =
        crate::agent_task_service::CookOutcome::from_run_metadata(&record.metadata)
            .and_then(|outcome| outcome.status)
            .and_then(settled)
    {
        return CookRunStatus::Cook(status);
    }
    match phase {
        CookRunPhase::PartialFailure => CookRunStatus::Cook(CookStatus::Failed),
        phase => CookRunStatus::of_phase(phase),
    }
}

/// Every action `state` makes legal. Verbs refuse exactly what is absent here,
/// so `status` can never advertise a verb that refuses the same run.
pub fn legal_actions(
    state: &CookRunState,
) -> BTreeSet<homeboy_control_plane_contract::ControlPlaneAction> {
    use homeboy_control_plane_contract::ControlPlaneAction as A;
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
        P::Running
        | P::CandidateRecoverable
        | P::Succeeded
        | P::PartialFailure
        | P::Failed
        | P::Cancelled
        | P::Unknown => {
            actions.insert(A::Recover);
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::ControlPlaneAction as A;
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
            execution_state: RunExecutionState::Unknown,
            liveness: AgentTaskLiveness::Stale,
            phase,
            terminal: !matches!(phase, P::Queued | P::Running),
            retryable: false,
            recoverable: phase == P::CandidateRecoverable,
            fenced,
            live_owner,
            live_gate: None,
            status: CookRunStatus::of_phase(phase),
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

    #[test]
    fn illegal_edges_are_refused_in_memory_and_at_commit() {
        let initial = state(P::Queued, false, false);
        assert!(matches!(
            initial.apply(CookRunEvent::Succeeded),
            Err(CookRunTransitionError::IllegalTransition { .. })
        ));
        assert_eq!(initial.phase, P::Queued);
        let stored = record(RunExecutionState::Succeeded, serde_json::json!({}));
        let mut next = stored.clone();
        next.set_state_for_test(RunExecutionState::Running);
        assert!(transition_record(&next, Some(&stored)).is_err());
        assert!(transition_record(&next, None).is_ok());
    }

    /// The exhaustive eligibility table: every phase, with and without a fence
    /// and a live owner, against every action.
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
                        (false, P::Running) => all(&[A::Recover]),
                        (false, P::CandidateRecoverable | P::Succeeded) => all(&[A::Recover]),
                        (false, P::PartialFailure | P::Failed | P::Cancelled | P::Unknown) => {
                            all(&[A::Recover])
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
                    let current = state(phase, fenced, live_owner);
                    assert_eq!(
                        current.allows_regate(),
                        expected.contains(&A::Recover) && phase != P::Running,
                        "regate: {phase:?} fenced={fenced} live={live_owner}"
                    );
                    for action in [A::Recover, A::Cancel] {
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
        assert_eq!(continuing.status, CookRunStatus::Cook(CookStatus::InFlight));
        assert!(!continuing.allows(A::Recover));

        let fenced = CookRunState::of(&record(
            RunExecutionState::CandidateRecoverable,
            serde_json::json!({}),
        ))
        .with_cook_index_fence(true);
        assert!(fenced.fenced && !fenced.recoverable);
        assert!(legal_actions(&fenced).is_empty());
    }

    #[test]
    fn subsystem_events_are_replay_safe() {
        for (event, phase) in [
            (CookRunEvent::LabHandoffAccepted, P::Running),
            (CookRunEvent::LabHandoffExpired, P::Cancelled),
            (CookRunEvent::LabHandoffRejected, P::Failed),
            (
                CookRunEvent::DeliveryRecorded {
                    phase: P::Succeeded,
                },
                P::Succeeded,
            ),
            (
                CookRunEvent::DeliveryRecorded {
                    phase: P::CandidateRecoverable,
                },
                P::CandidateRecoverable,
            ),
        ] {
            let next = state(P::Running, false, false).apply(event).unwrap();
            assert_eq!(next.phase, phase);
            assert_eq!(next.apply(event).unwrap(), next);
        }
    }

    #[test]
    fn subsystem_classification_stays_in_owners() {
        fn scan(path: &std::path::Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy();
                if name.contains("test") || name.contains("fixture") {
                    continue;
                }
                if path.is_dir() {
                    scan(&path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                // Wire validation and fact adapters are the subsystem owners.
                if matches!(
                    name.as_ref(),
                    "records.rs"
                        | "cook_run_state.rs"
                        | "lab_offload.rs"
                        | "lab_handoff_reconciliation.rs"
                        | "terminal_transition.rs"
                        | "lifecycle_ops.rs"
                        | "cook_delivery.rs"
                ) {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let production = source.split("#[cfg(test)]").next().unwrap();
                assert!(
                    !production.contains("AgentTaskLabHandoffState::"),
                    "{}",
                    path.display()
                );
                assert!(
                    !production.contains("CookDeliveryState::"),
                    "{}",
                    path.display()
                );
            }
        }
        scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        for source in [
            include_str!("../agent_task_service/cook.rs"),
            include_str!("../agent_task_service/cook_recipe.rs"),
            include_str!("../agent_task_submission_service.rs"),
            include_str!("lifecycle_candidate_adoption.rs"),
            include_str!("runner_exec.rs"),
            include_str!("private_attachment.rs"),
            include_str!("lifecycle_transport_proxy.rs"),
        ] {
            let production = source.split("#[cfg(test)]").next().unwrap();
            assert!(!production.contains("AgentTaskLabHandoffState::"));
            assert!(!production.contains("CookDeliveryState::"));
        }
    }

    #[test]
    fn no_pull_request_reason_is_owned_by_the_run_state() {
        for phase in CookRunPhase::ALL {
            let reason = state(phase, false, false).no_pull_request_reason();
            assert_eq!(
                reason.is_some(),
                !matches!(phase, P::Queued | P::Running),
                "{phase:?}"
            );
            assert!(reason.is_none_or(|reason| !reason.is_empty()), "{phase:?}");
        }
        assert_ne!(
            state(P::CandidateRecoverable, true, false).no_pull_request_reason(),
            state(P::CandidateRecoverable, false, false).no_pull_request_reason()
        );
    }

    #[test]
    fn subsystem_receipts_drive_fenced_record_transitions() {
        use super::super::{
            AgentTaskLabHandoff, AgentTaskLabHandoffAuthority, AgentTaskLabHandoffState,
        };
        for (wire_state, execution, phase, expected) in [
            (
                AgentTaskLabHandoffState::Accepted,
                RunExecutionState::Running,
                P::Running,
                CookRunEvent::LabHandoffAccepted,
            ),
            (
                AgentTaskLabHandoffState::Expired,
                RunExecutionState::Cancelled,
                P::Cancelled,
                CookRunEvent::LabHandoffExpired,
            ),
            (
                AgentTaskLabHandoffState::Pending,
                RunExecutionState::Failed,
                P::Failed,
                CookRunEvent::LabHandoffRejected,
            ),
        ] {
            let stored = record(RunExecutionState::Running, serde_json::json!({}));
            let mut next = stored.clone();
            next.set_state_for_test(execution);
            next.lab_handoff = Some(AgentTaskLabHandoff {
                state: wire_state,
                authority: if wire_state == AgentTaskLabHandoffState::Accepted { AgentTaskLabHandoffAuthority::RunnerDaemon } else { AgentTaskLabHandoffAuthority::Controller },
                runner_id: "lab".into(), runner_job_id: (wire_state == AgentTaskLabHandoffState::Accepted).then(|| "job".into()),
                ..serde_json::from_value(serde_json::json!({"state":"pending","authority":"controller","runner_id":"lab"})).unwrap()
            });
            // Use the wire fixture's real validity rules, including acceptance timestamps.
            next.lab_handoff.as_mut().unwrap().accepted_at = Some("2026-06-01T00:00:00Z".into());
            assert_eq!(
                super::super::lab_offload::handoff_event(&next, Some(&stored)),
                Some(expected)
            );
            transition_record(&next, Some(&stored)).unwrap();
            assert_eq!(CookRunState::of(&next).phase, phase);
            transition_record(&next, Some(&next)).unwrap();
        }
    }

    #[test]
    fn verbs_do_not_reclassify_candidate_or_in_flight_status() {
        for source in [
            include_str!("action_eligibility.rs"),
            include_str!("../agent_task_service/cook_recovery.rs"),
        ] {
            let production = source.split("#[cfg(test)]").next().expect("source");
            assert!(!production.contains("AgentTaskLabHandoffState::"));
            assert!(!production.contains("CookDeliveryState::"));
            assert!(!production.contains("CookStatus::CandidateRecoverable"));
            assert!(!production.contains(".is_in_flight()"));
        }
    }

    /// Status, diagnose, recover and notifications all read one
    /// classification. The phase comes from the record's execution state and
    /// the outcome detail from its `cook_delivery` record; a `cook_progress`
    /// label is never a verdict (#18920).
    #[test]
    fn cook_outcomes_classify_from_phase_and_delivery_only() {
        use crate::agent_task_service::{set_cook_delivery_artifact_for_test, CookOutcome};
        let outcome = |metadata: &mut Value, status: CookStatus, exit_code: i32| {
            CookOutcome::write(
                metadata,
                Some(CookOutcome {
                    status: Some(status),
                    exit_code,
                }),
            );
        };
        let cook =
            || serde_json::json!({ "cook_id": "cook", "cook_progress": { "phase": "terminal" } });
        let mut no_changes = cook();
        outcome(&mut no_changes, CookStatus::Gate(GateVerdict::NoChanges), 2);
        let mut intentional = cook();
        outcome(&mut intentional, CookStatus::IntentionalNoChange, 0);
        let mut gate_failed = cook();
        set_cook_delivery_artifact_for_test(
            &mut gate_failed,
            "/promotion",
            serde_json::json!({ "status": "gate_failed" }),
        );
        outcome(
            &mut gate_failed,
            CookStatus::Gate(GateVerdict::GateFailed),
            1,
        );
        let mut published = cook();
        set_cook_delivery_artifact_for_test(
            &mut published,
            "/promotion",
            serde_json::json!({ "status": "applied" }),
        );
        set_cook_delivery_artifact_for_test(
            &mut published,
            "/finalization",
            serde_json::json!({ "status": "review_ready", "pr_url": "https://example.test/pull/1" }),
        );
        outcome(&mut published, CookStatus::ReviewReady, 0);
        // A record written before #18920 keeps its verdict label in
        // `cook_progress`; it reads from its phase instead.
        let legacy_label = serde_json::json!({
            "cook_id": "cook",
            "cook_progress": { "phase": "terminal", "terminal_status": "gate_failed", "exit_code": 1 },
        });
        let legacy_in_flight = serde_json::json!({
            "cook_id": "cook",
            "cook_progress": { "phase": "terminal", "terminal_status": "in_flight" },
        });

        for (name, stored, metadata, phase, status, recoverable) in [
            (
                "no_changes",
                RunExecutionState::PartialFailure,
                no_changes,
                P::PartialFailure,
                CookStatus::Gate(GateVerdict::NoChanges),
                false,
            ),
            (
                "intentional_no_change",
                RunExecutionState::Succeeded,
                intentional,
                P::Succeeded,
                CookStatus::IntentionalNoChange,
                false,
            ),
            (
                "gate_failed",
                RunExecutionState::CandidateRecoverable,
                gate_failed,
                P::CandidateRecoverable,
                CookStatus::Gate(GateVerdict::GateFailed),
                true,
            ),
            (
                "published",
                RunExecutionState::Running,
                published,
                P::Succeeded,
                CookStatus::ReviewReady,
                false,
            ),
            (
                "legacy_label",
                RunExecutionState::PartialFailure,
                legacy_label,
                P::PartialFailure,
                CookStatus::Failed,
                false,
            ),
            (
                "legacy_in_flight",
                RunExecutionState::CandidateRecoverable,
                legacy_in_flight,
                P::CandidateRecoverable,
                CookStatus::CandidateRecoverable,
                true,
            ),
        ] {
            let classified = CookRunState::of(&record(stored, metadata));
            assert_eq!(classified.phase, phase, "{name}");
            assert!(classified.terminal, "{name}");
            assert_eq!(classified.status, CookRunStatus::Cook(status), "{name}");
            assert_eq!(classified.recoverable, recoverable, "{name}");
            assert_eq!(classified.retryable, phase == P::PartialFailure, "{name}");
            assert!(classified.allows(A::Recover), "{name}");
            assert!(classified.allows_regate(), "{name}");
        }
    }

    /// #19108: a candidate latched recoverable while a live owner still runs
    /// its gates is not recoverable; a stale or dead gate owner is.
    #[test]
    fn live_gate_owner_blocks_recovery_until_it_stops() {
        use crate::agent_task_service::set_cook_delivery_artifact_for_test;
        let now = chrono::Utc::now();
        let gating_with = |promotion: &str, phase: &str, pid: u64, heartbeat_age: i64| {
            let mut metadata = serde_json::json!({
                "cook_id": "cook",
                "promotion_progress": {
                    "active": true,
                    "phase": phase,
                    "gate": "cargo check",
                    "started_at": (now - chrono::Duration::seconds(204)).to_rfc3339(),
                    "updated_at": (now - chrono::Duration::seconds(heartbeat_age)).to_rfc3339(),
                    "owner_pid": pid,
                },
            });
            set_cook_delivery_artifact_for_test(
                &mut metadata,
                "/promotion",
                serde_json::json!({ "status": promotion }),
            );
            record(RunExecutionState::CandidateRecoverable, metadata)
        };
        let gating = |phase: &str, pid: u64, heartbeat_age: i64| {
            gating_with("verification_pending", phase, pid, heartbeat_age)
        };
        let live_pid = u64::from(std::process::id());

        let live = gating("gate", live_pid, 5);
        let owner = live_gate_owner_at(&live, now).expect("live gate owner");
        assert_eq!(owner.gate.as_deref(), Some("cargo check"));
        assert_eq!(owner.elapsed_seconds, Some(204));
        assert!(owner
            .describe()
            .starts_with("gate `cargo check` running 3.4m"));
        let state = CookRunState::of(&live);
        assert_eq!(state.phase, P::CandidateRecoverable);
        assert!(state.live_gate.is_some());
        assert!(!state.allows(A::Recover));
        assert!(!state.allows_regate());
        assert!(state.allows(A::Cancel));

        for (name, record) in [
            (
                "stale heartbeat",
                gating("gate", live_pid, LIVE_GATE_HEARTBEAT_WINDOW_SECONDS + 1),
            ),
            ("dead owner", gating("gate", 99_999_999, 5)),
            ("gates finished", gating("finalization", live_pid, 5)),
            (
                "gate verdict recorded",
                gating_with("gate_environment_unavailable", "gate", live_pid, 5),
            ),
        ] {
            assert_eq!(live_gate_owner_at(&record, now), None, "{name}");
            assert!(CookRunState::of(&record).allows_regate(), "{name}");
        }
    }
}
