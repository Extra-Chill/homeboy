//! Collection, ranking, and bounded rendering of durable failure evidence.

use super::*;
use homeboy::core::error::posix_quote_arg;

pub(super) fn collect_hydrated_evidence_diagnostics(
    task_id: &str,
    evidence: &AgentTaskEvidenceRef,
    diagnostics: &mut Vec<CollectedDiagnostic>,
    budget: &mut DiagnosticCollectionBudget,
) -> Option<Value> {
    if budget.evidence_refs >= DIAGNOSTIC_EVIDENCE_REF_LIMIT {
        return Some(budget.truncation(task_id, evidence, "evidence_refs"));
    }
    budget.evidence_refs += 1;
    let Some(value) = agent_task_service::hydrate_evidence_diagnostics(evidence) else {
        return None;
    };
    let count = value
        .pointer("/usage/diagnostic_count")
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize;
    let bytes = value
        .pointer("/usage/diagnostic_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize;
    if budget.diagnostics.saturating_add(count) > DIAGNOSTIC_COLLECTION_COUNT_LIMIT {
        return Some(budget.truncation(task_id, evidence, "diagnostic_count"));
    }
    if budget.bytes.saturating_add(bytes) > DIAGNOSTIC_COLLECTION_BYTE_LIMIT {
        return Some(budget.truncation(task_id, evidence, "diagnostic_bytes"));
    }
    budget.diagnostics += count;
    budget.bytes += bytes;
    collect_nested_diagnostics(
        task_id,
        &json!({ "diagnostics": value.get("diagnostics") }),
        "hydrated_evidence",
        diagnostics,
    );
    collect_process_stream_diagnostics(
        task_id,
        value.get("process_streams").unwrap_or(&Value::Null),
        diagnostics,
    );
    (value
        .pointer("/truncation/truncated")
        .and_then(Value::as_bool)
        == Some(true))
    .then(|| {
        json!({
            "task_id": bounded_diagnostic_value(&Value::String(task_id.to_string())),
            "kind": bounded_diagnostic_value(&Value::String(evidence.kind.clone())),
            "uri": bounded_diagnostic_value(&Value::String(evidence.uri.clone())),
            "truncation": value.get("truncation"),
        })
    })
}

pub(super) const DIAGNOSTIC_EVIDENCE_REF_LIMIT: usize = 64;
pub(super) const DIAGNOSTIC_COLLECTION_COUNT_LIMIT: usize = 256;
pub(super) const DIAGNOSTIC_COLLECTION_BYTE_LIMIT: usize = 256 * 1024;

#[derive(Default)]
pub(super) struct DiagnosticCollectionBudget {
    pub(super) evidence_refs: usize,
    pub(super) diagnostics: usize,
    pub(super) bytes: usize,
}

impl DiagnosticCollectionBudget {
    pub(super) fn truncation(
        &self,
        task_id: &str,
        evidence: &AgentTaskEvidenceRef,
        reason: &str,
    ) -> Value {
        json!({
            "task_id": bounded_diagnostic_value(&Value::String(task_id.to_string())),
            "kind": bounded_diagnostic_value(&Value::String(evidence.kind.clone())),
            "uri": bounded_diagnostic_value(&Value::String(evidence.uri.clone())),
            "truncation": {
                "truncated": true,
                "reason": reason,
                "evidence_ref_limit": DIAGNOSTIC_EVIDENCE_REF_LIMIT,
                "diagnostic_count_limit": DIAGNOSTIC_COLLECTION_COUNT_LIMIT,
                "diagnostic_byte_limit": DIAGNOSTIC_COLLECTION_BYTE_LIMIT,
            },
        })
    }
}

pub(super) fn diagnostic_collection_projection(truncations: &[Value]) -> Value {
    json!({
        "truncated_evidence": truncations.len(),
        "truncations": truncations.iter().take(COMPACT_REF_LIMIT).collect::<Vec<_>>(),
        "truncations_omitted": truncations.len().saturating_sub(COMPACT_REF_LIMIT),
    })
}

/// Cap the number of surfaced failure reasons so a pathological run with
/// hundreds of nested diagnostics cannot flood the failure summary. Overflow is
/// still available in the full nested payload (`--full` / aggregate file).
pub(super) const FAILURE_REASON_LIMIT: usize = 8;

/// Build a prominent, top-level "failure reasons" summary for a failed run
/// (#3806). The actual root cause of an agent-task failure (recipe validation
/// issue, PHP fatal, provider registration error, missing path) is otherwise
/// buried deep in nested outcome JSON — both in the typed
/// `outcomes[].diagnostics[]` and in provider-specific nested structures.
///
/// This collects diagnostics from BOTH the typed field and any nested
/// `diagnostics[]` arrays found anywhere in each outcome's `outputs`/`metadata`,
/// dedupes by `(class, message)`, and orders them so the most actionable
/// root-cause classes (validation / fatal / registration / missing-path) appear
/// first. The full nested JSON is left untouched; this only ADDS a surfaced
/// summary so operators see WHY a run failed without hand-digging.
pub(crate) fn failure_reasons_from_aggregate(aggregate: &AgentTaskAggregate) -> Vec<Value> {
    ranked_diagnostics(aggregate_failure_diagnostics(aggregate))
        .into_iter()
        .take(FAILURE_REASON_LIMIT)
        .map(|item| {
            json!({
                "task_id": item.task_id,
                "class": item.class,
                "message": item.message,
                "source": item.source,
            })
        })
        .collect()
}

pub(super) fn aggregate_failure_diagnostics(
    aggregate: &AgentTaskAggregate,
) -> Vec<CollectedDiagnostic> {
    let mut collected = Vec::new();

    // Failure reasons only describe failed outcomes. Scanning successful
    // outcomes incorrectly promoted provider success diagnostics (including
    // exit status zero) into failure summaries.
    let failed_first = aggregate.outcomes.iter().filter(|outcome| {
        matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Failed
                | AgentTaskOutcomeStatus::ProviderError
                | AgentTaskOutcomeStatus::Timeout
                | AgentTaskOutcomeStatus::UnableToRemediate
                | AgentTaskOutcomeStatus::Cancelled
        )
    });
    let scan: Vec<&homeboy::agents::agent_task::AgentTaskOutcome> = failed_first.collect();

    // An agent that declared its task incomplete already said why; its words
    // outrank whatever the provider stream happened to end with.
    for outcome in &aggregate.outcomes {
        if let Some(reason) = agent_task_service::agent_incomplete_reason(&outcome.outputs) {
            collected.push(CollectedDiagnostic {
                task_id: outcome.task_id.clone(),
                class: "agent.incomplete".to_string(),
                message: reason,
                source: "review_form".to_string(),
                data: Value::Null,
            });
        }
    }

    for outcome in scan {
        for diagnostic in &outcome.diagnostics {
            collected.push(CollectedDiagnostic {
                task_id: outcome.task_id.clone(),
                class: diagnostic.class.clone(),
                message: diagnostic.message.clone(),
                source: "diagnostics".to_string(),
                data: diagnostic.data.clone(),
            });
        }
        collect_nested_diagnostics(
            &outcome.task_id,
            &outcome.outputs,
            "outputs",
            &mut collected,
        );
        collect_nested_diagnostics(
            &outcome.task_id,
            &outcome.metadata,
            "metadata",
            &mut collected,
        );
    }

    collected
}

