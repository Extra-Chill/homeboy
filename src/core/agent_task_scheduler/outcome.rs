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

pub(super) fn missing_required_typed_artifacts(
    outcome: &AgentTaskOutcome,
    request: &AgentTaskRequest,
) -> Vec<String> {
    request
        .canonical_artifact_declarations()
        .into_iter()
        .filter(|declaration| declaration.required)
        .map(|declaration| declaration.name)
        .filter(|name| {
            !outcome
                .typed_artifacts
                .iter()
                .any(|artifact| artifact.name == *name)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InvalidRequiredTypedArtifact {
    pub(super) task_id: String,
    pub(super) name: String,
    pub(super) artifact_type: Option<String>,
    pub(super) artifact_id: Option<String>,
    pub(super) path: Option<String>,
    pub(super) url: Option<String>,
    pub(super) size_bytes: Option<u64>,
    pub(super) reason: String,
}

pub(super) fn invalid_required_typed_artifacts(
    outcome: &AgentTaskOutcome,
    request: &AgentTaskRequest,
) -> Vec<InvalidRequiredTypedArtifact> {
    request
        .canonical_artifact_declarations()
        .into_iter()
        .filter(|declaration| declaration.required)
        .filter_map(|declaration| {
            let artifact = outcome
                .typed_artifacts
                .iter()
                .find(|artifact| artifact.name == declaration.name)?;
            invalid_required_typed_artifact(outcome, artifact)
        })
        .collect()
}

fn invalid_required_typed_artifact(
    outcome: &AgentTaskOutcome,
    typed_artifact: &AgentTaskTypedArtifact,
) -> Option<InvalidRequiredTypedArtifact> {
    let artifact_id = typed_artifact
        .artifact
        .as_ref()
        .map(|artifact| artifact.id.clone());
    let path = typed_artifact
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.path.clone())
        .or_else(|| string_field(&typed_artifact.payload, "path"));
    let url = typed_artifact
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.url.clone())
        .or_else(|| string_field(&typed_artifact.payload, "url"));
    let size_bytes = typed_artifact
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.size_bytes)
        .or_else(|| {
            typed_artifact
                .payload
                .get("size_bytes")
                .and_then(Value::as_u64)
        });
    let reason = invalid_typed_artifact_reason(typed_artifact, path.as_deref(), size_bytes)?;

    Some(InvalidRequiredTypedArtifact {
        task_id: outcome.task_id.clone(),
        name: typed_artifact.name.clone(),
        artifact_type: typed_artifact.artifact_type.clone(),
        artifact_id,
        path,
        url,
        size_bytes,
        reason,
    })
}

fn invalid_typed_artifact_reason(
    typed_artifact: &AgentTaskTypedArtifact,
    path: Option<&str>,
    size_bytes: Option<u64>,
) -> Option<String> {
    if size_bytes == Some(0) {
        return Some("declared artifact size is zero bytes".to_string());
    }

    if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
        if let Ok(metadata) = fs::metadata(path) {
            if metadata.is_file() && metadata.len() == 0 {
                return Some("referenced artifact file is zero bytes".to_string());
            }
        }
    }

    if typed_artifact.artifact.is_none() && value_is_empty(&typed_artifact.payload) {
        return Some("typed artifact payload is empty and has no artifact reference".to_string());
    }

    None
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(object) => object.is_empty() || object.values().all(Value::is_null),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

pub(super) fn missing_typed_artifacts_failure(outcome: &AgentTaskOutcome) -> bool {
    outcome
        .summary
        .as_deref()
        .map(text_reports_missing_typed_artifacts)
        .unwrap_or(false)
        || outcome.diagnostics.iter().any(|diagnostic| {
            text_reports_missing_typed_artifacts(&diagnostic.class)
                || text_reports_missing_typed_artifacts(&diagnostic.message)
        })
}

pub(super) fn text_reports_missing_typed_artifacts(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("missing required typed artifacts")
        || value.contains("did not produce required typed artifacts")
}

