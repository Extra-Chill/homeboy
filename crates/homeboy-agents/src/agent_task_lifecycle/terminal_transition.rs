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
//! Non-terminal writes are fenced the same way. A caller-held snapshot is
//! committed only as [`AgentTaskTransition::Annotate`], which may not change
//! the run state, the Lab handoff or workspace ownership; those move only
//! through a typed [`AgentTaskTransition::Advance`], which validates the
//! state it leaves against the stored record.
//! `mutate_record` re-reads under the config lock and commits at the revision
//! it read, so no production writer can commit a stale snapshot.
//!
//! Whatever the commit kind, the edge from the stored state to the committed
//! one must be legal in [`AgentTaskRunState::can_transition_to`]. The single
//! write path refuses any other edge with [`AGENT_TASK_ILLEGAL_TRANSITION`];
//! the per-kind rules below only narrow it.

use super::*;

/// The terminal run states a typed transition may commit. Mirrors the
/// terminal subset of [`AgentTaskRunState`] exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskTerminalOutcome {
    Succeeded,
    CandidateRecoverable,
    PartialFailure,
    Failed,
    Cancelled,
}

impl AgentTaskTerminalOutcome {
    pub fn run_state(self) -> AgentTaskRunState {
        match self {
            Self::Succeeded => AgentTaskRunState::Succeeded,
            Self::CandidateRecoverable => AgentTaskRunState::CandidateRecoverable,
            Self::PartialFailure => AgentTaskRunState::PartialFailure,
            Self::Failed => AgentTaskRunState::Failed,
            Self::Cancelled => AgentTaskRunState::Cancelled,
        }
    }

    pub fn from_run_state(state: AgentTaskRunState) -> Option<Self> {
        match state {
            AgentTaskRunState::Succeeded => Some(Self::Succeeded),
            AgentTaskRunState::CandidateRecoverable => Some(Self::CandidateRecoverable),
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
    /// Commit a terminal record the caller prepared from the record at the
    /// expected revision, leaving the mirrored aggregate untouched.
    ///
    /// For terminal writes whose source-specific projection touches more than
    /// top-level metadata (task states, provider handles, runner evidence), so
    /// it cannot be expressed as [`Self::Terminate`]. As with
    /// [`Self::ProjectAggregate`], the revision check is what makes carrying
    /// the prepared record safe.
    CommitRecord { prepared: Box<AgentTaskRunRecord> },
    /// Commit a record the caller prepared from the record at the expected
    /// revision, changing neither the run state, nor the Lab handoff, nor
    /// workspace ownership. This is the only form a caller-held snapshot can
    /// be committed in; it records no transition event.
    Annotate { prepared: Box<AgentTaskRunRecord> },
    /// A typed non-terminal state or ownership change, prepared from the
    /// record at the expected revision and validated against it.
    Advance {
        kind: AgentTaskAdvance,
        prepared: Box<AgentTaskRunRecord>,
    },
}

/// The non-terminal state and ownership transitions a run can make. Each is
/// validated against the stored record it replaces; a conflict names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTaskAdvance {
    /// Create the run from a plan submission, or replace its plan-derived
    /// fields on re-submission.
    Submit,
    /// A consumer claimed the run: `Queued` -> `Running`, or a new owner
    /// reclaimed a running run whose owner is gone.
    Start,
    /// Return a live run to the queue before provider work begins.
    Requeue,
    /// Publish or refresh the controller's pending Lab handoff.
    PendLabHandoff,
    /// Bind the runner daemon's accepted job, moving the run to `Running`.
    BindLabHandoff,
    /// Drop an unaccepted Lab handoff so the run executes locally.
    ReleaseLabHandoff,
    /// Mark an unaccepted Lab handoff expired on a run already cancelled.
    ExpireLabHandoff,
}

impl AgentTaskAdvance {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::Start => "start",
            Self::Requeue => "requeue",
            Self::PendLabHandoff => "pend_lab_handoff",
            Self::BindLabHandoff => "bind_lab_handoff",
            Self::ReleaseLabHandoff => "release_lab_handoff",
            Self::ExpireLabHandoff => "expire_lab_handoff",
        }
    }

    /// Why `prepared` is not this transition from `stored`, if it is not.
    fn violation(
        self,
        stored: &AgentTaskRunRecord,
        prepared: &AgentTaskRunRecord,
    ) -> Option<&'static str> {
        let handoff_state =
            |record: &AgentTaskRunRecord| record.lab_handoff.as_ref().map(|h| h.state);
        match self {
            Self::Submit => None,
            Self::Start => (stored.state().is_terminal()
                || prepared.state() != AgentTaskRunState::Running)
                .then_some("start must move a live run to running"),
            Self::Requeue => (stored.state().is_terminal()
                || prepared.state() != AgentTaskRunState::Queued)
                .then_some("requeue must return a live run to the queue"),
            Self::PendLabHandoff => (stored.state().is_terminal()
                || prepared.state() != stored.state()
                || accepted(stored).is_some()
                || handoff_state(prepared) != Some(AgentTaskLabHandoffState::Pending))
            .then_some("a pending Lab handoff requires a live, unaccepted run"),
            Self::BindLabHandoff => match (accepted(stored), accepted(prepared)) {
                _ if prepared.state() != AgentTaskRunState::Running => {
                    Some("binding a Lab handoff must leave the run running")
                }
                (_, None) => Some("binding a Lab handoff must accept a runner job"),
                (Some(stored), Some(prepared))
                    if stored.runner_job_id != prepared.runner_job_id =>
                {
                    Some("an accepted Lab handoff cannot be rebound to another job")
                }
                // Late acceptance may supersede only the synthetic expiry
                // cancellation of an unaccepted handoff.
                _ if stored.state().is_terminal()
                    && handoff_state(stored) != Some(AgentTaskLabHandoffState::Expired) =>
                {
                    Some("a terminal run cannot bind a Lab handoff")
                }
                _ => None,
            },
            Self::ReleaseLabHandoff => (stored.state().is_terminal()
                || prepared.state() != stored.state()
                || accepted(stored).is_some()
                || prepared.lab_handoff.is_some())
            .then_some("only a live, unaccepted Lab handoff can be released"),
            Self::ExpireLabHandoff => (stored.state() != AgentTaskRunState::Cancelled
                || prepared.state() != stored.state()
                || handoff_state(prepared) != Some(AgentTaskLabHandoffState::Expired))
            .then_some("only a cancelled run's Lab handoff can expire"),
        }
    }
}

