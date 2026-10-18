//! The single writer of the persisted `cook_delivery` record (#16049).
//!
//! Every delivery stage (promotion, finalization, delivery blockers,
//! manual finalization and force-with-lease receipts) commits through
//! [`record_cook_delivery`]. It applies the stage's own receipt to the record
//! it read, re-derives the delivery from the result, stores it as the
//! versioned `cook_delivery` record, and moves the run state the delivery
//! asks for, all in one compare-and-swap commit (#15718). A concurrent writer
//! makes the commit conflict; the retry re-reads and re-derives, so neither
//! writer's facts are lost.

use super::*;
use crate::agent_task_service::{CookDelivery, CookDeliveryBlockerCode, COOK_DELIVERY_KEY};
use homeboy_core::cook_status::CookStatus;

/// How a delivery receipt decides the run state it commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CookDeliveryCommit {
    /// The run state follows the delivery (see [`CookDelivery::lifecycle`]).
    FollowDelivery,
    /// The receipt terminalizes the run itself, e.g. a manual publication
    /// receipt or failure.
    Terminal(AgentTaskTerminalOutcome),
}

/// Commit a delivery receipt and the `cook_delivery` record it implies.
///
/// `record_receipt` applies the stage's receipt to a copy of the current
/// record and returns how to commit it, or `None` when the receipt is already
/// recorded. It may be called again on a fresher record after a conflict.
pub(crate) fn record_cook_delivery(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    mut record_receipt: impl FnMut(&mut AgentTaskRunRecord) -> Option<CookDeliveryCommit>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let committed = lifecycle_store.transition_when(&run_id, |current| {
        let mut prepared = current.clone();
        let commit = record_receipt(&mut prepared)?;
        prepared.updated_at = Some(now_timestamp());
        let delivery = CookDelivery::from_legacy_metadata(&prepared.metadata);
        retire_superseded_deferral(&mut prepared, delivery.as_ref());
        let metadata = prepared.ensure_metadata_object();
        match &delivery {
            Some(delivery) => metadata.insert(COOK_DELIVERY_KEY.to_string(), delivery.to_record()),
            None => metadata.remove(COOK_DELIVERY_KEY),
        };
        let next_state = match commit {
            CookDeliveryCommit::Terminal(outcome) => Some(outcome.run_state()),
            CookDeliveryCommit::FollowDelivery => delivery_run_state(
                CookDelivery::from_run_metadata(&current.metadata).as_ref(),
                delivery.as_ref(),
                current,
            ),
        };
        if let Some(state) = next_state {
            set_run_state(&mut prepared, state);
        }
        let prepared = Box::new(prepared);
        Some(if prepared.state == current.state {
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
) -> Option<AgentTaskRunState> {
    let next = next?;
    // A cancelled run keeps its cancellation (#16046), and a receipt that
    // leaves the delivery unchanged moves nothing.
    if run.state == AgentTaskRunState::Cancelled || previous == Some(next) {
        return None;
    }
    match next.lifecycle().run_state? {
        AgentTaskRunState::CandidateRecoverable => (run.state
            != AgentTaskRunState::CandidateRecoverable)
            .then_some(AgentTaskRunState::CandidateRecoverable),
        AgentTaskRunState::Succeeded => (run.state == AgentTaskRunState::CandidateRecoverable
            && previous.is_some_and(CookDelivery::is_unverified)
            && run.candidate_adoption.is_none())
        .then_some(AgentTaskRunState::Succeeded),
        _ => None,
    }
}

/// A `gates_deferred` Cook result describes deferred gates. Once a later gate
/// run executes, the delivery is verified or rejected and that result no
/// longer holds: replace it with the status the delivery reports, or clear it
/// so the Cook records its next result, instead of latching an unverified
/// status over an executed gate.
fn retire_superseded_deferral(record: &mut AgentTaskRunRecord, delivery: Option<&CookDelivery>) {
    if delivery.is_some_and(|delivery| {
        delivery.blocker_code() == Some(CookDeliveryBlockerCode::GatesDeferred)
    }) {
        return;
    }
    let Some(progress) = record
        .metadata
        .get_mut("cook_progress")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let deferred = progress
        .get("terminal_status")
        .and_then(Value::as_str)
        .map(CookStatus::from_status)
        == Some(CookStatus::GatesDeferred);
    if !deferred {
        return;
    }
    match delivery.and_then(|delivery| delivery.lifecycle().cook_status) {
        Some(status) => {
            let success = status.is_success_exit();
            progress.insert("terminal_status".to_string(), json!(status.as_str()));
            progress.insert("terminal_success".to_string(), json!(success));
            progress.insert("exit_code".to_string(), json!(i32::from(!success)));
        }
        None => {
            for key in ["terminal_status", "terminal_success", "exit_code"] {
                progress.remove(key);
            }
        }
    }
}
