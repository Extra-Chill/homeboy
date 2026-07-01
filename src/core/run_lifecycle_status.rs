use serde::{Deserialize, Serialize};

use crate::core::run_lifecycle_record::RunExecutionState;

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
    fn execution_state_projects_to_canonical_status() {
        assert_eq!(
            RunLifecycleStatus::from(RunExecutionState::Running),
            RunLifecycleStatus::Running
        );
        assert_eq!(
            RunLifecycleStatus::from(RunExecutionState::Failed),
            RunLifecycleStatus::Failed
        );
    }
}
