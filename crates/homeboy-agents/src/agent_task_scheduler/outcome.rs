//! Outcome helpers grouped by normalization and provider-status concern.

pub(super) use super::outcome_artifacts::*;
pub(super) use super::outcome_status::*;

use super::*;

pub(super) fn event(
    task_id: &str,
    state: AgentTaskState,
    attempt: u32,
    message: Option<String>,
) -> AgentTaskProgressEvent {
    AgentTaskProgressEvent {
        task_id: task_id.to_string(),
        state,
        attempt,
        message,
    }
}
