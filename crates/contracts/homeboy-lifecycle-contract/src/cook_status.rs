//! The Cook status vocabulary and the single classification of terminality.
//!
//! # Why this exists
//!
//! Cook status was an untyped `String` classified by three independent,
//! hand-maintained lists inside `agent_task_service/cook.rs`:
//!
//! | purpose | vocabulary it recognized |
//! |---|---|
//! | batch totals | `queued`, `running`/`in_flight`, `cancelled`, `timed_out` |
//! | exit code `0` | `queued`, `running`, `in_flight`, `review_ready`, `green_no_finalize` |
//! | terminality | everything except `queued`, `running`, `in_flight` |
//!
//! Three lists over one open vocabulary have to agree by hand, and they did
//! not: `cancelled` and `timed_out` were known only to the batch list, and
//! `review_ready`/`green_no_finalize` only to the exit-code list. This module
//! is the one place the vocabulary is written down, so the three call sites
//! classify against the same enum instead of three divergent string literals.
//!
//! # Why unknown statuses stay terminal
//!
//! [`CookStatus::is_terminal`] is defined as "not one of the in-flight
//! states", deliberately keeping the *in-flight* set closed rather than the
//! terminal set.
//!
//! The in-flight set is small, internal, and owned by the Cook loop itself.
//! The terminal set is open: three call sites feed it straight from
//! `finalization["status"]`, an arbitrary JSON string, defaulting to
//! `"unknown"` when the field is missing. Making the *terminal* side the
//! allow-list would mean any status this binary has not been taught is treated
//! as still-running — so a Cook that genuinely finished would never emit its
//! terminal notification and the orchestrator would wait forever. Because new
//! statuses overwhelmingly arrive from finalization, and finalization statuses
//! are overwhelmingly terminal, the safer default for an unrecognized status
//! is "the Cook has stopped".
//!
//! [`CookStatus::Unknown`] preserves the raw string so this classification
//! never rewrites a status it did not recognize.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A reported Cook status.
///
/// Serializes as the bare snake_case string it was parsed from, so the
/// `homeboy/agent-task-cook/v1` wire format is unchanged and unknown values
/// round-trip byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookStatus {
    // --- in flight: the Cook will advance on its own ---
    /// Accepted, not yet started.
    Queued,
    /// Executing.
    Running,
    /// Executing, reported from a continuation or a detached provider.
    InFlight,

    // --- terminal: the Cook will not advance without external action ---
    /// Finished and finalized.
    Completed,
    /// Finished with a candidate ready for review.
    ReviewReady,
    /// Gates were green but finalization was intentionally not performed.
    GreenNoFinalize,
    /// Finalization ran and found no changed files.
    NoChanges,
    /// A no-op candidate failed its gates.
    NoOpGateFailed,
    /// A deterministic gate failed.
    GateFailed,
    /// Waiting on an independent acceptance verdict.
    AwaitingAcceptance,
    /// Stopped, but the candidate can still be recovered.
    CandidateRecoverable,
    /// Stopped because a dependency was not satisfied.
    BlockedByDependency,
    /// Stopped by a blocking claim.
    Blocked,
    /// Cancelled by an operator or a supervisor.
    Cancelled,
    /// Exceeded its wall-clock budget.
    TimedOut,
    /// Ran out of retries.
    RetriesExhausted,
    /// Ran out of execution budget.
    ExecutionBudgetExhausted,
    /// Stopped by policy.
    PolicyFailure,
    /// The provider failed.
    ProviderFailure,
    /// Failed with a durable recipe intact.
    DurableFailure,
    /// Failed before execution began.
    PreExecutionFailure,
    /// Interrupted before artifacts were captured.
    PreArtifactInterruption,
    /// Failed without a more specific classification.
    Failed,

    /// A status this binary does not know.
    ///
    /// Classified as terminal — see the module docs. The raw string is kept so
    /// serialization is lossless.
    Unknown(String),
}