pub(super) fn ranked_diagnostics(collected: Vec<CollectedDiagnostic>) -> Vec<CollectedDiagnostic> {
    // A provider that exited 0 makes every raw, unparsed process-stream
    // excerpt (progress heartbeats, not a diagnostics payload) causally
    // meaningless: it is a slice of the progress-event stream, not an
    // explanation. When the provider succeeded, a later phase is the root
    // cause; root-cause attribution must never select the heartbeat noise
    // instead (#14735). Structured provider errors and nested diagnostics
    // extracted *from* a stream still carry real signal and are unaffected.
    let provider_succeeded = collected
        .iter()
        .any(|item| is_successful_process_exit(&format!("{} {}", item.class, item.message)));
    let is_unparsed_stream_noise = |item: &CollectedDiagnostic| {
        item.source == "hydrated_process_stream" && item.class == "provider.process_stream"
    };
    // Dedupe by (class, message) keeping the first occurrence, then order the
    // most actionable root-cause diagnostics first.
    let mut deduped: Vec<CollectedDiagnostic> = Vec::new();
    for item in collected {
        if provider_succeeded && is_unparsed_stream_noise(&item) {
            continue;
        }
        let trimmed = item.message.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = (
            item.task_id.clone(),
            item.class.to_ascii_lowercase(),
            trimmed.to_string(),
            item.source.clone(),
        );
        if let Some(existing) = deduped.iter_mut().find(|existing| {
            (
                existing.task_id.clone(),
                existing.class.to_ascii_lowercase(),
                existing.message.trim().to_string(),
                existing.source.clone(),
            ) == key
        }) {
            merge_diagnostic_data(&mut existing.data, item.data);
            continue;
        }
        deduped.push(item);
    }

    deduped.sort_by_key(diagnostic_priority);
    deduped
}

pub(super) fn merge_diagnostic_data(existing: &mut Value, incoming: Value) {
    match (existing, incoming) {
        (Value::Object(existing), Value::Object(incoming)) => {
            for (key, value) in incoming {
                let current = existing.entry(key).or_insert(Value::Null);
                if current.is_null() || diagnostic_data_size(&value) > diagnostic_data_size(current)
                {
                    *current = value;
                }
            }
        }
        (current, incoming)
            if current.is_null()
                || diagnostic_data_size(&incoming) > diagnostic_data_size(current) =>
        {
            *current = incoming;
        }
        _ => {}
    }
}