/// The runner daemon's accepted handoff, when the record carries one.
fn accepted(record: &AgentTaskRunRecord) -> Option<&AgentTaskLabHandoff> {
    record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
            && handoff.runner_job_id.is_some()
    })
}

/// Why `prepared` changes something only a typed transition may change.
fn ownership_change(
    stored: &AgentTaskRunRecord,
    prepared: &AgentTaskRunRecord,
) -> Option<&'static str> {
    let lease_owner = |record: &AgentTaskRunRecord| {
        record
            .workspace_owner_lease
            .as_ref()
            .map(|lease| (lease.workspace.clone(), lease.owner_id.clone()))
    };
    if prepared.state() != stored.state() {
        Some("the run state")
    } else if prepared.lab_handoff != stored.lab_handoff {
        Some("the Lab handoff")
    } else if prepared.workspace_identity != stored.workspace_identity
        || prepared.workspace_claim != stored.workspace_claim
        || lease_owner(prepared) != lease_owner(stored)
    {
        Some("workspace ownership")
    } else {
        None
    }
}

impl AgentTaskTransition {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Terminate { .. } => "terminate",
            Self::ProjectAggregate { .. } => "project_aggregate",
            Self::CommitRecord { .. } => "commit_record",
            Self::Annotate { .. } => "annotate",
            Self::Advance { kind, .. } => kind.name(),
        }
    }

    /// Whether the commit is recorded as a durable `run.transitioned` event:
    /// transitions into a terminal state. A non-terminal advance is already
    /// recorded by the state progress event its write emits, and annotations
    /// are not transitions; neither may grow the unpruned durable log.
    pub(crate) fn records_event(&self) -> bool {
        !matches!(self, Self::Annotate { .. } | Self::Advance { .. })
    }

    /// The state this transition commits.
    pub fn target_state(&self) -> AgentTaskRunState {
        match self {
            Self::Terminate { outcome, .. } => outcome.run_state(),
            Self::ProjectAggregate { prepared, .. }
            | Self::CommitRecord { prepared }
            | Self::Annotate { prepared }
            | Self::Advance { prepared, .. } => prepared.state(),
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
                validate_prepared_terminal_record(
                    &prepared,
                    &stored,
                    "project_aggregate",
                    "an aggregate projection",
                )?;
                Ok((
                    keep_committed_cancellation(&stored, *prepared),
                    Some(*aggregate),
                    true,
                ))
            }
            Self::CommitRecord { prepared } => {
                validate_prepared_terminal_record(
                    &prepared,
                    &stored,
                    "commit_record",
                    "a terminal record commit",
                )?;
                Ok((
                    keep_committed_cancellation(&stored, *prepared),
                    stored_aggregate,
                    false,
                ))
            }
            Self::Annotate { prepared } => {
                validate_identity(&prepared, &stored, "an annotation")?;
                if let Some(field) = ownership_change(&stored, &prepared) {
                    return Err(illegal_transition(
                        &stored,
                        prepared.state(),
                        "annotate",
                        &format!("an annotation cannot change {field}; commit a typed transition"),
                    ));
                }
                Ok((*prepared, stored_aggregate, false))
            }
            Self::Advance { kind, prepared } => {
                validate_identity(&prepared, &stored, kind.name())?;
                if let Some(problem) = kind.violation(&stored, &prepared) {
                    return Err(illegal_transition(
                        &stored,
                        prepared.state(),
                        kind.name(),
                        &format!("{problem} (stored state {:?})", stored.state()),
                    ));
                }
                Ok((*prepared, stored_aggregate, false))
            }
        }
    }
}

