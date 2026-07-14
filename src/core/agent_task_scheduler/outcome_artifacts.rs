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
    if is_empty_patch_artifact(typed_artifact) {
        return None;
    }

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

fn is_empty_patch_artifact(typed_artifact: &AgentTaskTypedArtifact) -> bool {
    let is_patch = typed_artifact.name == "patch"
        || typed_artifact.artifact_type.as_deref() == Some("patch")
        || string_field(&typed_artifact.payload, "kind").as_deref() == Some("patch")
        || typed_artifact
            .artifact
            .as_ref()
            .map(|artifact| artifact.kind == "patch")
            .unwrap_or(false);
    let has_reference = typed_artifact
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.path.as_ref().or(artifact.url.as_ref()))
        .is_some()
        || string_field(&typed_artifact.payload, "path").is_some()
        || string_field(&typed_artifact.payload, "url").is_some();
    is_patch && has_reference
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
