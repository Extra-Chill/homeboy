//! The one Cook run state and the actions it makes legal (#18436 slices 1-2).
//!
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
//! The state is derived on read, not persisted: it is a pure function of the
//! record's execution state, its continuation claim and its fence, so it can
//! never drift from the record it describes.

use std::collections::BTreeSet;

use homeboy_core::cook_status::CookStatus;
use homeboy_lifecycle_contract::RunExecutionState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AgentTaskRunRecord;

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

    fn of_execution(state: RunExecutionState) -> Self {
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
            // A newer binary's state carries no verdict to transition from.
            (P::Unknown, _) => None,
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
        let execution = record.state();
        let phase = if execution.is_terminal()
            && crate::agent_task_service::live_cook_continuation(record).is_some()
        {
            CookRunPhase::Continuing
        } else {
            CookRunPhase::of_execution(execution)
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
            phase,
            terminal: execution.is_terminal(),
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
    if state.fenced {
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
            phase,
            terminal: !matches!(phase, P::Queued | P::Running),
            retryable: false,
            recoverable: phase == P::CandidateRecoverable,
            fenced,
            live_owner,
            status: phase.as_str().to_string(),
        }
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
                [None, None, None, None, None, None, None, Some(P::Cancelled)],
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