impl CookStatus {
    /// Parse a reported status. Never fails; unrecognized input becomes
    /// [`CookStatus::Unknown`].
    pub fn from_status(status: &str) -> Self {
        match status {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "in_flight" => Self::InFlight,
            "completed" => Self::Completed,
            "review_ready" => Self::ReviewReady,
            "green_no_finalize" => Self::GreenNoFinalize,
            "no_changes" => Self::NoChanges,
            "no_op_gate_failed" => Self::NoOpGateFailed,
            "gate_failed" => Self::GateFailed,
            "awaiting_acceptance" => Self::AwaitingAcceptance,
            "candidate_recoverable" => Self::CandidateRecoverable,
            "blocked_by_dependency" => Self::BlockedByDependency,
            "blocked" => Self::Blocked,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "retries_exhausted" => Self::RetriesExhausted,
            "execution_budget_exhausted" => Self::ExecutionBudgetExhausted,
            "policy_failure" => Self::PolicyFailure,
            "provider_failure" => Self::ProviderFailure,
            "durable_failure" => Self::DurableFailure,
            "pre_execution_failure" => Self::PreExecutionFailure,
            "pre_artifact_interruption" => Self::PreArtifactInterruption,
            "failed" => Self::Failed,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The wire representation.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::InFlight => "in_flight",
            Self::Completed => "completed",
            Self::ReviewReady => "review_ready",
            Self::GreenNoFinalize => "green_no_finalize",
            Self::NoChanges => "no_changes",
            Self::NoOpGateFailed => "no_op_gate_failed",
            Self::GateFailed => "gate_failed",
            Self::AwaitingAcceptance => "awaiting_acceptance",
            Self::CandidateRecoverable => "candidate_recoverable",
            Self::BlockedByDependency => "blocked_by_dependency",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::RetriesExhausted => "retries_exhausted",
            Self::ExecutionBudgetExhausted => "execution_budget_exhausted",
            Self::PolicyFailure => "policy_failure",
            Self::ProviderFailure => "provider_failure",
            Self::DurableFailure => "durable_failure",
            Self::PreExecutionFailure => "pre_execution_failure",
            Self::PreArtifactInterruption => "pre_artifact_interruption",
            Self::Failed => "failed",
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    /// Whether the Cook will advance on its own.
    ///
    /// This is the closed set. Everything else — including
    /// [`CookStatus::Unknown`] — is terminal.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::InFlight)
    }

    /// Whether the Cook will not advance without external action.
    pub fn is_terminal(&self) -> bool {
        !self.is_in_flight()
    }

    /// Whether this status alone means the operator has nothing to act on.
    ///
    /// Callers that also inspect the finalization payload should treat this as
    /// the status-only half of that decision.
    pub fn is_success_exit(&self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Running
                | Self::InFlight
                | Self::ReviewReady
                | Self::GreenNoFinalize
        )
    }
}

impl fmt::Display for CookStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for CookStatus {
    fn from(value: &str) -> Self {
        Self::from_status(value)
    }
}

impl Serialize for CookStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CookStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_status(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_in_flight_states_are_non_terminal() {
        for status in ["queued", "running", "in_flight"] {
            assert!(
                !CookStatus::from_status(status).is_terminal(),
                "{status} must not be terminal"
            );
        }
        for status in [
            "completed",
            "review_ready",
            "green_no_finalize",
            "no_changes",
            "gate_failed",
            "awaiting_acceptance",
            "cancelled",
            "timed_out",
            "durable_failure",
        ] {
            assert!(
                CookStatus::from_status(status).is_terminal(),
                "{status} must be terminal"
            );
        }
    }

    /// A Cook that finished under a status this binary predates must still
    /// emit its terminal notification, or the orchestrator waits forever.
    #[test]
    fn an_unknown_status_is_terminal() {
        let status = CookStatus::from_status("some_status_from_a_newer_binary");
        assert!(matches!(status, CookStatus::Unknown(_)));
        assert!(status.is_terminal());
        assert!(!status.is_success_exit());
    }

    /// The classification must never rewrite a status it did not recognize.
    #[test]
    fn unknown_statuses_round_trip_losslessly() {
        let raw = "some_status_from_a_newer_binary";
        let status = CookStatus::from_status(raw);
        assert_eq!(status.as_str(), raw);
        let json = serde_json::to_string(&status).expect("serialize");
        assert_eq!(json, format!("\"{raw}\""));
        let parsed: CookStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, status);
    }

    #[test]
    fn every_known_status_round_trips_through_its_wire_form() {
        for status in [
            CookStatus::Queued,
            CookStatus::Running,
            CookStatus::InFlight,
            CookStatus::Completed,
            CookStatus::ReviewReady,
            CookStatus::GreenNoFinalize,
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
            CookStatus::ProviderFailure,
            CookStatus::DurableFailure,
            CookStatus::PreExecutionFailure,
            CookStatus::PreArtifactInterruption,
            CookStatus::Failed,
        ] {
            assert_eq!(
                CookStatus::from_status(status.as_str()),
                status,
                "{status} must round-trip"
            );
            assert!(
                !matches!(status, CookStatus::Unknown(_)),
                "{status} must be a known variant"
            );
        }
    }

    /// Pins the exit-code vocabulary that previously lived as a separate
    /// string list, so it cannot drift from terminality again.
    #[test]
    fn success_exit_covers_in_flight_plus_the_two_green_terminal_states() {
        for status in [
            "queued",
            "running",
            "in_flight",
            "review_ready",
            "green_no_finalize",
        ] {
            assert!(
                CookStatus::from_status(status).is_success_exit(),
                "{status} must exit 0 on status alone"
            );
        }
        for status in ["failed", "gate_failed", "no_changes", "durable_failure"] {
            assert!(
                !CookStatus::from_status(status).is_success_exit(),
                "{status} must not exit 0 on status alone"
            );
        }
    }
}