pub(super) fn diagnostic_data_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |value| value.len())
}

#[derive(Clone)]
pub(super) struct CollectedDiagnostic {
    pub(super) task_id: String,
    pub(super) class: String,
    pub(super) message: String,
    pub(super) source: String,
    pub(super) data: Value,
}

/// Lower number = higher priority. Explicit provider-contract failures must
/// precede an otherwise successful provider-process observation: the latter
/// describes the wrapper, not why the task failed.
pub(super) fn diagnostic_priority(item: &CollectedDiagnostic) -> (u8, u8, u8) {
    // Recorded delivery/terminal-operation blockers and the review form are the run's own verdict.
    // Text-matched heuristics below (including untrusted stream output that
    // happens to say "valid" or "schema") can never outrank them.
    let recorded_verdict = matches!(
        item.source.as_str(),
        "current_lifecycle"
            | "terminal_operation_failure"
            | "review_form"
            | "controller_failure"
            | "gate_failure"
    );
    let class = item.class.to_ascii_lowercase();
    let text = format!("{} {}", item.class, item.message).to_ascii_lowercase();
    // Typed build skew explains every downstream continuation failure.
    let priority = if recorded_verdict || class == RUNNER_VERSION_SKEW_CLASS {
        0
    } else if item.source == "hydrated_process_stream" && class == "provider.process_stream" {
        // An unparsed stream tail is whatever the provider printed last,
        // often its own prompt. Its words are not evidence of a cause.
        4
    } else if class == "agent_task.provider_liveness_timeout" {
        // The typed lifecycle timeout is authoritative. An unparsed provider
        // stream can contain arbitrary words such as "error" and must not
        // replace the recorded terminal state as the root cause.
        0
    } else if is_policy_denial(&class, &text) {
        0
    } else if is_provider_structured_error(&class) {
        // The provider's own terminal error event, already normalized by the
        // provider adapter: the most specific execution-layer cause there is.
        1
    } else if is_required_output_diagnostic(&class) {
        // Missing required outputs are retained as a consequence, but a typed
        // provider rejection explains why those outputs were never produced.
        2
    } else if is_provider_contract_diagnostic(&class) {
        3
    } else if is_successful_process_exit(&text) {
        // A successful provider process can be useful context, but it cannot
        // explain why the task failed.
        10
    } else if text.contains("provider_malformed_json")
        || text.contains("malformed executor")
        || text.contains("outcome-normalization")
    {
        8
    } else if text.contains("typed_artifacts_missing")
        || text.contains("required_typed_artifacts_missing")
        || text.contains("required typed artifacts")
        || text.contains("declared artifact result envelope")
    {
        8
    } else if text.contains("valid") || text.contains("recipe") || text.contains("schema") {
        0
    } else if text.contains("fatal") || text.contains("error") || text.contains("exception") {
        1
    } else if text.contains("registr")
        || text.contains("provider")
        || text.contains("discovery")
        || text.contains("capability")
    {
        2
    } else if text.contains("missing")
        || text.contains("not_found")
        || text.contains("path")
        || text.contains("io")
    {
        3
    } else if item.source == "hydrated_process_stream" {
        4
    } else {
        9
    };
    // Stream provenance is causal only among equally actionable diagnostics;
    // it must not hide a stronger typed validation or fatal failure.
    (
        u8::from(!recorded_verdict),
        priority,
        u8::from(item.source != "hydrated_process_stream"),
    )
}

/// Process streams are untrusted wrapper output. When a stream is JSON, its
/// diagnostics are the execution-layer cause and therefore rank ahead of the
/// wrapper's outcome-normalization consequence. A stream carrying a
/// normalized structured provider error (already converted by the provider
/// adapter) is promoted with its message, status code, retryability, and
/// account/quota classification instead of collapsing to an exit code
/// (#13703).
pub(super) fn collect_process_stream_diagnostics(
    task_id: &str,
    streams: &Value,
    out: &mut Vec<CollectedDiagnostic>,
) {
    for stream in streams.as_array().into_iter().flatten() {
        if let Some(error) = stream
            .get("structured_error")
            .and_then(normalized_structured_error)
        {
            out.push(structured_error_diagnostic(task_id, &error));
            continue;
        }
        let Some(excerpt) = stream.get("excerpt").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str(excerpt) {
            collect_nested_diagnostics(task_id, &value, "hydrated_process_stream", out);
            continue;
        }
        if !excerpt.trim().is_empty() {
            out.push(CollectedDiagnostic {
                task_id: task_id.to_string(),
                class: "provider.process_stream".to_string(),
                message: excerpt.to_string(),
                source: "hydrated_process_stream".to_string(),
                data: Value::Null,
            });
        }
    }
}

