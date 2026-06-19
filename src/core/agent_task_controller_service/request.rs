//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

/// Parse an `AgentTaskPlan` out of a controller spawn-task request.
///
/// The request may either wrap the plan under a `"plan"` field or be the plan
/// directly. Exposed for callers that need to validate plans before scheduling.
pub fn plan_from_controller_request(request: &Value) -> Result<AgentTaskPlan> {
    let plan_value = request.get("plan").unwrap_or(request);
    serde_json::from_value(plan_value.clone()).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("controller spawn_task plan".to_string()),
            Some(plan_value.to_string()),
        )
    })
}

pub(super) fn materialize_fan_out_request(template: &Value, entity_id: &str) -> Value {
    let mut request = template.clone();
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "entity_id".to_string(),
            Value::String(entity_id.to_string()),
        );
        object.entry("run_id".to_string()).or_insert_with(|| {
            Value::String(format!(
                "controller-{}",
                entity_id.replace([':', '/', '#'], "_")
            ))
        });
    }
    request
}

pub(super) fn controller_request_run_id(
    request: &Value,
    dedupe_key: &str,
    action_id: &str,
) -> String {
    optional_string(request, "run_id").unwrap_or_else(|| {
        format!(
            "controller-{}-{}",
            action_id,
            dedupe_key.replace([':', '/', '#', ' '], "_")
        )
    })
}

pub(super) fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key).ok_or_else(|| {
        Error::validation_invalid_argument(
            key,
            format!("controller action request requires string field '{key}'"),
            None,
            None,
        )
    })
}

/// Extract an optional string field from a controller request JSON value.
pub fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Extract an optional boolean field from a controller request JSON value.
pub fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

/// Extract an optional `u32` field from a controller request JSON value.
pub fn optional_u32(value: &Value, key: &str) -> Result<Option<u32>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a u32"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

/// Extract an optional `usize` field from a controller request JSON value.
pub fn optional_usize(value: &Value, key: &str) -> Result<Option<usize>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a usize"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

/// Extract an optional `Vec<String>` field from a controller request JSON value.
pub fn optional_string_array(value: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = value.get(key) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(Error::validation_invalid_argument(
            key,
            format!("controller action request field '{key}' must be an array of strings"),
            Some(value.to_string()),
            None,
        ));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                Error::validation_invalid_argument(
                    key,
                    format!("controller action request field '{key}' must contain only strings"),
                    Some(value.to_string()),
                    None,
                )
            })
        })
        .collect()
}

pub(super) fn push_controller_history(
    record: &mut AgentTaskLoopControllerRecord,
    event_type: &str,
    entity_id: Option<String>,
    payload: Value,
) {
    record.history.push(AgentTaskLoopHistoryEvent {
        event_id: format!("event-{}", record.history.len() + 1),
        event_type: event_type.to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entity_id,
        payload,
    });
}
