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
//! | exit code `0` | `queued`, `running`, `in_flight`, `review_ready`, `draft_published`, `green_no_finalize` |
//! | terminality | everything except `queued`, `running`, `in_flight` |
//!
//! Three lists over one open vocabulary have to agree by hand, and they did
//! not: `cancelled` and `timed_out` were known only to the batch list, and
//! `review_ready`/`draft_published`/`green_no_finalize` only to the exit-code list. This module
//! is the one place the vocabulary is written down, so the three call sites
//! classify against the same enum instead of three divergent string literals.
//!
//! # Terminality is declared, not inferred
//!
//! Whether a Cook will advance on its own is *not* derived from this
//! vocabulary. It is [`CookDisposition`], declared by the exit that built the
//! report.
//!
//! Inferring it from the status string was the original defect. The terminal
//! side of the vocabulary is open — several exits feed it straight from
//! `finalization["status"]`, an arbitrary JSON string that defaults to
//! `"unknown"` when the field is missing — so any inference has to pick a
//! wrong default for statuses it has not been taught. Guessing "terminal"
//! fires a completion at the orchestrator for a Cook that is still running;
//! guessing "in flight" leaves a finished Cook with no completion at all.
//!
//! There is no need to guess. At every one of those exits the Cook loop has
//! already decided: it either handed the work to a durable owner that will
//! carry it forward, or it stopped. Recording that decision removes the
//! question instead of answering it.
//!
//! [`CookStatus::Unknown`] preserves the raw string so a status this binary
//! does not recognize is never rewritten.
//!
//! # Classifying an open vocabulary for a machine consumer
//!
//! Openness is right for reporting and wrong for classification. A consumer
//! that receives `{"status": "moving_base"}` over HTTP, or reads it back from
//! a persisted report, has no process exit code to branch on and cannot decide
//! success, terminality, or retry from the string alone.
//!
//! `homeboy_core::run_lifecycle_status::RunLifecycleStatus` is the closed
//! vocabulary that answers those questions, and `From<&CookStatus>` is the
//! projection onto it. Reports emit it as an additive `lifecycle_status`
//! beside the unchanged `status`, so callers that match on the raw string keep
//! working while a machine consumer gets a decidable classification.
//!
//! That projection is deliberately *not* an authority on terminality. It is
//! pinned to agree with [`CookStatus::is_in_flight`] for every known variant,
//! and reports emit `terminal` from the declared [`CookDisposition`] below.
//! [`CookStatus::Unknown`] projects to an explicit "unknown" rather than a
//! manufactured failure, for exactly the reason terminality is declared here
//! rather than inferred.

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
    /// Finished with a verified draft pull request published for review.
    DraftPublished,
    /// Gates were green but finalization was intentionally not performed.
    GreenNoFinalize,
    /// A verified provider review intentionally produced no candidate patch.
    IntentionalNoChange,
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
    /// The optional read-only review form exceeded its distinct deadline.
    ReviewFormTimeout,
    /// Ran out of retries.
    RetriesExhausted,
    /// Ran out of execution budget.
    ExecutionBudgetExhausted,
    /// Stopped by policy.
    PolicyFailure,
    /// Stopped until an operator selects one of several distinct patch candidates.
    SelectionRequired,
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
            "draft_published" => Self::DraftPublished,
            "green_no_finalize" => Self::GreenNoFinalize,
            "intentional_no_change" => Self::IntentionalNoChange,
            "no_changes" => Self::NoChanges,
            "no_op_gate_failed" => Self::NoOpGateFailed,
            "gate_failed" => Self::GateFailed,
            "awaiting_acceptance" => Self::AwaitingAcceptance,
            "candidate_recoverable" => Self::CandidateRecoverable,
            "blocked_by_dependency" => Self::BlockedByDependency,
            "blocked" => Self::Blocked,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "review_form_timeout" => Self::ReviewFormTimeout,
            "retries_exhausted" => Self::RetriesExhausted,
            "execution_budget_exhausted" => Self::ExecutionBudgetExhausted,
            "policy_failure" => Self::PolicyFailure,
            "selection_required" => Self::SelectionRequired,
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
            Self::DraftPublished => "draft_published",
            Self::GreenNoFinalize => "green_no_finalize",
            Self::IntentionalNoChange => "intentional_no_change",
            Self::NoChanges => "no_changes",
            Self::NoOpGateFailed => "no_op_gate_failed",
            Self::GateFailed => "gate_failed",
            Self::AwaitingAcceptance => "awaiting_acceptance",
            Self::CandidateRecoverable => "candidate_recoverable",
            Self::BlockedByDependency => "blocked_by_dependency",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::ReviewFormTimeout => "review_form_timeout",
            Self::RetriesExhausted => "retries_exhausted",
            Self::ExecutionBudgetExhausted => "execution_budget_exhausted",
            Self::PolicyFailure => "policy_failure",
            Self::SelectionRequired => "selection_required",
            Self::ProviderFailure => "provider_failure",
            Self::DurableFailure => "durable_failure",
            Self::PreExecutionFailure => "pre_execution_failure",
            Self::PreArtifactInterruption => "pre_artifact_interruption",
            Self::Failed => "failed",
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    /// Whether this status *reads* as a Cook that is still progressing.
    ///
    /// This is a property of the vocabulary, not the authority on terminality
    /// — that is [`CookDisposition`], which the producing exit declares. This
    /// exists so a report can be checked for self-consistency between the
    /// status it carries and the disposition it declares, and so batch totals
    /// can bucket a cell by its reported status.
    ///
    /// Unlike the terminal side, this set is closed: it is internal to the
    /// Cook loop and never sourced from finalization JSON.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::InFlight)
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
                | Self::DraftPublished
                | Self::GreenNoFinalize
                | Self::IntentionalNoChange
        )
    }
}

