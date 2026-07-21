//! JSON field-access helpers for controller action records.
//!
//! Small, pure navigators over `serde_json::Value` used to pull diagnostic /
//! failure context out of loosely-typed action payloads (`string_field`,
//! `nested_string_field`, `find_string_field`, `find_value_field`, and the
//! `FailureContext` summary). Extracted from the controller-service `actions`
//! god file to isolate the generic value-navigation utilities from the action
//! execution logic.

use serde_json::Value;

#[derive(Default)]
pub(super) struct FailureContext {
    pub(super) diagnostic: Option<String>,
    pub(super) task_id: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) failure_phase: Option<String>,
    pub(super) runtime_context: Option<Value>,
    pub(super) replay_command: Option<String>,
}

pub(super) fn find_failure_context(value: &Value) -> Option<FailureContext> {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(diagnostics)) = map.get("diagnostics") {
                if let Some(diagnostic) = diagnostics.iter().find_map(Value::as_object) {
                    let message = diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if message.is_some() {
                        return Some(FailureContext {
                            diagnostic: message,
                            task_id: string_field(value, "task_id"),
                            provider: diagnostic
                                .get("provider")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .or_else(|| nested_string_field(diagnostic, "data", "provider"))
                                .or_else(|| nested_string_field(diagnostic, "data", "provider_id")),
                            failure_phase: diagnostic
                                .get("phase")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .or_else(|| nested_string_field(diagnostic, "data", "phase"))
                                .or_else(|| {
                                    diagnostic
                                        .get("class")
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                }),
                            runtime_context: diagnostic
                                .get("data")
                                .and_then(|data| data.get("runtime_context"))
                                .cloned()
                                .or_else(|| find_value_field(value, "runtime_context")),
                            replay_command: diagnostic
                                .get("data")
                                .and_then(|data| string_field(data, "replay_command"))
                                .or_else(|| find_string_field(value, "replay_command")),
                        });
                    }
                }
            }
            map.values().find_map(find_failure_context)
        }
        Value::Array(items) => items.iter().find_map(find_failure_context),
        _ => None,
    }
}

pub(super) fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

pub(super) fn nested_string_field(
    map: &serde_json::Map<String, Value>,
    parent: &str,
    field: &str,
) -> Option<String> {
    map.get(parent)?.get(field)?.as_str().map(str::to_string)
}

pub(super) fn find_string_field(value: &Value, field: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                map.values()
                    .find_map(|value| find_string_field(value, field))
            }),
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_string_field(value, field)),
        _ => None,
    }
}

pub(super) fn find_value_field(value: &Value, field: &str) -> Option<Value> {
    match value {
        Value::Object(map) => map.get(field).cloned().or_else(|| {
            map.values()
                .find_map(|value| find_value_field(value, field))
        }),
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_value_field(value, field)),
        _ => None,
    }
}
