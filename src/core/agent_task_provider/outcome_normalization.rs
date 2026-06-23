use super::*;

pub(super) fn normalize_provider_outcome_roles(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    normalize_provider_artifact_roles(&mut outcome.artifacts, &provider.role_aliases);
    normalize_provider_run_result_output(outcome, &provider.role_aliases);
    let codebox_public_envelope_valid = normalize_codebox_public_result_envelope(outcome, provider);
    normalize_provider_runtime_contract(outcome, provider);
    if codebox_public_envelope_valid {
        surface_provider_run_result_diagnostics(outcome);
    }
}

const CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA: &str =
    "wp-codebox/artifact-result-envelope/v1";

fn normalize_codebox_public_result_envelope(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) -> bool {
    if provider.backend != "codebox" {
        return true;
    }

    let envelope = output_value(&outcome.outputs, "provider_run_result").cloned();
    let Some(envelope) = envelope else {
        fail_codebox_public_envelope_boundary(
            outcome,
            "codebox.public_result_envelope_missing",
            "WP Codebox provider result did not include the public artifact result envelope.",
            json!({
                "expected_schema": CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
                "provider_backend": provider.backend,
            }),
        );
        return false;
    };

    if envelope.get("schema").and_then(Value::as_str)
        != Some(CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA)
    {
        fail_codebox_public_envelope_boundary(
            outcome,
            "codebox.public_result_envelope_missing",
            "WP Codebox provider result used a non-public result shape; Homeboy only consumes the public artifact result envelope.",
            json!({
                "expected_schema": CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
                "actual_schema": envelope.get("schema").and_then(Value::as_str),
                "private_shape_detected": codebox_private_result_shape_detected(&envelope),
            }),
        );
        return false;
    }

    let typed_artifacts = codebox_public_typed_artifacts(&envelope);
    if typed_artifacts.is_empty() {
        fail_codebox_public_envelope_boundary(
            outcome,
            "codebox.public_result_typed_artifacts_missing",
            "WP Codebox public artifact result envelope did not include any typed artifacts.",
            json!({
                "expected_schema": CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
                "envelope_status": envelope.get("status").and_then(Value::as_str),
            }),
        );
        return true;
    }

    for typed_artifact in typed_artifacts {
        if outcome
            .typed_artifacts
            .iter()
            .any(|existing| existing.name == typed_artifact.name)
        {
            continue;
        }
        outcome.typed_artifacts.push(typed_artifact);
    }

    true
}

fn fail_codebox_public_envelope_boundary(
    outcome: &mut AgentTaskOutcome,
    class: &str,
    message: &str,
    data: Value,
) {
    outcome.status = AgentTaskOutcomeStatus::Failed;
    outcome.failure_classification = Some(AgentTaskFailureClassification::Provider);
    push_unique_diagnostic(
        &mut outcome.diagnostics,
        class.to_string(),
        message.to_string(),
        data,
    );
}

fn codebox_private_result_shape_detected(value: &Value) -> bool {
    value.get("agent_result").is_some()
        || value
            .get("metadata")
            .and_then(|metadata| metadata.get("agent_runtime"))
            .is_some()
}