/// Project a normalized structured provider error as a diagnostic whose
/// message carries the provider's own answer: the human-readable message, the
/// HTTP status, and whether the provider declared the failure retryable.
pub(super) fn structured_error_diagnostic(task_id: &str, error: &Value) -> CollectedDiagnostic {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let status_code = error.get("status_code").and_then(Value::as_i64);
    let retryable = error.get("retryable").and_then(Value::as_bool);
    let status_note = status_code
        .map(|code| format!("HTTP {code}, "))
        .unwrap_or_default();
    let retry_note = match retryable {
        Some(true) => "retryable",
        Some(false) => "not retryable",
        None => "retryability unspecified",
    };
    CollectedDiagnostic {
        task_id: task_id.to_string(),
        class: "provider.structured_error".to_string(),
        message: format!("provider rejected the request ({status_note}{retry_note}): {message}"),
        source: "hydrated_process_stream".to_string(),
        data: json!({
            "status_code": status_code,
            "retryable": retryable,
            "failure_classification": error.get("failure_classification"),
            "error_name": error.get("error_name"),
        }),
    }
}

/// Recursively walk a provider-specific JSON value looking for `diagnostics`
/// arrays of objects carrying a `message` (and optional `class`). This is how
/// provider-owned runtime diagnostics get surfaced without the renderer needing
/// to know the exact provider path.
pub(super) fn collect_nested_diagnostics(
    task_id: &str,
    value: &Value,
    source: &str,
    out: &mut Vec<CollectedDiagnostic>,
) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("diagnostics") {
                for item in items {
                    if let Some(message) = item
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    {
                        let class = item
                            .get("class")
                            .or_else(|| item.get("kind"))
                            .or_else(|| item.get("level"))
                            .and_then(Value::as_str)
                            .unwrap_or("nested")
                            .to_string();
                        out.push(CollectedDiagnostic {
                            task_id: task_id.to_string(),
                            class,
                            message,
                            source: source.to_string(),
                            data: item.get("data").cloned().unwrap_or(Value::Null),
                        });
                    }
                }
            }
            for nested in map.values() {
                collect_nested_diagnostics(task_id, nested, source, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_nested_diagnostics(task_id, nested, source, out);
            }
        }
        _ => {}
    }
}

pub(super) fn lab_staging_root_cause(record: &AgentTaskRunRecord) -> Option<CollectedDiagnostic> {
    let diagnostic = record.metadata.get("lab_staging_diagnostic")?;
    if diagnostic.get("schema")?.as_str()? != "homeboy/lab-staging-diagnostic/v1"
        || diagnostic.get("cause")?.as_str()? != "json_parse"
    {
        return None;
    }
    let phase = diagnostic.get("phase")?.as_str()?;
    let attachment_kind = diagnostic.get("attachment_kind")?.as_str()?;
    let parse = diagnostic.get("parse")?;
    let category = parse.get("category")?.as_str()?;
    if phase != "loading_recipe"
        || attachment_kind != "lab-staging-recipe"
        || !matches!(category, "syntax" | "data" | "eof" | "io")
    {
        return None;
    }
    let line = parse.get("line")?.as_u64()?;
    let column = parse.get("column")?.as_u64()?;
    let message = format!(
        "Lab staging recipe JSON could not be parsed ({category}, line {line}, column {column}); the failing field is unknown"
    );
    Some(CollectedDiagnostic {
        task_id: "controller".to_string(),
        class: "lab_staging.json_parse".to_string(),
        message,
        source: "current_lifecycle".to_string(),
        data: json!({
            "phase": phase,
            "cause": "json_parse",
            "attachment_kind": attachment_kind,
            "category": category,
            "line": line,
            "column": column,
            "field": null,
            "field_status": "unknown",
            "next_action": "Repair or recreate the private Lab staging recipe, then retry the run.",
        }),
    })
}

/// Class of a controller/runner Homeboy build mismatch, shared with the typed
/// gate and runner errors.
pub(super) const RUNNER_VERSION_SKEW_CLASS: &str = "runner.version_skew";

