//! Typed compare-and-swap terminal transitions for agent-task runs (#15718).
//!
//! `write_record` rewrites the whole snapshot under the config lock. A writer
//! that read the record earlier and then decided to terminalize it replaced
//! whatever had happened since, including another writer's terminal decision:
//! the only guard was SQL refusing a `running` status over a terminal row, and
//! a terminal-over-terminal write is not `running`.
//!
//! [`AgentTaskLifecycleStore::transition`] instead takes the revision the
//! caller decided from. The store commits only while the stored revision still
//! equals it (checked in the same SQL statement as the row replacement), bumps
//! the revision, and appends a `run.transitioned` event in that transaction. A
//! stale writer gets [`AgentTaskTransitionError::Conflict`] and writes nothing.
//!
//! This first slice covers terminal outcomes only. Non-terminal writes still go
//! through `write_record`/`mutate_record`, which bump the revision on every
//! commit so a terminal transition decided before them is detected.

use super::*;

/// The terminal run states a typed transition may commit. Mirrors the
/// terminal subset of [`AgentTaskRunState`] exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskTerminalOutcome {
    Succeeded,
    CandidateRecoverable,
    PartialRecoverable,
    PartialFailure,
    Failed,
    Cancelled,
}

impl AgentTaskTerminalOutcome {
    pub fn run_state(self) -> AgentTaskRunState {
        match self {
            Self::Succeeded => AgentTaskRunState::Succeeded,
            Self::CandidateRecoverable => AgentTaskRunState::CandidateRecoverable,
            Self::PartialRecoverable => AgentTaskRunState::PartialRecoverable,
            Self::PartialFailure => AgentTaskRunState::PartialFailure,
            Self::Failed => AgentTaskRunState::Failed,
            Self::Cancelled => AgentTaskRunState::Cancelled,
        }
    }

    pub fn from_run_state(state: AgentTaskRunState) -> Option<Self> {
        match state {
            AgentTaskRunState::Succeeded => Some(Self::Succeeded),
            AgentTaskRunState::CandidateRecoverable => Some(Self::CandidateRecoverable),
            AgentTaskRunState::PartialRecoverable => Some(Self::PartialRecoverable),
            AgentTaskRunState::PartialFailure => Some(Self::PartialFailure),
            AgentTaskRunState::Failed => Some(Self::Failed),
            AgentTaskRunState::Cancelled => Some(Self::Cancelled),
            AgentTaskRunState::Queued | AgentTaskRunState::Running => None,
        }
    }
}

/// A typed terminal transition. The store applies it to the record stored at
/// the expected revision, never to a caller-held snapshot of unknown age.
#[derive(Debug, Clone)]
pub enum AgentTaskTransition {
    /// Terminalize the run. `metadata` entries replace the same top-level
    /// metadata keys (a `null` value removes the key); every other field of
    /// the stored record is preserved.
    Terminate {
        outcome: AgentTaskTerminalOutcome,
        metadata: serde_json::Map<String, Value>,
    },
    /// Commit a terminal aggregate projection the caller prepared from the
    /// record at the expected revision.
    ///
    /// The aggregate projection still replaces the record's task, artifact,
    /// handle and evidence fields wholesale, and its callers attach
    /// source-specific evidence to the prepared record first, so the prepared
    /// record is carried as-is. The revision check is what makes that safe: it
    /// commits only if nothing else was written since the caller read it.
    ProjectAggregate {
        prepared: Box<AgentTaskRunRecord>,
        aggregate: Box<AgentTaskAggregate>,
    },
}

