//! The closed vocabulary of outcomes Cook produces.
//! Known labels convert into canonical lifecycle state through `From`
//! (`homeboy_core::run_lifecycle_status::RunLifecycleStatus::from`). Terminality inside Cook is instead declared
//! by the producing exit's [`CookDisposition`].

use serde::de::IntoDeserializer;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The deterministic-gate verdict on a candidate. Cook status, the cook loop,
/// promotion and the delivery blocker all embed this one enum, so each
/// verdict has exactly one spelling and one legacy alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    /// A deterministic gate rejected the patched candidate.
    GateFailed,
    /// The provider produced no patch.
    NoChanges,
    /// The provider produced no patch and the candidate failed its gates.
    NoChangesGateFailed,
    /// Verification could not execute; the candidate remains recoverable.
    // Accepts `gates_deferred` for records written before #18802.
    #[serde(alias = "gates_deferred")]
    GateEnvironmentUnavailable,
}

impl GateVerdict {
    /// The wire representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GateFailed => "gate_failed",
            Self::NoChanges => "no_changes",
            Self::NoChangesGateFailed => "no_changes_gate_failed",
            Self::GateEnvironmentUnavailable => "gate_environment_unavailable",
        }
    }

    /// Whether a deterministic gate rejected the candidate.
    pub fn is_gate_failure(self) -> bool {
        matches!(self, Self::GateFailed | Self::NoChangesGateFailed)
    }
}

impl fmt::Display for GateVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A reported Cook status, serialized as a bare snake_case string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookStatus {
    Queued,
    InFlight,
    ReviewReady,
    DraftPublished,
    /// Published, but the agent reported at least one task item as not
    /// delivered; the pull request stays or opens as a draft (#18884).
    SucceededPartial,
    GreenNoFinalize,
    /// Read-only finalization validated publication without publishing.
    Validated,
    IntentionalNoChange,
    SupersededByActiveCook,
    NoCandidate,
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
    /// A gate verdict, serialized as the verdict's own label.
    #[serde(untagged)]
    Gate(GateVerdict),
}

impl CookStatus {
    /// Parse a persisted label at a serialization boundary, through the
    /// same serde vocabulary (and legacy aliases) the enum declares.
    pub fn from_status(status: &str) -> Option<Self> {
        Self::deserialize(IntoDeserializer::<serde::de::value::Error>::into_deserializer(status))
            .ok()
    }

    /// The wire representation.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::InFlight => "in_flight",
            Self::ReviewReady => "review_ready",
            Self::DraftPublished => "draft_published",
            Self::SucceededPartial => "succeeded_partial",
            Self::GreenNoFinalize => "green_no_finalize",
            Self::Validated => "validated",
            Self::IntentionalNoChange => "intentional_no_change",
            Self::SupersededByActiveCook => "superseded_by_active_cook",
            Self::NoCandidate => "no_candidate",
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
            Self::Gate(verdict) => verdict.as_str(),
        }
    }

    /// Whether the label reads as progressing, not an authority on terminality.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::Queued | Self::InFlight | Self::AwaitingCi)
    }

    /// Whether a deterministic gate rejected the candidate.
    pub fn is_gate_failure(&self) -> bool {
        matches!(self, Self::Gate(verdict) if verdict.is_gate_failure())
    }

    /// Whether the status reports a published pull request for the candidate.
    pub fn is_published(&self) -> bool {
        matches!(
            self,
            Self::ReviewReady | Self::DraftPublished | Self::SucceededPartial
        )
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
                | Self::SucceededPartial
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