/// Find the runner's provenance build-skew refusal anywhere the run retained
/// it: collected diagnostics, the durable record, or the runner job snapshot.
///
/// Records written before the refusal was typed carry it only as text, while
/// the Cook's own failure reads `internal.unexpected: terminal Cook
/// continuation exited with status 1` (#18206). The skew is the cause, and the
/// remedy is converging the runner, not retrying the same dispatch.
pub(super) fn provenance_build_skew_root_cause(
    record: &AgentTaskRunRecord,
    collected: &[CollectedDiagnostic],
    runner_probe: &impl Serialize,
) -> Option<CollectedDiagnostic> {
    use homeboy::core::build_identity::ProvenanceBuildSkew;

    let skew = collected
        .iter()
        .find_map(|item| {
            ProvenanceBuildSkew::parse(&item.message)
                .or_else(|| ProvenanceBuildSkew::parse(&item.data.to_string()))
        })
        .or_else(|| ProvenanceBuildSkew::parse(&record.metadata.to_string()))
        .or_else(|| {
            serde_json::to_string(runner_probe)
                .ok()
                .and_then(|probe| ProvenanceBuildSkew::parse(&probe))
        })?;
    let runner_id = record.runner_id();
    let runner = runner_id
        .map(|runner| format!("runner `{runner}`"))
        .unwrap_or_else(|| "the runner".to_string());
    Some(CollectedDiagnostic {
        task_id: "controller".to_string(),
        class: RUNNER_VERSION_SKEW_CLASS.to_string(),
        message: format!(
            "controller/runner build skew: {runner} daemon is Homeboy `{}` but its Lab job binary is Homeboy `{}`, so the daemon refused the job's provenance references",
            skew.runner_build, skew.child_build
        ),
        source: "runner_build_skew".to_string(),
        data: json!({
            "runner_id": runner_id,
            "child_build_identity": skew.child_build,
            "runner_build_identity": skew.runner_build,
            "controller_build_identity": homeboy::core::build_identity::current().display,
            "convergence_commands": skew.convergence_commands(runner_id),
            "next_action": skew.next_action(runner_id),
        }),
    })
}

/// Converge the runner first; retrying on the same skewed runner fails again.
pub(super) fn provenance_build_skew_next_commands(
    record: &AgentTaskRunRecord,
    skew: &CollectedDiagnostic,
) -> Vec<String> {
    skew.data["convergence_commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .chain(std::iter::once(format!(
            "homeboy agent-task retry {}",
            posix_quote_arg(
                record.metadata["cook_id"]
                    .as_str()
                    .unwrap_or(record.run_id.as_str())
            )
        )))
        .collect()
}

pub(super) fn persisted_cook_failure_diagnostic(
    record: &AgentTaskRunRecord,
) -> Option<CollectedDiagnostic> {
    let terminal = record
        .metadata
        .get("cook_operation_claims")
        .and_then(Value::as_array)
        .and_then(|claims| {
            claims.iter().rev().find(|claim| {
                claim.get("state").and_then(Value::as_str) == Some("failed")
                    && claim
                        .get("operation_key")
                        .and_then(Value::as_str)
                        .is_some_and(|key| {
                            key.starts_with("promote:") || key.starts_with("finalize:")
                        })
            })
        })
        .and_then(|claim| claim.get("result"));
    if let Some(diagnostic) = terminal {
        let cause = diagnostic
            .get("deepest_cause")
            .filter(|value| value.is_object())
            .unwrap_or(diagnostic);
        return Some(CollectedDiagnostic {
            task_id: "controller".to_string(),
            class: cause.get("code")?.as_str()?.to_string(),
            message: cause.get("message")?.as_str()?.to_string(),
            source: "terminal_operation_failure".to_string(),
            data: diagnostic.get("details").cloned().unwrap_or_else(|| {
                json!({
                    "field": cause.get("field"),
                })
            }),
        });
    }
    if let Some(failure) = record.metadata.get("interrupted_owner") {
        return Some(CollectedDiagnostic {
            task_id: "controller".to_string(),
            class: "interrupted_owner".to_string(),
            message: failure
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    agent_task_lifecycle::interrupted_owner_stop_reason(
                        failure["provider_executions_consumed"]
                            .as_u64()
                            .unwrap_or(1),
                    )
                })
                .to_string(),
            source: "interrupted_owner".to_string(),
            data: failure.clone(),
        });
    }
    if let Some(failure) = record.metadata.get("execution_step_failure") {
        let diagnostic = &failure["diagnostic"];
        return Some(CollectedDiagnostic {
            task_id: "controller".to_string(),
            class: diagnostic["code"]
                .as_str()
                .unwrap_or("execution_step_failure")
                .to_string(),
            message: failure["message"]
                .as_str()
                .unwrap_or("execution step failed")
                .to_string(),
            source: "execution_step_failure".to_string(),
            data: diagnostic.get("details").cloned().unwrap_or(Value::Null),
        });
    }
    if let Some(failure) = record.metadata.get("pre_execution_failure") {
        let details = failure.get("details")?;
        if let Some(receipt) = lab_transport_receipt_from_details(details) {
            return Some(CollectedDiagnostic {
                task_id: "controller".to_string(),
                class: receipt.error.code.clone(),
                message: receipt.error.message.clone(),
                source: "lab_preacceptance_transport".to_string(),
                data: json!({
                    "phase": failure.get("phase"),
                    "lab_transport_attempt_receipt": receipt,
                    "source_error": details.get("source_error"),
                }),
            });
        }
        // Historical snapshot records include the producer's obsolete generic
        // retry command. Durable recovery projection owns that command now, so
        // retain the failure facts but never relay the stale hint as diagnosis.
        let details = diagnostic_pre_execution_details(details);
        return Some(CollectedDiagnostic {
            task_id: "controller".to_string(),
            class: failure
                .get("error_code")
                .and_then(Value::as_str)
                .unwrap_or("pre_execution_failure")
                .to_string(),
            message: failure
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Cook pre-execution failure")
                .to_string(),
            source: "pre_execution_failure".to_string(),
            data: json!({
                "phase": failure.get("phase"),
                "error_code": failure.get("error_code"),
                "provider_executions_consumed": failure.get("provider_executions_consumed"),
                "details": details,
            }),
        });
    }
    let diagnostic = record.metadata.get("cook_controller_failure")?;
    let cause = diagnostic
        .get("deepest_cause")
        .filter(|value| value.is_object())
        .unwrap_or(diagnostic);
    Some(CollectedDiagnostic {
        task_id: "controller".to_string(),
        class: cause.get("code")?.as_str()?.to_string(),
        message: cause.get("message")?.as_str()?.to_string(),
        source: "controller_failure".to_string(),
        data: diagnostic.get("details").cloned().unwrap_or_else(|| {
            json!({
                "field": cause.get("field"),
            })
        }),
    })
}