impl AgentTaskTransition {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Terminate { .. } => "terminate",
            Self::ProjectAggregate { .. } => "project_aggregate",
        }
    }

    /// The terminal state this transition commits.
    pub fn target_state(&self) -> AgentTaskRunState {
        match self {
            Self::Terminate { outcome, .. } => outcome.run_state(),
            Self::ProjectAggregate { prepared, .. } => prepared.state,
        }
    }

    /// Apply to the record stored at the expected revision. Returns the record
    /// to commit and the aggregate to mirror beside it.
    pub(crate) fn apply(
        self,
        stored: AgentTaskRunRecord,
        stored_aggregate: Option<AgentTaskAggregate>,
    ) -> Result<(AgentTaskRunRecord, Option<AgentTaskAggregate>, bool)> {
        match self {
            Self::Terminate { outcome, metadata } => {
                let mut record = stored;
                let object = record.ensure_metadata_object();
                for (key, value) in metadata {
                    if value.is_null() {
                        object.remove(&key);
                    } else {
                        object.insert(key, value);
                    }
                }
                record.updated_at = Some(now_timestamp());
                set_run_state(&mut record, outcome.run_state());
                Ok((record, stored_aggregate, false))
            }
            Self::ProjectAggregate {
                prepared,
                aggregate,
            } => {
                if prepared.run_id != stored.run_id || prepared.plan_id != stored.plan_id {
                    return Err(Error::validation_invalid_argument(
                        "transition",
                        "an aggregate projection must keep the stored run and plan identity",
                        Some(stored.run_id),
                        None,
                    ));
                }
                if !prepared.state.is_terminal() {
                    return Err(Error::validation_invalid_argument(
                        "transition",
                        "an aggregate projection must commit a terminal run state",
                        Some(stored.run_id),
                        None,
                    ));
                }
                Ok((*prepared, Some(*aggregate), true))
            }
        }
    }
}

/// A transition refused because the stored record moved past the caller's
/// expected revision. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskTransitionConflict {
    pub run_id: String,
    pub expected_revision: u64,
    pub actual_revision: u64,
    /// The stored state the conflicting writer left behind, when decodable.
    pub actual_state: Option<AgentTaskRunState>,
    pub transition: &'static str,
}

#[derive(Debug)]
pub enum AgentTaskTransitionError {
    Conflict(AgentTaskTransitionConflict),
    Store(Error),
}

/// `reason_code` carried in the details of a converted conflict, so callers
/// that only see [`Error`] can still recognize it.
pub const AGENT_TASK_TRANSITION_CONFLICT: &str = "agent_task_transition_conflict";

impl From<Error> for AgentTaskTransitionError {
    fn from(error: Error) -> Self {
        Self::Store(error)
    }
}

impl From<AgentTaskTransitionError> for Error {
    fn from(error: AgentTaskTransitionError) -> Self {
        match error {
            AgentTaskTransitionError::Store(error) => error,
            AgentTaskTransitionError::Conflict(conflict) => {
                let mut error = Error::validation_invalid_argument(
                    "expected_revision",
                    format!(
                        "agent-task run `{}` moved from revision {} to {} (state {:?}) before its `{}` transition committed; nothing was written",
                        conflict.run_id,
                        conflict.expected_revision,
                        conflict.actual_revision,
                        conflict.actual_state,
                        conflict.transition,
                    ),
                    Some(conflict.run_id.clone()),
                    None,
                );
                let extra = json!({
                    "reason_code": AGENT_TASK_TRANSITION_CONFLICT,
                    "expected_revision": conflict.expected_revision,
                    "actual_revision": conflict.actual_revision,
                    "actual_state": conflict.actual_state,
                    "transition": conflict.transition,
                });
                match error.details.as_object_mut() {
                    Some(details) => {
                        if let Some(extra) = extra.as_object() {
                            details.extend(extra.clone());
                        }
                    }
                    None => error.details = extra,
                }
                error
            }
        }
    }
}

/// Bounded re-read attempts for transitions whose decision is re-evaluated
/// against each fresh read. Each attempt holds the config lock, so a conflict
/// means a writer outside it (another store or an older binary) committed.
pub(crate) const TRANSITION_RETRY_ATTEMPTS: usize = 3;
