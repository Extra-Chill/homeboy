//! Agent-task outcome and failure-classification enums.
//!
//! Standalone serde enums (no dependencies) describing an agent-task's terminal
//! status and how a failed attempt is classified. They live here because the
//! lab-contract `agent_task_config` policy types reference them; `core`'s
//! `agent_task::outcome` module re-exports them so its many call sites are
//! unchanged.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskOutcomeStatus {
    Succeeded,
    NoOp,
    UnableToRemediate,
    ProviderError,
    Timeout,
    /// A timed-out, cancelled, or panicked executor left a complete patch for
    /// controller-owned review, promotion, or explicit provider adoption.
    CandidateRecoverable,
    Failed,
    FollowUpIssue,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFailureClassification {
    Provider,
    /// Transient provider/network failure (timeouts, connection resets, cURL
    /// error 28, 5xx, temporarily-unavailable). These are safe to retry with
    /// bounded backoff because the same request can succeed on a later attempt.
    Transient,
    Timeout,
    /// Provider attempt produced no output/progress within the configured
    /// liveness window. Distinguishes a silent hang from a wall-clock timeout
    /// so the scheduler can rotate instead of waiting indefinitely.
    Stalled,
    /// Provider or backend explicitly signaled a rate limit (HTTP 429 or
    /// generic rate-limit text). Distinct from transient so callers can
    /// respect retry-after hints and the scheduler can rotate when the
    /// pinned model is throttled.
    #[serde(
        alias = "provider_quota",
        alias = "quota_exceeded",
        alias = "rate_limit"
    )]
    RateLimited,
    PolicyDenied,
    CapabilityMissing,
    InvalidInput,
    ExecutionFailed,
    Unknown,
}

impl AgentTaskOutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::NoOp => "no_op",
            Self::UnableToRemediate => "unable_to_remediate",
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::CandidateRecoverable => "candidate_recoverable",
            Self::Failed => "failed",
            Self::FollowUpIssue => "follow_up_issue",
            Self::Cancelled => "cancelled",
        }
    }
}
