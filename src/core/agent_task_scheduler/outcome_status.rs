//! Outcome normalization, typed-artifact extraction, template rendering, and
//! status-detection helpers for the agent-task scheduler.
//!
//! These functions inspect provider outcomes/results to decide materializability,
//! detect nested failed-executor status, recognize empty/incomplete runs, match
//! required typed artifacts, and render output-binding templates. They are split
//! from the scheduling engine so the value-shaping concerns stay together.
//! Helpers are `pub(super)` so the scheduler loop, scheduling engine, and tests
//! can reach them without widening the crate-public surface.

use std::collections::HashMap;
use std::fs;

use serde_json::{Map, Value};

use super::*;
use crate::core::config::value_type_name;

pub(super) fn provider_run_result_is_empty_incomplete(result: &Value) -> bool {
    // The provider run state may live at the top level of the result object or
    // nested under an `outputs` key (a common provider-wrapper shape, e.g.
    // `{ "success": true, "status": "completed", "outputs": { "completed": false, ... } }`).
    // Detect an incomplete, no-output run at either level so cook does not treat
    // a cell that never produced an assistant/tool interaction as successful.
    if empty_incomplete_run_state(result) {
        return true;
    }
    result
        .get("outputs")
        .filter(|value| value.is_object())
        .map(empty_incomplete_run_state)
        .unwrap_or(false)
}

pub(super) fn empty_incomplete_run_state(state: &Value) -> bool {
    state.get("completed").and_then(Value::as_bool) == Some(false)
        && value_text_is_empty(state.get("reply"))
        && !has_assistant_message(state)
        && !has_tool_calls(state)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NestedFailedExecutorStatus {
    pub(super) path: String,
    pub(super) key: String,
    pub(super) value: String,
}

pub(super) fn nested_failed_executor_status(
    outcome: &AgentTaskOutcome,
) -> Option<NestedFailedExecutorStatus> {
    if let Some(found) = outcome
        .outputs
        .get("provider_run_result")
        .and_then(|result| find_failed_status_value(result, "outputs.provider_run_result"))
    {
        return Some(found);
    }

    outcome
        .typed_artifacts
        .iter()
        .filter(|artifact| {
            artifact
                .metadata
                .get("normalized_from")
                .and_then(Value::as_str)
                != Some("runtime_outcome")
                && (artifact.name == "agent_result"
                    || artifact.artifact_schema.as_deref() == Some(AGENT_TASK_OUTCOME_SCHEMA))
        })
        .find_map(|artifact| {
            find_failed_status_value(
                &artifact.payload,
                &format!("typed_artifacts.{}.payload", artifact.name),
            )
        })
}

pub(super) fn find_failed_status_value(
    value: &Value,
    path: &str,
) -> Option<NestedFailedExecutorStatus> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                if is_terminal_status_key(key) {
                    if let Some(raw) = child.as_str().map(str::trim).filter(|raw| !raw.is_empty()) {
                        if status_value_is_failed(raw) {
                            return Some(NestedFailedExecutorStatus {
                                path: child_path,
                                key: key.clone(),
                                value: raw.to_string(),
                            });
                        }
                    }
                }
                if let Some(found) = find_failed_status_value(child, &child_path) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, child)| find_failed_status_value(child, &format!("{path}.{index}"))),
        _ => None,
    }
}

/// Recognizes keys that carry a nested executor's terminal status/state.
///
/// Matches the `status`/`*_status` family (e.g. `status`, `job_status`,
/// `completion_status`) and the `state`/`*_state` family (e.g. `state`,
/// `terminal_state`, `run_state`) so that a failed bundle reported through
/// either family of typed-output fields propagates as a task failure. A nested
/// executor may report its terminal failure only through a `terminal_state`
/// field (e.g. `wait_result.terminal_state = "failed - ..."`); without
/// recognizing `*_state` keys, a wrapper outcome of `Succeeded` would mask that
/// failure. Whether a matched key's value is actually a failure is decided
/// separately by [`status_value_is_failed`], so non-failure values such as
/// `completed`, `succeeded`, or `partial` never trip this on their own.
pub(super) fn is_terminal_status_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized == "status"
        || normalized == "state"
        || normalized.ends_with("_status")
        || normalized.ends_with("_state")
}

pub(super) fn status_value_is_failed(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "fail"
        || normalized == "failed"
        || normalized == "failure"
        || normalized == "error"
        || normalized == "timeout"
        || normalized == "timed_out"
        || normalized == "cancelled"
        || normalized == "canceled"
        || normalized.starts_with("failed ")
        || normalized.starts_with("failed-")
}

pub(super) fn value_text_is_empty(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
}

pub(super) fn has_assistant_message(result: &Value) -> bool {
    ["assistant_message", "assistantMessage"]
        .into_iter()
        .any(|key| value_has_text(result.get(key)))
        || ["assistant_messages", "assistantMessages"]
            .into_iter()
            .any(|key| array_has_message_text(result.get(key)))
        || array_has_assistant_role_message(result.get("messages"))
}

pub(super) fn value_has_text(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(raw)) => !raw.trim().is_empty(),
        Some(Value::Object(object)) => ["content", "text", "message", "reply"]
            .into_iter()
            .any(|key| value_has_text(object.get(key))),
        Some(Value::Array(items)) => items.iter().any(|item| value_has_text(Some(item))),
        _ => false,
    }
}

pub(super) fn array_has_message_text(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Array(items)) => items.iter().any(|item| value_has_text(Some(item))),
        other => value_has_text(other),
    }
}

pub(super) fn array_has_assistant_role_message(value: Option<&Value>) -> bool {
    let Some(Value::Array(messages)) = value else {
        return false;
    };
    messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("assistant")
            && value_has_text(Some(message))
    })
}

pub(super) fn has_tool_calls(result: &Value) -> bool {
    ["tool_calls", "toolCalls"]
        .into_iter()
        .any(|key| value_has_items(result.get(key)))
        || result
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages.iter().any(|message| {
                    value_has_items(message.get("tool_calls"))
                        || value_has_items(message.get("toolCalls"))
                })
            })
            .unwrap_or(false)
}

pub(super) fn value_has_items(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(object)) => !object.is_empty(),
        Some(Value::String(raw)) => !raw.trim().is_empty(),
        _ => false,
    }
}