fn codebox_public_typed_artifacts(envelope: &Value) -> Vec<AgentTaskTypedArtifact> {
    let Some(value) = envelope
        .get("typed_artifacts")
        .or_else(|| envelope.get("typedArtifacts"))
    else {
        return Vec::new();
    };

    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(codebox_public_typed_artifact_from_value)
            .collect(),
        Value::Object(map) => map
            .iter()
            .filter_map(|(name, payload)| {
                codebox_public_typed_artifact_from_value(payload).or_else(|| {
                    Some(AgentTaskTypedArtifact {
                        name: name.clone(),
                        artifact_type: payload
                            .get("type")
                            .or_else(|| payload.get("artifact_type"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        artifact_schema: payload
                            .get("artifact_schema")
                            .or_else(|| payload.get("schema"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        payload: payload.clone(),
                        artifact: None,
                        metadata: json!({ "source": CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA }),
                    })
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn codebox_public_typed_artifact_from_value(value: &Value) -> Option<AgentTaskTypedArtifact> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())?
        .to_string();
    Some(AgentTaskTypedArtifact {
        name,
        artifact_type: value
            .get("type")
            .or_else(|| value.get("artifact_type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        artifact_schema: value
            .get("artifact_schema")
            .or_else(|| value.get("schema"))
            .and_then(Value::as_str)
            .map(str::to_string),
        payload: value
            .get("payload")
            .cloned()
            .unwrap_or_else(|| value.clone()),
        artifact: None,
        metadata: value
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| json!({ "source": CODEBOX_PUBLIC_ARTIFACT_RESULT_ENVELOPE_SCHEMA })),
    })
}

/// Whether an outcome status represents a failure for which we want to mine the
/// provider run-result for actionable evidence.
fn is_failure_status(status: AgentTaskOutcomeStatus) -> bool {
    matches!(
        status,
        AgentTaskOutcomeStatus::Failed
            | AgentTaskOutcomeStatus::ProviderError
            | AgentTaskOutcomeStatus::Timeout
            | AgentTaskOutcomeStatus::UnableToRemediate
    )
}

/// Surface actionable provider run-result evidence into the outcome on failure
/// (#4105).
///
/// Provider executors emit a
/// structured run-result under `outputs.provider_run_result` following the
/// `*/agent-task-run-result/*` shape: `{status, failure_classification,
/// diagnostics[], artifacts[], metadata{provider_error,run_id,run_status,
/// runtime_id,runtime_status}, refs{logs,transcripts,artifact_bundles,...}}`.
///
/// Before this fix a FAILED run-result could be preserved verbatim while
/// homeboy surfaced nothing actionable — `agent-task logs/artifacts/review`
/// showed only the generic "agent task failed" summary even though the
/// run-result carried (or conspicuously lacked) provider error codes, a run /
/// runtime id, and log / transcript refs.
///
/// This walks the preserved run-result and ADDS (never overwrites) the
/// following to the outcome so operators get actionable info:
/// - each run-result `diagnostics[]` entry becomes an outcome diagnostic;
/// - `metadata.provider_error` + run/runtime ids + statuses become a single
///   `provider.run_result_failed` diagnostic and are mirrored onto
///   `outcome.metadata.provider_error`;
/// - `refs.{logs,transcripts,artifact_bundles,runtimes,patches}` become
///   `evidence_refs` so review/artifacts can surface them;
/// - if the run-result is an empty shell (no diagnostics, no provider_error, no
///   run/runtime id, no refs) a single reviewer-safe diagnostic explains that
///   no provider runtime/session was created, satisfying the acceptance rule
///   that a failed run-result is never an empty shell.
///
/// It is fully provider-agnostic: it keys only off the generic run-result shape
/// and never references any specific runtime, framework, or provider id.
pub(super) fn surface_provider_run_result_diagnostics(outcome: &mut AgentTaskOutcome) {
    if !is_failure_status(outcome.status) {
        return;
    }
    let Some(run_result) = output_value(&outcome.outputs, "provider_run_result").cloned() else {
        return;
    };
    let Some(run_result) = run_result.as_object() else {
        return;
    };

    // Only mine FAILED (or non-succeeded) run-results. A run-result that omits
    // status is treated as a failure here because the outcome itself already
    // failed.
    let run_status = run_result
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    if matches!(run_status.as_deref(), Some("succeeded") | Some("success")) {
        return;
    }

    let mut surfaced_evidence = false;

    // 1. Lift each run-result diagnostic into the outcome diagnostics, deduped
    //    by (class, message) against what is already present.
    if let Some(Value::Array(items)) = run_result.get("diagnostics") {
        for item in items {
            let Some(message) = item
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            if message.trim().is_empty() {
                continue;
            }
            let class = item
                .get("class")
                .or_else(|| item.get("kind"))
                .or_else(|| item.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("provider.run_result_diagnostic")
                .to_string();
            push_unique_diagnostic(
                &mut outcome.diagnostics,
                class,
                message,
                item.get("data").cloned().unwrap_or(Value::Null),
            );
            surfaced_evidence = true;
        }
    }

    // 2. Pull the structured failure metadata (provider error + run/runtime
    //    identity + statuses) into a single actionable diagnostic and mirror
    //    provider_error onto the outcome metadata.
    let metadata = run_result.get("metadata").and_then(Value::as_object);
    let provider_error = metadata
        .and_then(|map| map.get("provider_error"))
        .filter(|value| !is_empty_value(value))
        .cloned();
    let run_id = metadata
        .and_then(|map| map.get("run_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let runtime_id = metadata
        .and_then(|map| map.get("runtime_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let runtime_status = metadata
        .and_then(|map| map.get("runtime_status"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);

    let has_identity = provider_error.is_some()
        || run_id.is_some()
        || runtime_id.is_some()
        || runtime_status.is_some();

    if has_identity {
        let message = describe_run_result_failure(
            run_status.as_deref(),
            run_id.as_deref(),
            runtime_id.as_deref(),
            runtime_status.as_deref(),
            provider_error.as_ref(),
        );
        let data = json!({
            "run_status": run_status,
            "run_id": run_id,
            "runtime_id": runtime_id,
            "runtime_status": runtime_status,
            "provider_error": provider_error,
        });
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "provider.run_result_failed".to_string(),
            message,
            data,
        );
        surfaced_evidence = true;

        if let Some(provider_error) = provider_error {
            mirror_provider_error_metadata(outcome, provider_error);
        }
    }

    // 3. Promote run-result refs (logs, transcripts, artifact bundles, runtimes,
    //    patches) into evidence refs so review/artifacts can surface them.
    if let Some(refs) = run_result.get("refs").and_then(Value::as_object) {
        for (group, kind) in [
            ("logs", "provider-log"),
            ("transcripts", "provider-transcript"),
            ("artifact_bundles", "provider-artifact-bundle"),
            ("runtimes", "provider-runtime"),
            ("patches", "provider-patch"),
        ] {
            let Some(Value::Array(entries)) = refs.get(group) else {
                continue;
            };
            for entry in entries {
                if let Some(reference) = run_result_ref_uri(entry) {
                    push_unique_evidence_ref(&mut outcome.evidence_refs, kind, reference, group);
                    surfaced_evidence = true;
                }
            }
        }
    }

    // 4. Empty shell guard: a failed run-result that surfaced no diagnostics,
    //    no provider error, no run/runtime identity, and no refs must still
    //    explain itself rather than appear as an opaque empty failure.
    if !surfaced_evidence {
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "provider.run_result_empty".to_string(),
            "Provider run-result reported failure but produced no diagnostics, \
             provider error, run/runtime id, or log/transcript refs: no provider \
             runtime or session appears to have been created."
                .to_string(),
            json!({ "run_status": run_status }),
        );
    }
}

/// Treat `null`, empty string, empty object, and empty array as "no value" so an
/// empty `provider_error: {}` shell is not mistaken for real evidence.
fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Build a human-readable failure message from the run-result identity fields.
fn describe_run_result_failure(
    run_status: Option<&str>,
    run_id: Option<&str>,
    runtime_id: Option<&str>,
    runtime_status: Option<&str>,
    provider_error: Option<&Value>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(error) = provider_error {
        if let Some(message) = provider_error_message(error) {
            parts.push(format!("provider error: {message}"));
        } else {
            parts.push("provider error reported".to_string());
        }
    }
    if let Some(run_id) = run_id {
        parts.push(format!("run_id={run_id}"));
    }
    if let Some(runtime_id) = runtime_id {
        parts.push(format!("runtime_id={runtime_id}"));
    }
    if let Some(runtime_status) = runtime_status {
        parts.push(format!("runtime_status={runtime_status}"));
    }
    if parts.is_empty() {
        return format!(
            "Provider run-result failed (status={}).",
            run_status.unwrap_or("failed")
        );
    }
    format!("Provider run-result failed: {}.", parts.join(", "))
}

/// Extract a short error message from a provider_error value, accepting either a
/// string or an object carrying `message`/`error`/`detail`/`code`.
fn provider_error_message(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => map
            .get("message")
            .or_else(|| map.get("error"))
            .or_else(|| map.get("detail"))
            .or_else(|| map.get("code"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Mirror the provider_error onto `outcome.metadata.provider_error` without
/// clobbering an existing populated value.
fn mirror_provider_error_metadata(outcome: &mut AgentTaskOutcome, provider_error: Value) {
    let mut metadata = outcome.metadata.as_object().cloned().unwrap_or_default();
    let already_populated = metadata
        .get("provider_error")
        .is_some_and(|existing| !is_empty_value(existing));
    if !already_populated {
        metadata.insert("provider_error".to_string(), provider_error);
        outcome.metadata = Value::Object(metadata);
    }
}

/// Resolve a usable reference URI/path from a run-result ref entry, accepting a
/// bare string or an object carrying `uri`/`url`/`path`/`ref`/`id`.
fn run_result_ref_uri(entry: &Value) -> Option<String> {
    match entry {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => map
            .get("uri")
            .or_else(|| map.get("url"))
            .or_else(|| map.get("path"))
            .or_else(|| map.get("ref"))
            .or_else(|| map.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Push a diagnostic unless an identical (class, message) is already present.
fn push_unique_diagnostic(
    diagnostics: &mut Vec<AgentTaskDiagnostic>,
    class: String,
    message: String,
    data: Value,
) {
    if diagnostics
        .iter()
        .any(|existing| existing.class == class && existing.message == message)
    {
        return;
    }
    diagnostics.push(AgentTaskDiagnostic {
        class,
        message,
        data,
    });
}

/// Push an evidence ref unless an identical (kind, uri) is already present.
fn push_unique_evidence_ref(
    evidence_refs: &mut Vec<AgentTaskEvidenceRef>,
    kind: &str,
    uri: String,
    group: &str,
) {
    if evidence_refs
        .iter()
        .any(|existing| existing.kind == kind && existing.uri == uri)
    {
        return;
    }
    evidence_refs.push(AgentTaskEvidenceRef {
        kind: kind.to_string(),
        uri,
        label: Some(format!("provider run-result {group}")),
    });
}

fn normalize_provider_runtime_contract(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    let normalization = &provider.runtime_contract.normalization;
    if let Some(summary_path) = normalization.summary_path.as_deref() {
        if let Some(summary) = dotted_value(outcome, summary_path).and_then(Value::as_str) {
            if !summary.trim().is_empty() {
                outcome.summary = Some(summary.to_string());
            }
        }
    }

    if let Some(status_path) = normalization.status_path.as_deref() {
        if let Some(status) = dotted_value(outcome, status_path)
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if let Some(mapped_status) = provider
                .runtime_contract
                .lifecycle_states
                .outcome_statuses
                .get(&status)
                .copied()
            {
                outcome.status = mapped_status;
            }
            if let Some(mapped_classification) = provider
                .runtime_contract
                .lifecycle_states
                .failure_classifications
                .get(&status)
                .copied()
            {
                outcome.failure_classification = Some(mapped_classification);
            } else if outcome.status != AgentTaskOutcomeStatus::Succeeded
                && outcome.failure_classification.is_none()
            {
                outcome.failure_classification = Some(AgentTaskFailureClassification::Unknown);
            }
        }
    }

    for mapping in &normalization.output_artifacts {
        let Some(value) = dotted_value(outcome, &mapping.path).cloned() else {
            continue;
        };
        normalize_provider_runtime_artifact(outcome, mapping, value);
    }
}

fn normalize_provider_runtime_artifact(
    outcome: &mut AgentTaskOutcome,
    mapping: &AgentTaskRuntimeOutputArtifactMapping,
    value: Value,
) {
    let id = mapping.id.clone().unwrap_or_else(|| mapping.name.clone());
    if outcome.artifacts.iter().any(|artifact| artifact.id == id) {
        return;
    }

    let path = value.as_str().map(str::to_string).or_else(|| {
        value
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let url = value.get("url").and_then(Value::as_str).map(str::to_string);
    let artifact = AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.clone(),
        kind: mapping
            .kind
            .clone()
            .or_else(|| mapping.artifact_type.clone())
            .unwrap_or_else(|| mapping.name.clone()),
        name: Some(mapping.name.clone()),
        label: None,
        role: None,
        semantic_key: None,
        path,
        url,
        mime: mapping.mime.clone(),
        size_bytes: None,
        sha256: None,
        metadata: json!({
            "runtime_contract": true,
            "source_path": mapping.path,
        }),
    };

    outcome.artifacts.push(artifact.clone());
    if mapping.artifact_type.is_some() || mapping.artifact_schema.is_some() {
        outcome.typed_artifacts.push(AgentTaskTypedArtifact {
            name: mapping.name.clone(),
            artifact_type: mapping.artifact_type.clone(),
            artifact_schema: mapping.artifact_schema.clone(),
            payload: value,
            artifact: Some(artifact),
            metadata: json!({ "runtime_contract": true }),
        });
    }
}

fn dotted_value<'a>(outcome: &'a AgentTaskOutcome, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let first = parts.next()?.trim();
    let mut current = match first {
        "outputs" => &outcome.outputs,
        "metadata" => &outcome.metadata,
        _ => return None,
    };
    for part in parts {
        current = current.get(part.trim())?;
    }
    Some(current).filter(|value| !value.is_null())
}

fn normalize_provider_artifact_roles(
    artifacts: &mut [AgentTaskArtifact],
    role_aliases: &AgentTaskProviderRoleAliases,
) {
    for artifact in artifacts {
        let Some(role) = role_aliases.role_for_artifact_kind(&artifact.kind) else {
            continue;
        };
        let original_kind = artifact.kind.clone();
        artifact.kind = role.to_string();
        if !artifact.metadata.is_object() {
            artifact.metadata = json!({});
        }
        if let Some(metadata) = artifact.metadata.as_object_mut() {
            metadata.entry("role".to_string()).or_insert(json!(role));
            metadata
                .entry("provider_kind".to_string())
                .or_insert(json!(original_kind));
        }
    }
}

fn normalize_provider_run_result_output(
    outcome: &mut AgentTaskOutcome,
    role_aliases: &AgentTaskProviderRoleAliases,
) {
    if output_value(&outcome.outputs, "provider_run_result").is_some() {
        return;
    }

    let value = role_aliases
        .output_aliases_for_role("provider_run_result")
        .into_iter()
        .find_map(|alias| output_value(&outcome.outputs, alias))
        .or_else(|| {
            role_aliases
                .metadata_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| output_value(&outcome.metadata, alias))
        });

    if let Some(value) = value.cloned() {
        let mut outputs = outcome.outputs.as_object().cloned().unwrap_or_default();
        outputs.insert("provider_run_result".to_string(), value);
        outcome.outputs = Value::Object(outputs);
    }
}

fn output_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|value| !value.is_null())
}
