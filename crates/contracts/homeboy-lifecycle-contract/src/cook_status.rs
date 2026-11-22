//! The closed vocabulary of outcomes Cook produces.
//! Known labels convert into canonical lifecycle state through `From`
//! (`homeboy_core::run_lifecycle_status::RunLifecycleStatus::from`). Terminality inside Cook is instead declared
//! by the producing exit's [`CookDisposition`].

use serde::{Deserialize, Serialize};
use std::fmt;

/// A reported Cook status, serialized as a bare snake_case string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookStatus {
    Queued,
    InFlight,
    ReviewReady,
    DraftPublished,
    GreenNoFinalize,
    /// Read-only finalization validated publication without publishing.
    Validated,
    IntentionalNoChange,
    SupersededByActiveCook,
    NoCandidate,
    NoChanges,
    NoChangesGateFailed,
    GateFailed,
    /// Verification could not execute; the candidate remains recoverable.
    #[serde(alias = "gates_deferred")]
    GateEnvironmentUnavailable,
    /// Gates failed on the verified base as well as the candidate.
    BaselineRed,
    FinalizationFailed,
    GateDeclarationInvalid,
    ReproPassesOnBase,
    AwaitingAcceptance,
    AwaitingCi,
    CandidateRecoverable,
    Blocked,
    Cancelled,
    TimedOut,
    RetriesExhausted,
    ExecutionBudgetExhausted,
    PolicyFailure,
    SelectionRequired,
    ProviderFailure,
    DurableFailure,
    PreExecutionFailure,
    PreArtifactInterruption,
    Failed,
}

impl CookStatus {
    /// Parse a recognized persisted label.
    pub fn from_status(status: &str) -> Option<Self> {
        Some(match status {
            "queued" => Self::Queued,
            "in_flight" => Self::InFlight,
            "review_ready" => Self::ReviewReady,
            "draft_published" => Self::DraftPublished,
            "green_no_finalize" => Self::GreenNoFinalize,
            "validated" => Self::Validated,
            "intentional_no_change" => Self::IntentionalNoChange,
            "superseded_by_active_cook" => Self::SupersededByActiveCook,
            "no_candidate" => Self::NoCandidate,
            "no_changes" => Self::NoChanges,
            "no_changes_gate_failed" => Self::NoChangesGateFailed,
            "gate_failed" => Self::GateFailed,
            // Promotions still write the deferred-gate status as `gates_deferred`.
            "gate_environment_unavailable" | "gates_deferred" => Self::GateEnvironmentUnavailable,
            "baseline_red" => Self::BaselineRed,
            "finalization_failed" => Self::FinalizationFailed,
            "gate_declaration_invalid" => Self::GateDeclarationInvalid,
            "repro_passes_on_base" => Self::ReproPassesOnBase,
            "awaiting_acceptance" => Self::AwaitingAcceptance,
            "awaiting_ci" => Self::AwaitingCi,
            "candidate_recoverable" => Self::CandidateRecoverable,
            "blocked" => Self::Blocked,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "retries_exhausted" => Self::RetriesExhausted,
            "execution_budget_exhausted" => Self::ExecutionBudgetExhausted,
            "policy_failure" => Self::PolicyFailure,
            "selection_required" => Self::SelectionRequired,
            "provider_failure" => Self::ProviderFailure,
            "durable_failure" => Self::DurableFailure,
            "pre_execution_failure" => Self::PreExecutionFailure,
            "pre_artifact_interruption" => Self::PreArtifactInterruption,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    /// The wire representation.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::InFlight => "in_flight",
            Self::ReviewReady => "review_ready",
            Self::DraftPublished => "draft_published",
            Self::GreenNoFinalize => "green_no_finalize",
            Self::Validated => "validated",
            Self::IntentionalNoChange => "intentional_no_change",
            Self::SupersededByActiveCook => "superseded_by_active_cook",
            Self::NoCandidate => "no_candidate",
            Self::NoChanges => "no_changes",
            Self::NoChangesGateFailed => "no_changes_gate_failed",
            Self::GateFailed => "gate_failed",
            Self::GateEnvironmentUnavailable => "gate_environment_unavailable",
            Self::BaselineRed => "baseline_red",
            Self::FinalizationFailed => "finalization_failed",
            Self::GateDeclarationInvalid => "gate_declaration_invalid",
            Self::ReproPassesOnBase => "repro_passes_on_base",
            Self::AwaitingAcceptance => "awaiting_acceptance",
            Self::AwaitingCi => "awaiting_ci",
            Self::CandidateRecoverable => "candidate_recoverable",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::RetriesExhausted => "retries_exhausted",
            Self::ExecutionBudgetExhausted => "execution_budget_exhausted",
            Self::PolicyFailure => "policy_failure",
            Self::SelectionRequired => "selection_required",
            Self::ProviderFailure => "provider_failure",
            Self::DurableFailure => "durable_failure",
            Self::PreExecutionFailure => "pre_execution_failure",
            Self::PreArtifactInterruption => "pre_artifact_interruption",
            Self::Failed => "failed",
        }
    }

    /// Whether the label reads as progressing, not an authority on terminality.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::Queued | Self::InFlight | Self::AwaitingCi)
    }

    /// Whether a deterministic gate rejected the candidate.
    pub fn is_gate_failure(&self) -> bool {
        matches!(self, Self::GateFailed | Self::NoChangesGateFailed)
    }

    /// Whether the status alone means the operator has nothing to act on.
    pub fn is_success_exit(&self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::InFlight
                | Self::AwaitingCi
                | Self::ReviewReady
                | Self::DraftPublished
                | Self::GreenNoFinalize
                | Self::IntentionalNoChange
                | Self::SupersededByActiveCook
        )
    }
}

impl fmt::Display for CookStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a Cook will advance on its own, declared by its producing exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookDisposition {
    /// A durable owner is carrying the work; no terminal notification is due.
    InFlight,
    /// The Cook will not advance without external action.
    Terminal,
}

impl CookDisposition {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal)
    }

    pub fn phase(&self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::Terminal => "terminal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_declares_terminality_and_serializes_as_its_phase() {
        for (disposition, terminal) in [
            (CookDisposition::InFlight, false),
            (CookDisposition::Terminal, true),
        ] {
            assert_eq!(disposition.is_terminal(), terminal);
            assert_eq!(
                serde_json::to_value(disposition).unwrap(),
                disposition.phase()
            );
        }
    }
}