impl fmt::Display for CookStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a Cook will advance on its own, as declared by the exit that
/// produced the report.
///
/// Every Cook exit already knows this. It either handed the work to a durable
/// owner — a runner daemon or detached staging that owns timeout and provider
/// rotation from that point on — or it stopped and there is nothing left to
/// carry the Cook forward. This records that fact so no consumer has to
/// re-derive it from a status string.
///
/// Producing a report requires stating one of these, so an exit added later
/// cannot inherit a default that happens to be wrong for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookDisposition {
    /// The Cook returned, but a durable owner is still carrying the work.
    /// No terminal notification is due.
    InFlight,
    /// The Cook will not advance without external action. This is the
    /// completion the orchestrator is waiting for.
    Terminal,
}

impl CookDisposition {
    /// Whether the Cook will not advance without external action.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal)
    }

    /// The durable progress phase label for this disposition.
    pub fn phase(&self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::Terminal => "terminal",
        }
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
    fn the_in_flight_reading_covers_exactly_the_loop_owned_states() {
        for status in ["queued", "running", "in_flight"] {
            assert!(
                CookStatus::from_status(status).is_in_flight(),
                "{status} must read as in flight"
            );
        }
        for status in [
            "completed",
            "review_ready",
            "draft_published",
            "green_no_finalize",
            "intentional_no_change",
            "no_changes",
            "gate_failed",
            "awaiting_acceptance",
            "cancelled",
            "timed_out",
            "review_form_timeout",
            "durable_failure",
            "selection_required",
        ] {
            assert!(
                !CookStatus::from_status(status).is_in_flight(),
                "{status} must not read as in flight"
            );
        }
    }

    /// The whole point of declaring disposition: a status this binary has
    /// never seen carries no claim about whether the Cook is still running.
    #[test]
    fn an_unknown_status_makes_no_claim_about_progress() {
        let status = CookStatus::from_status("some_status_from_a_newer_binary");
        assert!(matches!(status, CookStatus::Unknown(_)));
        assert!(!status.is_in_flight());
        assert!(!status.is_success_exit());
    }

    #[test]
    fn disposition_is_the_authority_on_terminality() {
        assert!(CookDisposition::Terminal.is_terminal());
        assert!(!CookDisposition::InFlight.is_terminal());
        assert_eq!(CookDisposition::Terminal.phase(), "terminal");
        assert_eq!(CookDisposition::InFlight.phase(), "in_flight");
    }

    #[test]
    fn disposition_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&CookDisposition::InFlight).expect("serialize"),
            "\"in_flight\""
        );
        assert_eq!(
            serde_json::to_string(&CookDisposition::Terminal).expect("serialize"),
            "\"terminal\""
        );
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
    fn success_exit_covers_in_flight_and_candidate_free_terminal_successes() {
        for status in [
            "queued",
            "running",
            "in_flight",
            "review_ready",
            "draft_published",
            "green_no_finalize",
            "intentional_no_change",
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
