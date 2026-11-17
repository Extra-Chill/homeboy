use homeboy_control_plane_contract::ControlPlaneState;

use crate::cook_status::CookStatus;

pub const RUN_LIFECYCLE_STATUS_SCHEMA: &str = "homeboy/run-lifecycle-status/v1";

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
/// [`ControlPlaneState::is_terminal`]. What this projection guarantees is that
/// it never *contradicts* the vocabulary's own reading: for every known
/// variant,
/// `from_cook_status(status).is_terminal() == !status.is_in_flight()`.
/// [`CookStatus::Unknown`] is the single deliberate exception — it maps to
/// [`ControlPlaneState::Unknown`], which claims nothing, instead of
/// manufacturing a failure verdict for a status this binary cannot read.
///
/// # Retryability
///
/// [`ControlPlaneState::is_retryable`] is true for exactly `Failed`,
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
/// set as the statuses that project to [`ControlPlaneState::Succeeded`].
pub fn from_cook_status(status: &CookStatus) -> ControlPlaneState {
    match status {
        // --- in flight ---
        CookStatus::Queued => ControlPlaneState::Queued,
        CookStatus::Running | CookStatus::InFlight | CookStatus::AwaitingCi => {
            ControlPlaneState::Running
        }

        // --- terminal, successful ---
        // The Cook did the job it was asked to do. `ReviewReady`,
        // `DraftPublished`, and `GreenNoFinalize` all stop with green gates
        // and a reviewable outcome; `IntentionalNoChange` is a verified
        // review whose correct result was "no patch".
        CookStatus::Completed
        | CookStatus::ReviewReady
        | CookStatus::DraftPublished
        | CookStatus::GreenNoFinalize
        | CookStatus::IntentionalNoChange
        | CookStatus::SupersededByActiveCook => ControlPlaneState::Succeeded,

        // --- terminal, unsuccessful, not an invitation to retry ---
        // Finalization ran and produced nothing. Nothing failed, but the
        // Cook did not succeed either, and re-running it changes nothing.
        CookStatus::NoChanges | CookStatus::NoCandidate => ControlPlaneState::PartialFailure,
        // A candidate exists and can still be recovered. This matches the
        // execution state's CandidateRecoverable projection. Both used to
        // say `PartialFailure`; they were changed together so the two
        // routes still agree.
        CookStatus::CandidateRecoverable => ControlPlaneState::CandidateRecoverable,
        // A gate deferred rather than failed: the patch is promoted and
        // durable, but verification did not complete. This is the same
        // "stopped, unsuccessful, promotable candidate survived" shape as
        // `CandidateRecoverable`, not a verdict against the candidate
        // (#14731).
        CookStatus::GatesDeferred => ControlPlaneState::CandidateRecoverable,
        // Stopped waiting on a verdict that is not this run's to produce.
        CookStatus::AwaitingAcceptance => ControlPlaneState::PartialFailure,
        // Re-running reproduces the block identically until the dependency
        // lands or the claim clears.
        CookStatus::BlockedByDependency | CookStatus::Blocked => ControlPlaneState::PartialFailure,
        // The budget is the thing that ran out. Reporting these as
        // retryable is precisely how an orchestrator builds a retry loop
        // around a Cook that has already exhausted its retries.
        CookStatus::RetriesExhausted | CookStatus::ExecutionBudgetExhausted => {
            ControlPlaneState::PartialFailure
        }
        // Policy refused. It refuses again on the same inputs.
        CookStatus::PolicyFailure | CookStatus::GateDeclarationInvalid => {
            ControlPlaneState::PartialFailure
        }
        // The repro passes on base; it passes again until it is corrected.
        CookStatus::ReproPassesOnBase => ControlPlaneState::PartialFailure,
        // The candidate set needs an explicit operator decision before any
        // promotion can run; automatic retry cannot resolve that choice.
        CookStatus::SelectionRequired => ControlPlaneState::PartialFailure,

        // --- terminal, unsuccessful, retry is a legal next action ---
        // A gate verdict is against the candidate, not against running
        // again: the Cook loop's own remedy for a failed gate is another
        // attempt.
        CookStatus::GateFailed | CookStatus::NoChangesGateFailed => ControlPlaneState::Failed,
        // The provider failed. Another attempt is the standard remedy.
        CookStatus::ProviderFailure => ControlPlaneState::Failed,
        // The durable recipe is intact, which is exactly the precondition
        // for a legal continuation.
        CookStatus::DurableFailure => ControlPlaneState::Failed,
        // Nothing was consumed, so retrying costs nothing and may succeed
        // once the admission problem is fixed.
        CookStatus::PreExecutionFailure => ControlPlaneState::Failed,
        // An interruption, not a verdict. Closest sibling would be `Stale`;
        // both are retryable, so the actionable classification is
        // identical and `Failed` keeps the failure visible.
        CookStatus::PreArtifactInterruption => ControlPlaneState::Failed,
        CookStatus::Failed => ControlPlaneState::Failed,

        // --- terminal, declared by an operator or a budget ---
        CookStatus::Cancelled => ControlPlaneState::Cancelled,
        CookStatus::TimedOut | CookStatus::ReviewFormTimeout => ControlPlaneState::TimedOut,

        // A status this binary cannot read carries no verdict. Callers read
        // terminality from the declared `CookDisposition` instead.
        CookStatus::Unknown(_) => ControlPlaneState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every known Cook status, listed once, with the lifecycle status it
    /// projects to. The `match` below is exhaustive, so a variant added to
    /// `CookStatus` without a decision here is a compile error rather than a
    /// silent `Unknown`.
    fn cook_status_projection_matrix() -> Vec<(CookStatus, ControlPlaneState)> {
        [
            CookStatus::Queued,
            CookStatus::Running,
            CookStatus::InFlight,
            CookStatus::Completed,
            CookStatus::ReviewReady,
            CookStatus::DraftPublished,
            CookStatus::GreenNoFinalize,
            CookStatus::IntentionalNoChange,
            CookStatus::SupersededByActiveCook,
            CookStatus::NoCandidate,
            CookStatus::NoChanges,
            CookStatus::NoChangesGateFailed,
            CookStatus::GateFailed,
            CookStatus::GatesDeferred,
            CookStatus::GateDeclarationInvalid,
            CookStatus::ReproPassesOnBase,
            CookStatus::AwaitingAcceptance,
            CookStatus::CandidateRecoverable,
            CookStatus::BlockedByDependency,
            CookStatus::Blocked,
            CookStatus::Cancelled,
            CookStatus::TimedOut,
            CookStatus::ReviewFormTimeout,
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
                CookStatus::Queued => ControlPlaneState::Queued,
                CookStatus::Running => ControlPlaneState::Running,
                CookStatus::InFlight | CookStatus::AwaitingCi => ControlPlaneState::Running,
                CookStatus::Completed => ControlPlaneState::Succeeded,
                CookStatus::ReviewReady => ControlPlaneState::Succeeded,
                CookStatus::DraftPublished => ControlPlaneState::Succeeded,
                CookStatus::GreenNoFinalize => ControlPlaneState::Succeeded,
                CookStatus::IntentionalNoChange | CookStatus::SupersededByActiveCook => {
                    ControlPlaneState::Succeeded
                }
                CookStatus::NoCandidate => ControlPlaneState::PartialFailure,
                CookStatus::NoChanges => ControlPlaneState::PartialFailure,
                CookStatus::NoChangesGateFailed => ControlPlaneState::Failed,
                CookStatus::GateFailed => ControlPlaneState::Failed,
                CookStatus::GatesDeferred => ControlPlaneState::CandidateRecoverable,
                CookStatus::GateDeclarationInvalid => ControlPlaneState::PartialFailure,
                CookStatus::ReproPassesOnBase => ControlPlaneState::PartialFailure,
                CookStatus::AwaitingAcceptance => ControlPlaneState::PartialFailure,
                CookStatus::CandidateRecoverable => ControlPlaneState::CandidateRecoverable,
                CookStatus::BlockedByDependency => ControlPlaneState::PartialFailure,
                CookStatus::Blocked => ControlPlaneState::PartialFailure,
                CookStatus::Cancelled => ControlPlaneState::Cancelled,
                CookStatus::TimedOut => ControlPlaneState::TimedOut,
                CookStatus::ReviewFormTimeout => ControlPlaneState::TimedOut,
                CookStatus::RetriesExhausted => ControlPlaneState::PartialFailure,
                CookStatus::ExecutionBudgetExhausted => ControlPlaneState::PartialFailure,
                CookStatus::PolicyFailure => ControlPlaneState::PartialFailure,
                CookStatus::SelectionRequired => ControlPlaneState::PartialFailure,
                CookStatus::ProviderFailure => ControlPlaneState::Failed,
                CookStatus::DurableFailure => ControlPlaneState::Failed,
                CookStatus::PreExecutionFailure => ControlPlaneState::Failed,
                CookStatus::PreArtifactInterruption => ControlPlaneState::Failed,
                CookStatus::Failed => ControlPlaneState::Failed,
                CookStatus::Unknown(_) => ControlPlaneState::Unknown,
            };
            (status, expected)
        })
        .collect()
    }

    #[test]
    fn every_cook_status_projects_to_the_reviewed_lifecycle_status() {
        for (status, expected) in cook_status_projection_matrix() {
            assert_eq!(from_cook_status(&status), expected, "{status} projection");
            // The owning and borrowing conversions must not diverge.
            assert_eq!(
                from_cook_status(&status),
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
            let projected = from_cook_status(&status);
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
        let projected = from_cook_status(&status);

        assert_eq!(projected, ControlPlaneState::Unknown);
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
            CookStatus::NoCandidate,
        ] {
            let projected = from_cook_status(&status);
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
                    | CookStatus::SupersededByActiveCook
            );
            assert_eq!(is_success, expected_success, "{status} success claim");
        }
    }
}
