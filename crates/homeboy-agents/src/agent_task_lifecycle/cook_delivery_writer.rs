//! The single writer of the persisted `cook_delivery` record (#16049).
//!
//! Every Cook delivery step (promotion, finalization, delivery blockers and
//! force-with-lease receipts) commits through
//! [`record_cook_delivery`]. The step records its artifact in the record it
//! read; the writer projects the delivery facts from the result, stores both
//! as the versioned `cook_delivery` record, and moves the run state the
//! delivery asks for, all in one compare-and-swap commit (#15718). A step that
//! changes nothing commits nothing. A concurrent writer makes the commit
//! conflict; the retry re-reads and re-projects, so neither writer's facts are
//! lost.

use super::*;
use crate::agent_task_service::{
    CookDelivery, CookDeliveryArtifacts, CookDeliveryBlockerCode, CookOutcome, COOK_DELIVERY_KEY,
};
use homeboy_core::cook_status::{CookStatus, GateVerdict};

/// How a delivery receipt decides the run state it commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CookDeliveryCommit {
    /// The run state follows the delivery (see [`CookDelivery::lifecycle`]).
    FollowDelivery,
}

/// The `cook_delivery` record a delivery step updates.
pub(crate) struct CookDeliveryDraft {
    /// The full artifacts the steps recorded; facts are projected from them.
    pub artifacts: CookDeliveryArtifacts,
    /// The delivery the record held. A step that records a blocker without a
    /// receipt of its own sets it here (see [`CookDelivery::project`]).
    pub recorded: Option<CookDelivery>,
    /// The Cook's recorded terminal outcome (see [`CookOutcome`]).
    pub outcome: Option<CookOutcome>,
}

/// Commit a delivery step and the `cook_delivery` record it implies.
///
/// `record_step` updates a copy of the current run and its delivery record and
/// returns how to commit it, or `None` when the step must not be recorded. It
/// may be called again on a fresher record after a conflict.
pub(crate) fn record_cook_delivery(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    mut record_step: impl FnMut(
        &mut AgentTaskRunRecord,
        &mut CookDeliveryDraft,
    ) -> Option<CookDeliveryCommit>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let committed = lifecycle_store.transition_when(&run_id, |current| {
        let mut prepared = current.clone();
        let previous = CookDelivery::from_run_metadata(&current.metadata);
        let mut draft = CookDeliveryDraft {
            artifacts: CookDeliveryArtifacts::from_run_metadata(&current.metadata),
            recorded: previous.clone(),
            outcome: CookOutcome::from_run_metadata(&current.metadata),
        };
        let commit = record_step(&mut prepared, &mut draft)?;
        let delivery = CookDelivery::project(&draft.artifacts, draft.recorded.as_ref());
        let outcome = retire_superseded_deferral(draft.outcome, delivery.as_ref());
        let metadata = prepared.ensure_metadata_object();
        if delivery.is_none() && draft.artifacts == CookDeliveryArtifacts::default() {
            metadata.remove(COOK_DELIVERY_KEY);
        } else {
            metadata.insert(
                COOK_DELIVERY_KEY.to_string(),
                CookDelivery::to_record(delivery.as_ref(), &draft.artifacts),
            );
        }
        CookOutcome::write(&mut prepared.metadata, outcome);
        let next_state = match commit {
            CookDeliveryCommit::FollowDelivery => {
                delivery_run_state(previous.as_ref(), delivery.as_ref(), current)
            }
        }
        .filter(|state| *state != current.state());
        if next_state.is_none() && prepared == *current {
            return None;
        }
        prepared.updated_at = Some(now_timestamp());
        if let Some(state) = next_state {
            set_run_state(&mut prepared, state);
        }
        let prepared = Box::new(prepared);
        Some(if prepared.state() == current.state() {
            AgentTaskTransition::Annotate { prepared }
        } else {
            AgentTaskTransition::CommitRecord { prepared }
        })
    })?;
    match committed {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}

/// The run state a delivery change moves its run to, if any.
///
/// A delivery that becomes unverified or rejected makes its run recoverable.
/// One that becomes verified returns a run to `Succeeded` only when the
/// recoverable state was latched by an unverified candidate (pending or
/// deferred gates) of the run's own progression: a gate failure stays a
/// permanent scar on the run, and an active candidate adoption owns its own
/// revert (homeboy#15008).
fn delivery_run_state(
    previous: Option<&CookDelivery>,
    next: Option<&CookDelivery>,
    run: &AgentTaskRunRecord,
) -> Option<RunExecutionState> {
    let next = next?;
    // A cancelled run keeps its cancellation (#16046), and a receipt that
    // leaves the delivery unchanged moves nothing.
    if run.state() == RunExecutionState::Cancelled || previous == Some(next) {
        return None;
    }
    let state = next.lifecycle().run_state?;
    (state != RunExecutionState::Succeeded
        || (CookRunState::of(run).recoverable
            && previous.is_some_and(CookDelivery::is_unverified)
            && run.candidate_adoption.is_none()))
    .then_some(state)
}

/// A `gate_environment_unavailable` Cook outcome describes deferred gates.
/// Once a later gate run executes, the delivery is verified or rejected and
/// that outcome no longer holds: replace it with the status the delivery
/// reports, or clear it so the Cook records its next outcome, instead of
/// latching an unverified status over an executed gate.
fn retire_superseded_deferral(
    outcome: Option<CookOutcome>,
    delivery: Option<&CookDelivery>,
) -> Option<CookOutcome> {
    let deferred = CookStatus::Gate(GateVerdict::GateEnvironmentUnavailable);
    if outcome.and_then(|outcome| outcome.status) != Some(deferred)
        || delivery.is_some_and(|delivery| {
            delivery.blocker_code()
                == Some(CookDeliveryBlockerCode::Gate(
                    GateVerdict::GateEnvironmentUnavailable,
                ))
        })
    {
        return outcome;
    }
    delivery
        .and_then(|delivery| delivery.lifecycle().cook_status)
        .map(|status| CookOutcome {
            status: Some(status),
            exit_code: i32::from(!status.is_success_exit()),
        })
}