pub(super) fn diagnostic_pre_execution_details(details: &Value) -> Value {
    let mut details = details.clone();
    let is_snapshot_failure =
        details.get("classification").and_then(Value::as_str) == Some("snapshot_construction");
    if is_snapshot_failure {
        if let Some(recovery) = details.get_mut("recovery").and_then(Value::as_object_mut) {
            recovery.remove("command");
        }
    }
    details
}

/// The current delivery blocker, read through the Cook delivery model. Its
/// evidence is the persisted record the blocker was read from.
pub(super) fn current_lifecycle_diagnostic(
    record: &AgentTaskRunRecord,
) -> Option<CollectedDiagnostic> {
    let delivery = CookDelivery::from_run_metadata(&record.metadata)?;
    let blocker = delivery.blocker?;
    let (task_id, class) = match blocker.code {
        CookDeliveryBlockerCode::GateFailed => {
            ("promotion", "agent_task.promotion_gate_failed".to_string())
        }
        CookDeliveryBlockerCode::PublishRefused => (
            "finalization",
            blocker
                .cause
                .clone()
                .unwrap_or_else(|| "agent_task.finalization_failed".to_string()),
        ),
        code => ("delivery", format!("cook_delivery.{}", code.as_str())),
    };
    Some(CollectedDiagnostic {
        task_id: task_id.to_string(),
        class,
        message: blocker.message,
        source: "current_lifecycle".to_string(),
        data: record
            .metadata
            .pointer(&blocker.evidence)
            .cloned()
            .unwrap_or(Value::Null),
    })
}

/// The Cook failure status the recovery handoff admits for the current
/// delivery blocker, if any.
pub(super) fn current_lifecycle_status(record: &AgentTaskRunRecord) -> Option<CookStatus> {
    let delivery = CookDelivery::from_run_metadata(&record.metadata)?;
    delivery.blocker.as_ref()?;
    delivery.lifecycle().cook_status
}

/// Cancellation is controller-owned lifecycle evidence, even when the runner
/// never produced an aggregate. Preserve its typed reason rather than treating
/// a pre-provider cancellation as an unclassified provider failure.
pub(super) fn runner_cancellation_diagnostic(
    record: &AgentTaskRunRecord,
) -> Option<CollectedDiagnostic> {
    let reason = record.metadata.get("cancel_reason")?.as_str()?;
    if reason != "missing_runner_pid" {
        return None;
    }
    Some(CollectedDiagnostic {
        task_id: "runner".to_string(),
        class: "agent_task.runner_missing_pid".to_string(),
        message: "Runner-owned execution was cancelled before a runner PID was recorded."
            .to_string(),
        source: "runner_cancellation".to_string(),
        data: json!({
            "cancellation_reason": reason,
            "causal_phase": "runner_submission",
            "runner_id": record.runner_id(),
            "runner_job_id": record.runner_job_id(),
            "provider_executions_consumed": record.provider_executions_consumed(),
            "runner_execution_status": record.metadata.pointer("/runner_execution_record/status"),
        }),
    })
}

/// Admission is queueing, not a stalled provider execution. Preserve the same
/// driver-lock observation used by the recovery planner.
pub(super) fn attach_local_dispatch_wait(value: &mut Value, record: &AgentTaskRunRecord) {
    if record
        .metadata
        .pointer("/cook_progress/phase")
        .and_then(Value::as_str)
        != Some("queued_local_pressure")
    {
        return;
    }
    if let Some(wait) = record
        .metadata
        .pointer("/cook_progress/activity/local_dispatch_wait")
    {
        value["local_dispatch_wait"] = wait.clone();
        value["blocker"] = json!({
            "code": "local_pressure",
            "message": "Local dispatch waiting for host pressure to drop",
            "details": wait,
        });
        value["next_attempt_at"] = wait["next_attempt_at"].clone();
        value["waiting_for"] = json!("local_pressure");
    }
}

