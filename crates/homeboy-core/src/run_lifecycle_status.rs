//! Canonical run lifecycle status and the Cook report vocabulary's projection
//! onto it.
//!
//! Unknown labels carry no verdict. Terminality inside Cook remains the
//! producing exit's declared `CookDisposition`, not an inference from a label.

pub use homeboy_control_plane_contract::ControlPlaneState;

pub const RUN_LIFECYCLE_STATUS_SCHEMA: &str = "homeboy/run-lifecycle-status/v1";

/// Canonical run lifecycle status. Every Cook vocabulary converts into it
/// through `From` (`RunLifecycleStatus::from(&cook_status)`), so no caller
/// keeps its own copy of the Cook-to-lifecycle mapping.
pub type RunLifecycleStatus = ControlPlaneState;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cook_status::{CookStatus, GateVerdict};

    #[test]
    fn every_cook_status_round_trips_and_projects_to_the_reviewed_lifecycle_status() {
        use ControlPlaneState as State;
        use CookStatus::*;
        for (status, label, expected) in [
            (Queued, "queued", State::Queued),
            (InFlight, "in_flight", State::Running),
            (AwaitingCi, "awaiting_ci", State::Running),
            (ReviewReady, "review_ready", State::Succeeded),
            (DraftPublished, "draft_published", State::Succeeded),
            (SucceededPartial, "succeeded_partial", State::Succeeded),
            (GreenNoFinalize, "green_no_finalize", State::Succeeded),
            (
                IntentionalNoChange,
                "intentional_no_change",
                State::Succeeded,
            ),
            (
                SupersededByActiveCook,
                "superseded_by_active_cook",
                State::Succeeded,
            ),
            (Validated, "validated", State::PartialFailure),
            (NoCandidate, "no_candidate", State::PartialFailure),
            (
                Gate(GateVerdict::NoChanges),
                "no_changes",
                State::PartialFailure,
            ),
            (
                Gate(GateVerdict::NoChangesGateFailed),
                "no_changes_gate_failed",
                State::Failed,
            ),
            (Gate(GateVerdict::GateFailed), "gate_failed", State::Failed),
            (
                Gate(GateVerdict::GateEnvironmentUnavailable),
                "gate_environment_unavailable",
                State::CandidateRecoverable,
            ),
            (BaselineRed, "baseline_red", State::PartialFailure),
            (FinalizationFailed, "finalization_failed", State::Failed),
            (
                GateDeclarationInvalid,
                "gate_declaration_invalid",
                State::PartialFailure,
            ),
            (
                ReproPassesOnBase,
                "repro_passes_on_base",
                State::PartialFailure,
            ),
            (
                AwaitingAcceptance,
                "awaiting_acceptance",
                State::PartialFailure,
            ),
            (
                CandidateRecoverable,
                "candidate_recoverable",
                State::CandidateRecoverable,
            ),
            (Blocked, "blocked", State::PartialFailure),
            (Cancelled, "cancelled", State::Cancelled),
            (TimedOut, "timed_out", State::TimedOut),
            (RetriesExhausted, "retries_exhausted", State::PartialFailure),
            (
                ExecutionBudgetExhausted,
                "execution_budget_exhausted",
                State::PartialFailure,
            ),
            (PolicyFailure, "policy_failure", State::PartialFailure),
            (
                SelectionRequired,
                "selection_required",
                State::PartialFailure,
            ),
            (ProviderFailure, "provider_failure", State::Failed),
            (DurableFailure, "durable_failure", State::Failed),
            (PreExecutionFailure, "pre_execution_failure", State::Failed),
            (
                PreArtifactInterruption,
                "pre_artifact_interruption",
                State::Failed,
            ),
            (Failed, "failed", State::Failed),
        ] {
            assert_eq!(status.as_str(), label);
            assert_eq!(CookStatus::from_status(label), Some(status));
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(serde_json::from_str::<CookStatus>(&json).unwrap(), status);
            assert_eq!(RunLifecycleStatus::from(&status), expected, "{label}");
            assert_eq!(
                RunLifecycleStatus::from(status.clone()),
                expected,
                "{label}"
            );
            assert_eq!(expected.is_terminal(), !status.is_in_flight(), "{label}");
        }
    }

    #[test]
    fn stored_gates_deferred_reads_as_gate_environment_unavailable() {
        let status: CookStatus =
            serde_json::from_value(serde_json::json!("gates_deferred")).unwrap();
        assert_eq!(
            status,
            CookStatus::Gate(GateVerdict::GateEnvironmentUnavailable)
        );
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            "gate_environment_unavailable"
        );
    }

    #[test]
    fn retired_and_future_labels_are_rejected() {
        for label in [
            "running",
            "completed",
            "blocked_by_dependency",
            "review_form_timeout",
            "moving_base_from_a_newer_binary",
        ] {
            assert_eq!(CookStatus::from_status(label), None);
            assert!(serde_json::from_value::<CookStatus>(serde_json::json!(label)).is_err());
        }
    }
}
