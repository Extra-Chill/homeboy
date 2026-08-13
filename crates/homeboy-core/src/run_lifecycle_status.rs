use serde::{Deserialize, Serialize};

use crate::api_jobs::JobStatus;
use crate::cook_status::CookStatus;
use crate::run_lifecycle_record::RunExecutionState;

pub const RUN_LIFECYCLE_STATUS_SCHEMA: &str = "homeboy/run-lifecycle-status/v1";

/// Canonical run lifecycle status vocabulary for cross-runtime contracts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RunLifecycleStatus {
    Unknown,
    Queued,
    Running,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
    TimedOut,
    Stale,
}

impl RunLifecycleStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::PartialFailure
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::Stale
        )
    }

    pub fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }

    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Failed | Self::TimedOut | Self::Stale)
    }
}

impl From<RunExecutionState> for RunLifecycleStatus {
    fn from(state: RunExecutionState) -> Self {
        match state {
            RunExecutionState::Unknown => Self::Unknown,
            RunExecutionState::Queued => Self::Queued,
            RunExecutionState::Running => Self::Running,
            RunExecutionState::Succeeded => Self::Succeeded,
            RunExecutionState::PartialFailure => Self::PartialFailure,
            RunExecutionState::Failed => Self::Failed,
            RunExecutionState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<JobStatus> for RunLifecycleStatus {
    fn from(status: JobStatus) -> Self {
        match status {
            JobStatus::Queued => Self::Queued,
            JobStatus::Running => Self::Running,
            JobStatus::Succeeded => Self::Succeeded,
            JobStatus::Failed => Self::Failed,
            JobStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// Project the open Cook status vocabulary onto the closed lifecycle one.
///
/// # Why this exists
///
/// [`CookStatus`] is deliberately open: several Cook exits feed it straight
/// from `finalization["status"]`, and [`CookStatus::Unknown`] preserves any
/// value this binary has not been taught. That is correct for *reporting* and
/// useless for *classification* — a consumer holding `{"status":
/// "moving_base"}` cannot decide success, terminality, or retry without a
/// process exit code it may not have (a persisted report, an HTTP response, a
/// chat-plane message). This projection is the classification, emitted
/// alongside the raw status rather than replacing it.
///
/// # This is not a new opinion about terminality
///
/// [`CookDisposition`](crate::cook_status::CookDisposition) remains the
/// authority on whether a Cook will advance on its own, because the exit that
/// produced the report declares it. Reports therefore emit `terminal` from the
/// declared disposition, not from
/// [`RunLifecycleStatus::is_terminal`]. What this projection guarantees is that
/// it never *contradicts* the vocabulary's own reading: for every known
/// variant,
/// `RunLifecycleStatus::from(status).is_terminal() == !status.is_in_flight()`.
/// [`CookStatus::Unknown`] is the single deliberate exception — it maps to
/// [`RunLifecycleStatus::Unknown`], which claims nothing, instead of
/// manufacturing a failure verdict for a status this binary cannot read.
///
/// # Retryability
///
/// [`RunLifecycleStatus::is_retryable`] is true for exactly `Failed`,
/// `TimedOut`, and `Stale`. Mapping a Cook status to `Failed` therefore *tells
/// an orchestrator to retry*. Statuses whose defining property is that
/// re-running immediately reproduces them — retries exhausted, budget
/// exhausted, policy refusal, an unmet dependency, a held claim — map to
/// `PartialFailure` instead: terminal, unsuccessful, and not an invitation to
/// loop. `PartialFailure` is the only closed-vocabulary slot with those three
/// properties; it is used as "stopped, needs an operator, do not auto-retry"
/// rather than implying partial work.
///
/// Note that [`CookStatus::is_success_exit`] is a *different* question — it is
/// the exit-code rule, and it is true for in-flight statuses (a Cook that is
/// still running is not a failed command). It is intentionally not the same
/// set as the statuses that project to [`RunLifecycleStatus::Succeeded`].
impl From<&CookStatus> for RunLifecycleStatus {
    fn from(status: &CookStatus) -> Self {
        match status {
            // --- in flight ---
            CookStatus::Queued => Self::Queued,
            CookStatus::Running | CookStatus::InFlight => Self::Running,

            // --- terminal, successful ---
            // The Cook did the job it was asked to do. `ReviewReady`,
            // `DraftPublished`, and `GreenNoFinalize` all stop with green gates
            // and a reviewable outcome; `IntentionalNoChange` is a verified
            // review whose correct result was "no patch".
            CookStatus::Completed
            | CookStatus::ReviewReady
            | CookStatus::DraftPublished
            | CookStatus::GreenNoFinalize
            | CookStatus::IntentionalNoChange => Self::Succeeded,

            // --- terminal, unsuccessful, not an invitation to retry ---
            // Finalization ran and produced nothing. Nothing failed, but the
            // Cook did not succeed either, and re-running it changes nothing.
            CookStatus::NoChanges => Self::PartialFailure,
            // A candidate exists and can still be recovered. This matches the
            // existing `AgentTaskRunState::CandidateRecoverable ->
            // RunExecutionState::PartialFailure` projection rather than
            // inventing a second reading of the same situation.
            CookStatus::CandidateRecoverable => Self::PartialFailure,
            // Stopped waiting on a verdict that is not this run's to produce.
            CookStatus::AwaitingAcceptance => Self::PartialFailure,
            // Re-running reproduces the block identically until the dependency
            // lands or the claim clears.
            CookStatus::BlockedByDependency | CookStatus::Blocked => Self::PartialFailure,
            // The budget is the thing that ran out. Reporting these as
            // retryable is precisely how an orchestrator builds a retry loop
            // around a Cook that has already exhausted its retries.
            CookStatus::RetriesExhausted | CookStatus::ExecutionBudgetExhausted => {
                Self::PartialFailure
            }
            // Policy refused. It refuses again on the same inputs.
            CookStatus::PolicyFailure => Self::PartialFailure,
            // The candidate set needs an explicit operator decision before any
            // promotion can run; automatic retry cannot resolve that choice.
            CookStatus::SelectionRequired => Self::PartialFailure,

            // --- terminal, unsuccessful, retry is a legal next action ---
            // A gate verdict is against the candidate, not against running
            // again: the Cook loop's own remedy for a failed gate is another
            // attempt.
            CookStatus::GateFailed | CookStatus::NoOpGateFailed => Self::Failed,
            // The provider failed. Another attempt is the standard remedy.
            CookStatus::ProviderFailure => Self::Failed,
            // The durable recipe is intact, which is exactly the precondition
            // for a legal continuation.
            CookStatus::DurableFailure => Self::Failed,
            // Nothing was consumed, so retrying costs nothing and may succeed
            // once the admission problem is fixed.
            CookStatus::PreExecutionFailure => Self::Failed,
            // An interruption, not a verdict. Closest sibling would be `Stale`;
            // both are retryable, so the actionable classification is
            // identical and `Failed` keeps the failure visible.
            CookStatus::PreArtifactInterruption => Self::Failed,
            CookStatus::Failed => Self::Failed,

            // --- terminal, declared by an operator or a budget ---
            CookStatus::Cancelled => Self::Cancelled,
            CookStatus::TimedOut => Self::TimedOut,

            // A status this binary cannot read carries no verdict. Callers read
            // terminality from the declared `CookDisposition` instead.
            CookStatus::Unknown(_) => Self::Unknown,
        }
    }
}

impl From<CookStatus> for RunLifecycleStatus {
    fn from(status: CookStatus) -> Self {
        Self::from(&status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification_covers_terminal_success_and_retryable_sets() {
        let expectations = [
            (RunLifecycleStatus::Unknown, false, false, false),
            (RunLifecycleStatus::Queued, false, false, false),
            (RunLifecycleStatus::Running, false, false, false),
            (RunLifecycleStatus::Succeeded, true, true, false),
            (RunLifecycleStatus::PartialFailure, true, false, false),
            (RunLifecycleStatus::Failed, true, false, true),
            (RunLifecycleStatus::Cancelled, true, false, false),
            (RunLifecycleStatus::TimedOut, true, false, true),
            (RunLifecycleStatus::Stale, true, false, true),
        ];

        for (status, terminal, success, retryable) in expectations {
            assert_eq!(status.is_terminal(), terminal, "{status:?} terminal");
            assert_eq!(status.is_success(), success, "{status:?} success");
            assert_eq!(status.is_retryable(), retryable, "{status:?} retryable");
        }
    }

    #[test]
    fn status_serializes_as_snake_case_contract_value() {
        let value = serde_json::to_value(RunLifecycleStatus::PartialFailure).expect("serialize");

        assert_eq!(value, serde_json::json!("partial_failure"));
    }

    #[test]
    fn source_states_project_to_canonical_status() {
        for label in "unknown,queued,running,succeeded,partial_failure,failed,cancelled".split(',')
        {
            let source: RunExecutionState = serde_json::from_value(label.into()).unwrap();
            let expected: RunLifecycleStatus = serde_json::from_value(label.into()).unwrap();
            assert_eq!(RunLifecycleStatus::from(source), expected, "{source:?}");
        }
        for label in "queued,running,succeeded,failed,cancelled".split(',') {
            let source: JobStatus = serde_json::from_value(label.into()).unwrap();
            let expected: RunLifecycleStatus = serde_json::from_value(label.into()).unwrap();
            assert_eq!(RunLifecycleStatus::from(source), expected, "{source:?}");
        }
    }

    /// Every known Cook status, listed once, with the lifecycle status it
    /// projects to. The `match` below is exhaustive, so a variant added to
    /// `CookStatus` without a decision here is a compile error rather than a
    /// silent `Unknown`.
    fn cook_status_projection_matrix() -> Vec<(CookStatus, RunLifecycleStatus)> {
        [
            CookStatus::Queued,
            CookStatus::Running,
            CookStatus::InFlight,
            CookStatus::Completed,
            CookStatus::ReviewReady,
            CookStatus::DraftPublished,
            CookStatus::GreenNoFinalize,
            CookStatus::IntentionalNoChange,
            CookStatus::NoChanges,
            CookStatus::NoOpGateFailed,
            CookStatus::GateFailed,
            CookStatus::AwaitingAcceptance,
            CookStatus::CandidateRecoverable,
            CookStatus::BlockedByDependency,
            CookStatus::Blocked,
            CookStatus::Cancelled,
            CookStatus::TimedOut,
            CookStatus::RetriesExhausted,
            CookStatus::ExecutionBudgetExhausted,
            CookStatus::PolicyFailure,
            CookStatus::SelectionRequired,
            CookStatus::ProviderFailure,
            CookStatus::DurableFailure,
            CookStatus::PreExecutionFailure,
            CookStatus::PreArtifactInterruption,
            CookStatus::Failed,
        ]
        .into_iter()
        .map(|status| {
            // Written out independently of the `From` impl on purpose: this is
            // the reviewed table, not a restatement of the implementation.
            let expected = match &status {
                CookStatus::Queued => RunLifecycleStatus::Queued,
                CookStatus::Running => RunLifecycleStatus::Running,
                CookStatus::InFlight => RunLifecycleStatus::Running,
                CookStatus::Completed => RunLifecycleStatus::Succeeded,
                CookStatus::ReviewReady => RunLifecycleStatus::Succeeded,
                CookStatus::DraftPublished => RunLifecycleStatus::Succeeded,
                CookStatus::GreenNoFinalize => RunLifecycleStatus::Succeeded,
                CookStatus::IntentionalNoChange => RunLifecycleStatus::Succeeded,
                CookStatus::NoChanges => RunLifecycleStatus::PartialFailure,
                CookStatus::NoOpGateFailed => RunLifecycleStatus::Failed,
                CookStatus::GateFailed => RunLifecycleStatus::Failed,
                CookStatus::AwaitingAcceptance => RunLifecycleStatus::PartialFailure,
                CookStatus::CandidateRecoverable => RunLifecycleStatus::PartialFailure,
                CookStatus::BlockedByDependency => RunLifecycleStatus::PartialFailure,
                CookStatus::Blocked => RunLifecycleStatus::PartialFailure,
                CookStatus::Cancelled => RunLifecycleStatus::Cancelled,
                CookStatus::TimedOut => RunLifecycleStatus::TimedOut,
                CookStatus::RetriesExhausted => RunLifecycleStatus::PartialFailure,
                CookStatus::ExecutionBudgetExhausted => RunLifecycleStatus::PartialFailure,
                CookStatus::PolicyFailure => RunLifecycleStatus::PartialFailure,
                CookStatus::SelectionRequired => RunLifecycleStatus::PartialFailure,
                CookStatus::ProviderFailure => RunLifecycleStatus::Failed,
                CookStatus::DurableFailure => RunLifecycleStatus::Failed,
                CookStatus::PreExecutionFailure => RunLifecycleStatus::Failed,
                CookStatus::PreArtifactInterruption => RunLifecycleStatus::Failed,
                CookStatus::Failed => RunLifecycleStatus::Failed,
                CookStatus::Unknown(_) => RunLifecycleStatus::Unknown,
            };
            (status, expected)
        })
        .collect()
    }

    #[test]
    fn every_cook_status_projects_to_the_reviewed_lifecycle_status() {
        for (status, expected) in cook_status_projection_matrix() {
            assert_eq!(
                RunLifecycleStatus::from(&status),
                expected,
                "{status} projection"
            );
            // The owning and borrowing conversions must not diverge.
            assert_eq!(
                RunLifecycleStatus::from(status.clone()),
                expected,
                "{status} owned projection"
            );
        }
    }

    /// The projection must not become a second opinion about terminality.
    /// `CookDisposition` is the authority; this asserts the projection agrees
    /// with the vocabulary's own closed in-flight reading for every known
    /// status, so the two can never contradict each other on the wire.
    #[test]
    fn the_projection_agrees_with_the_cook_vocabularys_in_flight_reading() {
        for (status, _) in cook_status_projection_matrix() {
            let projected = RunLifecycleStatus::from(&status);
            assert_eq!(
                projected.is_terminal(),
                !status.is_in_flight(),
                "{status} terminality must match CookStatus::is_in_flight"
            );
        }
    }

    /// The one deliberate exception, pinned so it cannot be "fixed" into a
    /// fabricated failure verdict. An unreadable status claims nothing; the
    /// report's declared `CookDisposition` is what tells a consumer the Cook
    /// stopped.
    #[test]
    fn an_unknown_cook_status_projects_to_unknown_rather_than_a_verdict() {
        let status = CookStatus::from_status("moving_base_from_a_newer_binary");
        let projected = RunLifecycleStatus::from(&status);

        assert_eq!(projected, RunLifecycleStatus::Unknown);
        assert!(!projected.is_success());
        assert!(!projected.is_retryable());
        assert!(!status.is_in_flight());
    }

    /// Retryability is an instruction, so the statuses whose defining property
    /// is that re-running reproduces them must never carry it.
    #[test]
    fn exhausted_blocked_and_refused_statuses_are_never_reported_retryable() {
        for status in [
            CookStatus::RetriesExhausted,
            CookStatus::ExecutionBudgetExhausted,
            CookStatus::PolicyFailure,
            CookStatus::BlockedByDependency,
            CookStatus::Blocked,
            CookStatus::Cancelled,
            CookStatus::NoChanges,
        ] {
            let projected = RunLifecycleStatus::from(&status);
            assert!(projected.is_terminal(), "{status} must be terminal");
            assert!(
                !projected.is_retryable(),
                "{status} must not instruct a retry"
            );
        }
    }

    /// Only exits that carry a real, reviewable outcome may claim success.
    #[test]
    fn only_completed_and_reviewable_exits_project_to_succeeded() {
        for (status, expected) in cook_status_projection_matrix() {
            let is_success = expected.is_success();
            let expected_success = matches!(
                &status,
                CookStatus::Completed
                    | CookStatus::ReviewReady
                    | CookStatus::DraftPublished
                    | CookStatus::GreenNoFinalize
                    | CookStatus::IntentionalNoChange
            );
            assert_eq!(is_success, expected_success, "{status} success claim");
        }
    }
}