pub(super) fn attach_lab_admission_wait(
    value: &mut Value,
    admission: Option<&agent_task_service::QueuedAdmission>,
) {
    if let Some(wait) = admission
        .and_then(|admission| admission.wait.as_ref())
        .filter(|wait| wait["runner_eligible"].is_string())
    {
        value["execution_step_wait"] = wait.clone();
        value["reason"] = wait["message"].clone();
        value["summary"] = wait["message"].clone();
        value["blocker_summary"] = wait["message"].clone();
        value["waiting_for"] = json!("runner_eligible");
        value["next_commands"] = json!([wait["next_action"]]);
        return;
    }
    if let Some(wait) = admission
        .filter(|admission| admission.live_driver)
        .and_then(|admission| admission.wait.as_ref())
    {
        value["waiting_for"] = json!("lab_admission");
        value["lab_admission_wait"] = wait.clone();
        let message = format!(
            "Waiting for Lab admission on {}.",
            wait["runner_id"].as_str().unwrap_or("Lab")
        );
        value["reason"] = json!(message);
        value["summary"] = json!(message);
        value["diagnosis_outcome"] = json!("waiting_for_admission");
        if let Some(run_id) = value["run_id"].as_str() {
            let command = format!(
                "homeboy agent-task status {} --watch",
                posix_quote_arg(run_id)
            );
            value["next_commands"] = json!([command]);
        }
    }
}

/// A failed deterministic gate is the run's recorded verdict. Its output tail
/// (the gate's own report lines) is the cause a reader needs first.
pub(super) fn gate_failure_diagnostic(failure: &Value) -> CollectedDiagnostic {
    let reason = failure["reason"].as_str().unwrap_or("gate_failed");
    let exit = failure["exit_code"]
        .as_i64()
        .map(|code| format!(" (exit {code})"))
        .unwrap_or_default();
    let tail = failure["output_tail"].as_str().unwrap_or_default();
    CollectedDiagnostic {
        task_id: failure["gate"].as_str().unwrap_or("gate").to_string(),
        class: format!("gate.{reason}"),
        message: format!("{reason}{exit}\n{tail}").trim().to_string(),
        source: "gate_failure".to_string(),
        data: json!({ "command": failure["command"], "artifact": failure["artifact"] }),
    }
}

/// Callers pass only an orphaned queued admission (free driver lock and
/// reconcilable liveness), so this names the same owner `recover` resumes.
pub(super) fn queued_runner_ownership_diagnostic(
    record: &AgentTaskRunRecord,
    liveness: agent_task_service::AgentTaskLiveness,
    probe: &agent_task_lifecycle::AgentTaskRunnerDiagnosticProbe,
) -> Option<CollectedDiagnostic> {
    if record.state() != homeboy_lifecycle_contract::RunExecutionState::Queued {
        return None;
    }
    let phase = record.metadata.pointer("/cook_progress/phase");
    if record.runner_id().is_none() {
        // Before runner selection the Cook driver is the only owner.
        return Some(CollectedDiagnostic {
            task_id: "controller".to_string(),
            class: "agent_task.queued_driver_missing".to_string(),
            message: format!(
                "The Cook is queued at {} but no live driver owns it.",
                phase.and_then(Value::as_str).unwrap_or("admission")
            ),
            source: "controller_ownership".to_string(),
            data: json!({
                "causal_phase": phase,
                "liveness": liveness.as_str(),
                "liveness_reconcilable": liveness.is_reconcilable(),
                "ownership": "controller",
            }),
        });
    }
    (probe.skipped_reason == Some("missing_runner_job_id")).then(|| CollectedDiagnostic {
        task_id: "runner".to_string(),
        class: "agent_task.runner_missing_pid".to_string(),
        message: "Runner-owned execution is queued at provider_start without a runner PID."
            .to_string(),
        source: "runner_ownership".to_string(),
        data: json!({
            "causal_phase": phase,
            "liveness": liveness.as_str(),
            "liveness_reconcilable": liveness.is_reconcilable(),
            "ownership": "runner",
            "runner_id": record.runner_id(),
            "runner_job_id": record.runner_job_id(),
            "runner_execution_status": record.metadata.pointer("/runner_execution_record/status"),
        }),
    })
}

pub(super) fn is_successful_process_exit(text: &str) -> bool {
    text.contains("exit status 0")
        || text.contains("exited with status 0")
        || text.contains("exit_code: 0")
        || text.contains("exit_code=0")
}

pub(super) fn is_policy_denial(class: &str, text: &str) -> bool {
    class.contains("policy_denied")
        || class.contains("command_denied")
        || text.contains("policy denied")
        || text.contains("tool denied")
}

pub(super) fn is_required_output_diagnostic(class: &str) -> bool {
    class.contains("required_output_missing") || class.contains("required_outputs_missing")
}