/// A cancelled run's provenance is its immutable `run.cancelled` receipt. A
/// prepared record decided before that commit (a runner projection minting its
/// own timestamp, a rebased aggregate) re-commits the cancellation it already
/// has, so it keeps the stored cause instead of colliding with the receipt.
fn keep_committed_cancellation(
    stored: &AgentTaskRunRecord,
    mut prepared: AgentTaskRunRecord,
) -> AgentTaskRunRecord {
    if stored.state() != AgentTaskRunState::Cancelled
        || prepared.state() != AgentTaskRunState::Cancelled
    {
        return prepared;
    }
    if let Some(provenance) = stored.metadata.get("cancellation_provenance") {
        prepared
            .ensure_metadata_object()
            .insert("cancellation_provenance".to_string(), provenance.clone());
    }
    prepared
}

fn validate_identity(
    prepared: &AgentTaskRunRecord,
    stored: &AgentTaskRunRecord,
    what: &str,
) -> Result<()> {
    if prepared.run_id != stored.run_id || prepared.plan_id != stored.plan_id {
        return Err(Error::validation_invalid_argument(
            "transition",
            format!("{what} must keep the stored run and plan identity"),
            Some(stored.run_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn validate_prepared_terminal_record(
    prepared: &AgentTaskRunRecord,
    stored: &AgentTaskRunRecord,
    name: &str,
    what: &str,
) -> Result<()> {
    if prepared.run_id != stored.run_id || prepared.plan_id != stored.plan_id {
        return Err(Error::validation_invalid_argument(
            "transition",
            format!("{what} must keep the stored run and plan identity"),
            Some(stored.run_id.clone()),
            None,
        ));
    }
    if !prepared.state().is_terminal() {
        return Err(illegal_transition(
            stored,
            prepared.state(),
            name,
            &format!("{what} must commit a terminal run state"),
        ));
    }
    Ok(())
}

/// `reason_code` carried in the details of a refused transition: the commit
/// would take the run along an edge the run state graph
/// ([`AgentTaskRunState::can_transition_to`]) or the named transition does
/// not allow. Nothing was written.
pub const AGENT_TASK_ILLEGAL_TRANSITION: &str = "agent_task_illegal_transition";

/// Refuse a commit that would move `stored` to `to` along an illegal edge.
pub(crate) fn illegal_transition(
    stored: &AgentTaskRunRecord,
    to: AgentTaskRunState,
    transition: &str,
    problem: &str,
) -> Error {
    illegal_state_transition(&stored.run_id, stored.state(), to, transition, problem)
}

pub(crate) fn illegal_state_transition(
    run_id: &str,
    from: AgentTaskRunState,
    to: AgentTaskRunState,
    transition: &str,
    problem: &str,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "transition",
        problem.to_string(),
        Some(run_id.to_string()),
        None,
    );
    let extra = json!({
        "reason_code": AGENT_TASK_ILLEGAL_TRANSITION,
        "from_state": from,
        "to_state": to,
        "transition": transition,
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

/// Outcome of committing a terminal record prepared from a caller-held
/// snapshot (#15718).
#[derive(Debug, Clone)]
pub(crate) enum TerminalRecordCommit {
    /// The prepared record was committed; this is the committed copy.
    Committed(AgentTaskRunRecord),
    /// Another writer terminalized the run after the caller's snapshot was
    /// read. Nothing was written; this is the stored record that won.
    Superseded(AgentTaskRunRecord),
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

/// Whether `error` is a converted [`AgentTaskTransitionError::Conflict`]: a
/// writer committed after the caller read the record, and nothing was written.
pub(crate) fn is_transition_conflict(error: &Error) -> bool {
    error.details["reason_code"] == serde_json::json!(AGENT_TASK_TRANSITION_CONFLICT)
}

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
/// means a writer outside it (another store) committed.
pub(crate) const TRANSITION_RETRY_ATTEMPTS: usize = 3;

// ---------------------------------------------------------------------------
// State writes (#15718 step 4b).
//
// `AgentTaskRunRecord::state` and `AgentTaskRunTask::state` are private
// fields. Their only writer, `write_state`, demands a `StateWriteAuthority`,
// and only this module can mint one. Every run- and task-state change in
// production code is therefore one of the typed writers below, applied to a
// record that is then committed through the lifecycle store's fenced write
// path (`transition`, `transition_when`, `mutate_record`), which validates the
// edge against the stored state. A new direct assignment anywhere else does
// not compile.
// ---------------------------------------------------------------------------

/// Proof that a state write was issued by the transition module. The private
/// field keeps it unconstructible anywhere else.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StateWriteAuthority(());

const AUTHORITY: StateWriteAuthority = StateWriteAuthority(());

/// Single authoritative setter for a run's state. It writes both the
/// top-level run state and the derived `lifecycle.execution.state`
/// projection together, so the two cannot silently diverge.
pub(crate) fn set_run_state(record: &mut AgentTaskRunRecord, state: AgentTaskRunState) {
    record.write_state(state, AUTHORITY);
    let timestamp = record.updated_at.clone().unwrap_or_else(now_timestamp);
    record.lifecycle.execution.state = RunExecutionState::from(state);
    record.lifecycle.execution.updated_at = Some(timestamp.clone());
    if state == AgentTaskRunState::Running && record.lifecycle.execution.started_at.is_none() {
        record.lifecycle.execution.started_at = Some(timestamp.clone());
    }
    // A terminal run has finished executing, so stamp `finished_at` for the
    // canonical terminal set (`is_terminal`), matching the legacy-record
    // migration path in `health.rs`.
    if state.is_terminal() {
        record.lifecycle.execution.finished_at = Some(timestamp.clone());
    }
    record.lifecycle.updated_at = Some(timestamp);
    // Assert the invariant the record-health check enforces
    // (`ConflictingProjections`) so a future edit that breaks the pairing
    // fails fast in dev/tests rather than emitting divergent records.
    debug_assert!(
        record.run_state_projections_agree(),
        "set_run_state must leave run-state projections in agreement"
    );
}

/// Start the run: `Running`, and every task still `Queued` starts with it.
pub(crate) fn start_run(record: &mut AgentTaskRunRecord) {
    set_run_state(record, AgentTaskRunState::Running);
    for task in &mut record.tasks {
        if task.state() == AgentTaskState::Queued {
            task.write_state(AgentTaskState::Running, AUTHORITY);
        }
    }
}

/// Settle the run in `run_state`, moving every task that had not finished
/// (`Queued` or `Running`) to `task_state`. Tasks that already finished keep
/// their own outcome.
pub(crate) fn settle_run(
    record: &mut AgentTaskRunRecord,
    run_state: AgentTaskRunState,
    task_state: AgentTaskState,
) {
    set_run_state(record, run_state);
    for task in &mut record.tasks {
        if matches!(
            task.state(),
            AgentTaskState::Queued | AgentTaskState::Running
        ) {
            task.write_state(task_state, AUTHORITY);
        }
    }
}

/// Return a zero-execution run to the queue and drop its exact-step owner.
pub(crate) fn reset_for_requeue(record: &mut AgentTaskRunRecord) {
    set_run_state(record, AgentTaskRunState::Queued);
    record.lifecycle.execution.started_at = None;
    record.lifecycle.execution.finished_at = None;
    for task in &mut record.tasks {
        task.write_state(AgentTaskState::Queued, AUTHORITY);
    }
    let metadata = record.ensure_metadata_object();
    for key in [
        "execution_step_owner",
        "runner_pid",
        "runner_process_start_identity",
        "runner_started_at",
    ] {
        metadata.remove(key);
    }
}

/// Project a task's state from durable aggregate evidence (its latest event or
/// its outcome) while the run's task list is rebuilt from that aggregate.
pub(crate) fn project_task_state(task: &mut AgentTaskRunTask, state: AgentTaskState) {
    task.write_state(state, AUTHORITY);
}

/// A read-time copy of `record` presented in `state`, for observation
/// surfaces whose derived state differs from the stored one (for example a
/// provider-succeeded Cook that never finished end to end). Only the
/// top-level state is replaced, exactly as observed; the copy is a view and
/// is never handed back to the store.
pub(crate) fn observed_with_run_state(
    record: &AgentTaskRunRecord,
    state: AgentTaskRunState,
) -> AgentTaskRunRecord {
    let mut observed = record.clone();
    observed.write_state(state, AUTHORITY);
    observed
}