pub(super) fn artifact_matches_required_artifact(name: &str, artifact: &AgentTaskArtifact) -> bool {
    [
        Some(artifact.kind.as_str()),
        Some(artifact.id.as_str()),
        artifact.name.as_deref(),
        artifact.path.as_deref(),
        artifact.mime.as_deref(),
        artifact.metadata.get("role").and_then(Value::as_str),
        artifact.metadata.get("artifact").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .any(|candidate| value_matches_required_artifact(name, candidate))
}

pub(super) fn evidence_matches_required_artifact(
    name: &str,
    evidence: &AgentTaskEvidenceRef,
) -> bool {
    [
        Some(evidence.kind.as_str()),
        Some(evidence.uri.as_str()),
        evidence.label.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|candidate| value_matches_required_artifact(name, candidate))
}

pub(super) fn value_matches_required_artifact(name: &str, value: &str) -> bool {
    let name = normalize_artifact_role_token(name);
    let value = normalize_artifact_role_token(value);
    if value == name || value.contains(&name) {
        return true;
    }

    match name.as_str() {
        "patch" => {
            value.contains("diff") || value.contains("textxpatch") || value.contains("textxdiff")
        }
        "transcript" => value.contains("log"),
        "agentresult" => value.contains("agentresult"),
        _ => false,
    }
}

pub(super) fn normalize_artifact_role_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

pub(super) fn typed_artifact_from_artifact(
    name: &str,
    artifact: AgentTaskArtifact,
    source: &str,
) -> AgentTaskTypedArtifact {
    AgentTaskTypedArtifact {
        name: name.to_string(),
        artifact_type: Some("file".to_string()),
        artifact_schema: None,
        payload: serde_json::json!({
            "artifact_id": artifact.id.clone(),
            "kind": artifact.kind.clone(),
            "path": artifact.path.clone(),
            "url": artifact.url.clone(),
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256.clone(),
        }),
        artifact: Some(artifact),
        metadata: serde_json::json!({ "normalized_from": source }),
    }
}

pub(super) fn typed_artifact_from_evidence(
    name: &str,
    evidence: &AgentTaskEvidenceRef,
    source: &str,
) -> AgentTaskTypedArtifact {
    AgentTaskTypedArtifact {
        name: name.to_string(),
        artifact_type: Some("evidence_ref".to_string()),
        artifact_schema: None,
        payload: serde_json::json!({
            "kind": evidence.kind,
            "uri": evidence.uri,
            "label": evidence.label,
        }),
        artifact: None,
        metadata: serde_json::json!({ "normalized_from": source }),
    }
}

pub(super) fn typed_artifact_from_outcome(outcome: &AgentTaskOutcome) -> AgentTaskTypedArtifact {
    AgentTaskTypedArtifact {
        name: "agent_result".to_string(),
        artifact_type: Some("json".to_string()),
        artifact_schema: Some(AGENT_TASK_OUTCOME_SCHEMA.to_string()),
        payload: serde_json::json!({
            "task_id": outcome.task_id.clone(),
            "status": outcome.status,
            "summary": outcome.summary.clone(),
            "outputs": outcome.outputs.clone(),
        }),
        artifact: None,
        metadata: serde_json::json!({ "normalized_from": "runtime_outcome" }),
    }
}

pub(super) fn runtime_result_is_materializable(outcome: &AgentTaskOutcome) -> bool {
    !outcome.artifacts.is_empty()
        || !outcome.evidence_refs.is_empty()
        || !outcome.outputs.is_null()
        || outcome.summary.is_some()
}

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

pub(super) fn render_template_value(raw: &str, bindings: &HashMap<String, Value>) -> Option<Value> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?.trim();
    let name = inner.strip_prefix("outputs.")?.trim();
    bindings.get(name).cloned()
}

pub(super) fn render_template_string(raw: &str, bindings: &HashMap<String, Value>) -> String {
    let mut rendered = raw.to_string();
    for (name, value) in bindings {
        let replacement = template_replacement(value);
        let compact = format!("{{{{outputs.{name}}}}}");
        let spaced = format!("{{{{ outputs.{name} }}}}");
        rendered = rendered.replace(&compact, &replacement);
        rendered = rendered.replace(&spaced, &replacement);
    }
    rendered
}

pub(super) fn template_replacement(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    if let Some(value) = value.as_str() {
        return value.to_string();
    }
    if matches!(
        value_type_name(value),
        "number" | "bool" | "array" | "object"
    ) {
        return value.to_string();
    }
    String::new()
}

pub(super) fn mark_generated_from_outputs(
    request: &mut AgentTaskRequest,
    dependencies: &AgentTaskOutputDependencies,
    bindings: &HashMap<String, Value>,
) {
    if !request.metadata.is_object() {
        request.metadata = Value::Object(Map::new());
    }
    let metadata = request.metadata.as_object_mut().expect("metadata object");
    metadata.insert("generated_from_outputs".to_string(), Value::Bool(true));
    metadata.insert(
        "depends_on".to_string(),
        Value::Array(
            dependencies
                .depends_on
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    metadata.insert(
        "resolved_output_bindings".to_string(),
        Value::Object(
            bindings
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        ),
    );
}

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