pub(super) fn is_provider_structured_error(class: &str) -> bool {
    class.contains("provider.structured_error")
}

pub(super) fn is_provider_contract_diagnostic(class: &str) -> bool {
    class.contains("provider_outcome_contract_violation")
        || class.contains("outcome_contract_violation")
}

/// Preserve only the stable, actionable denial fields. Provider diagnostic data
/// may contain arbitrary command output, which does not belong in a root cause.
pub(super) fn policy_denial_details(data: &Value) -> Option<Value> {
    let fields = [
        "tool",
        "permission",
        "path",
        "requested_path",
        "canonical_path",
        "allowed_path",
        "workspace_path",
        "matched_pattern",
        "policy_mode",
        "reason",
    ];
    structured_details(data, &fields)
}

/// Preserve the bounded, actionable facts of a normalized structured provider
/// error — status code, retryability, and the account/quota classification
/// (#13691 consumes this classification for provider rotation) — without
/// forwarding arbitrary provider payload.
pub(super) fn structured_error_details(data: &Value) -> Option<Value> {
    structured_details(
        data,
        &[
            "status_code",
            "retryable",
            "failure_classification",
            "error_name",
        ],
    )
}

pub(super) fn structured_details(data: &Value, fields: &[&str]) -> Option<Value> {
    let details = fields.iter().filter_map(|field| {
        data.get(*field)
            .filter(|value| !value.is_null())
            .and_then(|value| {
                bounded_diagnostic_value(value).map(|value| (field.to_string(), value))
            })
    });
    let details = serde_json::Map::from_iter(details);
    (!details.is_empty()).then(|| Value::Object(details))
}

/// The diagnostic message is the causal answer `diagnose` exists to produce
/// (#14679): an oversized message must still name the failure, not disappear.
/// Unlike `bounded_diagnostic_value`'s atomic drop (correct for secondary
/// `details` payloads), truncate on a char boundary and keep the prefix.
pub(super) fn bounded_diagnostic_message(text: &str) -> Value {
    if text.chars().count() <= COMPACT_TEXT_LIMIT {
        return Value::String(text.to_string());
    }
    let mut truncated: String = text.chars().take(COMPACT_TEXT_LIMIT).collect();
    truncated.push('…');
    Value::String(truncated)
}

pub(super) fn bounded_diagnostic_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) if text.len() > COMPACT_TEXT_LIMIT => None,
        Value::Array(values) => Some(Value::Array(
            values
                .iter()
                .take(COMPACT_REF_LIMIT)
                .filter_map(bounded_diagnostic_value)
                .collect(),
        )),
        Value::Object(values) => Some(Value::Object(
            values
                .iter()
                .take(COMPACT_REF_LIMIT)
                .filter_map(|(key, value)| {
                    bounded_diagnostic_value(value).map(|value| (key.clone(), value))
                })
                .collect(),
        )),
        _ => Some(value.clone()),
    }
}

pub(super) fn diagnostic_owner(class: &str, source: &str, data: &Value) -> &'static str {
    let class = class.to_ascii_lowercase();
    if source == "pre_execution_failure"
        && data.get("phase").and_then(Value::as_str) == Some("controller_admission")
    {
        "controller_runtime"
    } else if source == "lab_preacceptance_transport" {
        "lab_transport"
    } else if source == "hydrated_process_stream" {
        "provider_runtime"
    } else if class.contains("malformed") || class.contains("normalization") {
        "executor_wrapper"
    } else if class.contains("provider") || class.contains("runtime") {
        "provider_runtime"
    } else {
        "agent_task"
    }
}

pub(super) fn missing_artifact_summaries(aggregate: &AgentTaskAggregate) -> Vec<Value> {
    aggregate
        .outcomes
        .iter()
        .filter_map(|outcome| {
            let expected: Vec<String> = outcome
                .metadata
                .get("expected_artifacts")
                .or_else(|| outcome.outputs.get("expected_artifacts"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let produced: std::collections::HashSet<String> = outcome
                .typed_artifacts
                .iter()
                .map(|artifact| artifact.name.clone())
                .collect();
            let missing: Vec<String> = expected
                .into_iter()
                .filter(|name| !produced.contains(name))
                .collect();
            (!missing.is_empty()).then(|| {
                json!({
                    "task_id": outcome.task_id,
                    "missing": missing,
                })
            })
        })
        .collect()
}

pub(super) fn causal_chain_from_aggregate(aggregate: &AgentTaskAggregate) -> Vec<Value> {
    aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            json!({
                "task_id": outcome.task_id,
                "surface": "agent-task",
                "status": outcome.status,
                "failure_classification": outcome.failure_classification,
                "provider_summary": outcome.summary,
                "evidence_kinds": outcome.evidence_refs.iter().map(|evidence| evidence.kind.clone()).collect::<Vec<_>>(),
            })
        })
        .collect()
}
