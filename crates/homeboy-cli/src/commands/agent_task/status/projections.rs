//! Bounded operator projections, redaction, and control-plane evidence deduplication.

use super::*;

/// Default terminal projection for cancellation. The complete, redacted action
/// acknowledgement remains available through `--full` and `--output`.
pub(crate) fn bounded_cancel_report(value: Value) -> Value {
    let cancellation = value
        .pointer("/result/data")
        .cloned()
        .unwrap_or(Value::Null);
    let run_id = cancellation
        .get("run_id")
        .or_else(|| value.get("run"))
        .and_then(Value::as_str)
        .filter(|run_id| run_id.len() <= COMPACT_TEXT_LIMIT)
        .unwrap_or("<oversized-run-id>");
    let status_command = format!("homeboy agent-task status {}", quote_arg(run_id));
    let projection = json!({
        "schema": homeboy_control_plane_contract::CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
        "presentation": "bounded_operator_projection",
        "run": {
            "requested": bounded_value(value.get("run").unwrap_or(&Value::Null)),
            "id": run_id,
            "state": bounded_value(value.pointer("/resource/state").unwrap_or(&Value::Null)),
        },
        "cancellation": {
            "acknowledgement": bounded_value(value.get("acknowledgement").unwrap_or(&Value::Null)),
            "accepted": bounded_value(value.get("outcome").unwrap_or(&Value::Null)),
            "disposition": bounded_value(cancellation.get("disposition").unwrap_or(&Value::Null)),
            "terminal": bounded_value(cancellation.get("terminal").unwrap_or(&Value::Null)),
            "waited_seconds": bounded_value(cancellation.get("waited_seconds").unwrap_or(&Value::Null)),
            "message": bounded_value(value.get("message").unwrap_or(&Value::Null)),
        },
        "next_action": { "command": status_command },
        "output_budget": {
            "max_bytes": CANCEL_PROJECTION_BYTE_LIMIT,
            "full_output": "--full",
            "lossless_output": "--output <path>",
        },
    });
    let projection = homeboy::core::redaction::RedactionPolicy::default().redact_json(&projection);
    if serialized_len(&projection) <= CANCEL_PROJECTION_BYTE_LIMIT {
        projection
    } else {
        json!({
            "schema": homeboy_control_plane_contract::CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
            "presentation": "bounded_operator_projection",
            "run": { "id": "<oversized-run-id>" },
            "next_action": { "command": "homeboy agent-task status <run-id>" },
            "output_budget": { "max_bytes": CANCEL_PROJECTION_BYTE_LIMIT, "full_output": "--full", "lossless_output": "--output <path>" },
        })
    }
}

/// `--output` retains the lossless report. Terminal stdout instead carries a
/// bounded, deduplicated view with the decision and recovery command first.
pub(crate) fn bounded_full_operation_report(value: Value, operation: &str) -> Value {
    let run_id = value
        .get("run_id")
        .or_else(|| value.get("latest_run_id"))
        .or_else(|| value.pointer("/source/run_id"))
        .and_then(Value::as_str)
        .unwrap_or("<run-id>");
    // An invalid oversized identifier must not defeat the terminal budget.
    // The complete identifier remains available in the lossless output artifact.
    let run_id = (run_id.len() <= COMPACT_TEXT_LIMIT)
        .then_some(run_id)
        .unwrap_or("<oversized-run-id>");
    let status = value
        .get("status")
        .or_else(|| value.get("state"))
        .or_else(|| value.pointer("/handoff/boundary"))
        .cloned()
        .unwrap_or(Value::Null);
    let pr_url = value
        .get("pr_url")
        .or_else(|| value.pointer("/finalization/pr_url"))
        .or_else(|| value.pointer("/handoff/pr_url"))
        .or_else(|| value.pointer("/cook_completion/pr_url"))
        .cloned()
        .unwrap_or(Value::Null);
    let next_command = value
        .pointer("/handoff/finalize_command")
        .or_else(|| value.get("continuation_command"))
        .or_else(|| value.pointer("/failure_context/next_action/command"))
        .or_else(|| value.pointer("/failure_context/next_actions/0/command"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("homeboy agent-task status {}", quote_arg(run_id)));
    let blocker = value
        .pointer("/failure_context/diagnostic/message")
        .or_else(|| value.pointer("/blocking_claim/message"))
        .or_else(|| value.get("error"))
        .map(bounded_value)
        .unwrap_or(Value::Null);
    let source_schema = value
        .get("schema")
        .filter(|schema| {
            schema
                .as_str()
                .is_some_and(|schema| schema.len() <= COMPACT_TEXT_LIMIT)
        })
        .cloned()
        .unwrap_or(Value::Null);
    let evidence_command = format!("homeboy runs evidence {} --full", quote_arg(run_id));
    let mut evidence_refs = stable_evidence_refs(&value);
    if evidence_refs.is_empty() {
        evidence_refs.push(json!({
            "run_id": run_id,
            "ref": format!("homeboy://agent-task/run/{}/evidence", homeboy::core::execution_contract::encode_uri_component(run_id)),
            "command": evidence_command,
            "export_command": format!("{evidence_command} --output <path>"),
        }));
    }
    let mut output = json!({
        "actionable": {
            "operation": operation,
            "terminal_state": bounded_value(&status),
            "pr_url": bounded_value(&pr_url),
            "blocker": blocker,
            "next_action": { "command": bounded_value(&Value::String(next_command)) },
        },
        "schema": source_schema,
        "presentation": "bounded_operator_projection",
        "run_id": bounded_value(&Value::String(run_id.to_string())),
        "status": bounded_value(&status),
        "evidence_refs": evidence_refs,
        "output_budget": {
            "max_bytes": BOUNDED_FULL_STATUS_BYTE_LIMIT,
            "lossless_output": "--output <path>",
            "deduplicated": true,
            "truncated": true,
        },
    });
    if operation == "recover" {
        output["step"] = bounded_value(&value["step"]);
    }
    if operation == "cook" {
        if let Some(candidate) = value.get("durable_candidate") {
            output["durable_candidate"] = candidate.clone();
        }
        if let Some(selected) = value.get("selected_candidate") {
            output["selected_candidate"] = compact_fields(
                selected,
                &["run_id", "selected_task_id", "selected_candidate_sha"],
            );
        }
    }
    if serialized_len(&output) <= BOUNDED_FULL_STATUS_BYTE_LIMIT {
        output
    } else {
        json!({
            "actionable": {
                "operation": operation,
                "terminal_state": bounded_value(&status),
                "next_action": { "command": format!("homeboy agent-task status {}", quote_arg(run_id)) },
            },
            "schema": source_schema,
            "output_budget": { "max_bytes": BOUNDED_FULL_STATUS_BYTE_LIMIT, "truncated": true },
        })
    }
}

/// Preserve a bounded set of stable evidence identities without repeating their
/// gate/environment/proof payloads. The traversal accepts legacy nesting.
pub(super) fn stable_evidence_refs(value: &Value) -> Vec<Value> {
    fn visit(value: &Value, refs: &mut Vec<Value>, seen: &mut HashSet<String>) {
        if refs.len() >= COMPACT_REF_LIMIT {
            return;
        }
        match value {
            Value::Array(values) => values.iter().for_each(|value| visit(value, refs, seen)),
            Value::Object(values) => {
                if let Some(evidence) = values.get("evidence_refs").and_then(Value::as_array) {
                    for item in evidence {
                        let reference = match item {
                            Value::String(reference) => Some(reference.clone()),
                            Value::Object(item) => ["ref", "uri", "id", "path"]
                                .iter()
                                .find_map(|key| item.get(*key).and_then(Value::as_str))
                                .map(str::to_string),
                            _ => None,
                        };
                        if let Some(reference) =
                            reference.filter(|reference| reference.len() <= COMPACT_TEXT_LIMIT)
                        {
                            if seen.insert(reference.clone()) {
                                let run_id = item
                                    .as_object()
                                    .and_then(|item| item.get("run_id"))
                                    .and_then(Value::as_str);
                                let mut projected = json!({ "ref": reference });
                                if let Some(run_id) = run_id {
                                    projected["run_id"] = json!(run_id);
                                }
                                refs.push(projected);
                            }
                        }
                    }
                }
                values.values().for_each(|value| visit(value, refs, seen));
            }
            _ => {}
        }
    }

    let mut refs = Vec::new();
    visit(value, &mut refs, &mut HashSet::new());
    refs
}

/// Keep lifecycle status useful at a glance while leaving the canonical
/// eligibility report and evidence available through `--full`.
pub(super) fn compact_control_plane_status(value: Value) -> Value {
    let run_id = value
        .get("run")
        .and_then(Value::as_str)
        .filter(|run_id| run_id.len() <= COMPACT_TEXT_LIMIT)
        .unwrap_or("<oversized-run-id>");
    let actions = value
        .pointer("/action_eligibility/actions")
        .and_then(Value::as_array);
    let available = actions
        .map(|items| {
            items
                .iter()
                .filter(|action| action["availability"] == "available")
                .count()
        })
        .unwrap_or_default();
    let next_action = value
        .pointer("/next_action/command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            crate::commands::agent_task_summary::control_plane_next_action(&value, run_id)
        });
    let next_action = if next_action.len() <= COMPACT_COMMAND_LIMIT {
        next_action
    } else {
        "homeboy runs evidence <run-id>".to_string()
    };
    let next_action_name = next_action
        .split_whitespace()
        .skip_while(|part| *part != "agent-task")
        .nth(1);
    let selected_eligibility_action = actions.and_then(|actions| {
        actions.iter().find(|action| {
            action["availability"] == "available" && action["action"].as_str() == next_action_name
        })
    });
    // Cook recovery is a planner command, not a ControlPlaneAction variant.
    // Keep the report's existing action vocabulary when showing recovery,
    // rather than filtering it to an empty list by a nonexistent enum value.
    let recovery_selected = next_action_name == Some("recover")
        && value
            .pointer("/recovery/step")
            .and_then(Value::as_str)
            .is_some_and(|step| step != "none")
        && actions.is_some_and(|actions| !actions.is_empty());
    let selected_action = selected_eligibility_action
        .map(|action| {
            json!([{
                "action": action.get("action"),
                "availability": "available"
            }])
        })
        .unwrap_or_else(|| {
            if recovery_selected {
                json!(actions
                    .unwrap()
                    .iter()
                    .map(|action| json!({
                        "action": action.get("action"),
                        "availability": action.get("availability")
                    }))
                    .collect::<Vec<_>>())
            } else {
                json!([])
            }
        });
    let blocker = bounded_control_plane_value(value.get("blocker").unwrap_or(&Value::Null));
    let blocker = if let Some(message) = value.pointer("/blocker/message").and_then(Value::as_str) {
        let mut blocker = blocker;
        blocker["message"] = bounded_diagnostic_message(message);
        blocker
    } else {
        blocker
    };
    let mut compact = json!({
        "schema": value.get("schema"),
        "run": bounded_control_plane_value(value.get("run").unwrap_or(&Value::Null)),
        "mission": bounded_control_plane_value(value.get("mission").unwrap_or(&Value::Null)),
        "state": bounded_control_plane_value(value.get("state").unwrap_or(&Value::Null)),
        "phase": bounded_control_plane_value(value.get("phase").unwrap_or(&Value::Null)),
        "blocker": blocker,
        "owner": bounded_control_plane_value(value.get("owner").unwrap_or(&Value::Null)),
        "placement": crate::commands::run_placement::placement(&value),
        "candidate": bounded_control_plane_value(value.get("candidate").unwrap_or(&Value::Null)),
        "gates": value.get("gates").and_then(Value::as_array)
            .map(|gates| gates.iter().take(COMPACT_REF_LIMIT).map(bounded_control_plane_value).collect::<Vec<_>>())
            .unwrap_or_default(),
        "gates_omitted": value.get("gates").and_then(Value::as_array)
            .map(|gates| gates.len().saturating_sub(COMPACT_REF_LIMIT)).unwrap_or_default(),
        "publication": bounded_control_plane_value(value.get("publication").unwrap_or(&Value::Null)),
        "recovery": compact_recovery(value.get("recovery").unwrap_or(&Value::Null)),
        "waiting_for": value.get("waiting_for"),
        "lab_admission_wait": bounded_control_plane_value(value.get("lab_admission_wait").unwrap_or(&Value::Null)),
        "runner_ownership_diagnostic": bounded_control_plane_value(value.get("runner_ownership_diagnostic").unwrap_or(&Value::Null)),
        "next_action": { "command": next_action },
        "action_eligibility": {
            "actions": selected_action,
            "available_action_count": available
        },
        "presentation": "compact",
        "terminal_outcome": bounded_control_plane_value(value.get("terminal_outcome").unwrap_or(&Value::Null)),
        "recoverable": value.get("recoverable"),
    });
    // The bound model the run executes with (or its latest executor recorded)
    // belongs next to the state, so operators plan rotations on facts (#15854).
    if let Some(provider) = value.get("provider").filter(|provider| !provider.is_null()) {
        compact["provider"] = bounded_control_plane_value(provider);
    }
    if let Some(models) = value
        .get("served_models")
        .filter(|models| !models.is_null())
    {
        compact["served_models"] = bounded_control_plane_value(models);
    }

    for key in ["artifacts", "evidence"] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            let mut seen = HashSet::new();
            let unique = items
                .iter()
                .filter(|item| {
                    let identity = item
                        .get("id")
                        .or_else(|| item.get("uri"))
                        .unwrap_or(*item)
                        .to_string();
                    seen.insert(identity)
                })
                .collect::<Vec<_>>();
            compact[format!("{key}_count")] = json!(unique.len());
            let refs = unique
                .iter()
                .take(3)
                .map(|item| {
                    let mut reference = json!({});
                    for field in ["id", "kind", "uri"] {
                        if let Some(value) = item.get(field) {
                            reference[field] = bounded_value(value);
                        }
                    }
                    reference
                })
                .collect::<Vec<_>>();
            compact[format!("{key}_refs")] = json!(refs);
        }
    }
    for key in [
        "candidate_commit",
        "candidate_ref",
        "candidate_sha",
        "gated_sha",
        "publication_sha",
        "tree",
        "identity_note",
        "lab_snapshot_failure",
        "lab_staging_diagnostic",
    ] {
        if let Some(diagnostic) = value.get(key) {
            compact[key] = bounded_control_plane_value(diagnostic);
        }
    }
    // Retain the prior canonical blocker and safe retry guidance attached by
    // this command, but never duplicate the complete eligibility matrix.
    if let Some(reason) = value.get("reason").and_then(Value::as_str) {
        compact["reason"] = bounded_diagnostic_message(reason);
    }
    if let Some(blocker_summary) = value.get("blocker_summary") {
        compact["blocker_summary"] = blocker_summary
            .as_str()
            .map(bounded_diagnostic_message)
            .unwrap_or_else(|| bounded_control_plane_value(blocker_summary));
    }
    enforce_control_plane_status_budget(compact)
}

pub(super) fn compact_recovery(recovery: &Value) -> Value {
    let mut compact = bounded_control_plane_value(recovery);
    if let Some(command) = recovery
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| command.len() <= COMPACT_COMMAND_LIMIT)
    {
        if compact.is_object() {
            compact["command"] = json!(command);
        }
    }
    compact
}

pub(super) fn bounded_control_plane_value(value: &Value) -> Value {
    if let Some(value) = bounded_diagnostic_value(value) {
        return value;
    }
    value
        .as_str()
        .map(bounded_diagnostic_message)
        .unwrap_or(Value::Null)
}

pub(super) fn enforce_control_plane_status_budget(mut value: Value) -> Value {
    if serialized_len(&value) <= COMPACT_STATUS_BYTE_LIMIT {
        return value;
    }
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    // Keep the causal core and legal next command. Supplementary payloads are
    // the first to go when the serialized UTF-8 budget is exhausted.
    for field in [
        "lab_snapshot_failure",
        "lab_staging_diagnostic",
        "publication",
        "candidate",
        "gates",
        "mission",
        "artifacts_refs",
        "evidence_refs",
    ] {
        object.remove(field);
        if serialized_len(&Value::Object(object.clone())) <= COMPACT_STATUS_BYTE_LIMIT {
            return value;
        }
    }
    for field in ["blocker_summary", "blocker", "owner", "phase", "run"] {
        if let Some(current) = object.get(field).cloned() {
            let serialized = current.to_string();
            object.insert(field.to_string(), bounded_diagnostic_message(&serialized));
        }
        if serialized_len(&Value::Object(object.clone())) <= COMPACT_STATUS_BYTE_LIMIT {
            return value;
        }
    }
    let mut supplementary = object
        .keys()
        .filter(|field| {
            !matches!(
                field.as_str(),
                "schema"
                    | "run"
                    | "state"
                    | "phase"
                    | "blocker"
                    | "owner"
                    | "blocker_summary"
                    | "next_action"
                    | "action_eligibility"
                    | "presentation"
                    | "terminal_outcome"
                    | "recoverable"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    supplementary.sort();
    for field in supplementary {
        object.remove(&field);
        if serialized_len(&Value::Object(object.clone())) <= COMPACT_STATUS_BYTE_LIMIT {
            return value;
        }
    }
    // The selected legal command is essential, but all other non-core facts
    // have already been bounded or discarded above.
    value
}

pub(super) fn deduplicate_control_plane_evidence(value: &mut Value) {
    for key in ["artifacts", "evidence"] {
        let Some(items) = value.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut seen = HashSet::new();
        items.retain(|item| {
            // Only collapse byte-identical records. Repeated IDs can carry
            // distinct facts in the full evidence view and must survive.
            seen.insert(item.to_string())
        });
    }
}

pub(crate) fn project_operator_output(value: &mut Value) {
    project_operator_value(value, false, &mut RawFailureBudget::default());
}

pub(super) const RAW_FAILURE_COUNT_LIMIT: usize = 8;
pub(super) const RAW_FAILURE_BYTE_LIMIT: usize = 8 * 1024;
pub(super) const RAW_FAILURE_PARSE_BYTE_LIMIT: usize = 32 * 1024;
pub(super) const RAW_FAILURE_JSON_DEPTH_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct RawFailureBudget {
    retained: usize,
    pub(super) bytes: usize,
}

pub(super) fn project_operator_value(
    value: &mut Value,
    evidence_content: bool,
    failure_budget: &mut RawFailureBudget,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                project_operator_value(item, evidence_content, failure_budget);
            }
        }
        Value::Object(fields) => {
            let hydrated_evidence = fields.contains_key("kind")
                && fields.contains_key("uri")
                && fields.contains_key("status")
                && fields.contains_key("content");
            let collection_keys = fields
                .keys()
                .filter(|key| OPERATOR_HEAVY_COLLECTIONS.contains(&key.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            for key in collection_keys {
                project_heavy_collection(fields, &key, failure_budget);
            }
            for (key, item) in fields.iter_mut() {
                if OPERATOR_HEAVY_FIELDS.contains(&key.as_str())
                    || (evidence_content && key == "body")
                {
                    if let Value::String(text) = item {
                        let inline_small_stderr = key == "stderr"
                            && text.len() <= STDERR_INLINE_LIMIT
                            && !is_failure_result(text);
                        if let Some(redacted) = bounded_failure_result(text, failure_budget) {
                            *text = redacted;
                        } else if !inline_small_stderr
                            && (is_failure_result(text) || text.len() > COMPACT_TEXT_LIMIT)
                        {
                            let digest = content_hash::sha256_hex(text.as_bytes());
                            *text = format!("[omitted {} bytes; sha256={digest}]", text.len());
                        }
                    }
                    continue;
                }
                if OPERATOR_HEAVY_COLLECTIONS.contains(&key.as_str()) {
                    continue;
                }
                project_operator_value(item, hydrated_evidence && key == "content", failure_budget);
            }
        }
        _ => {}
    }
}

pub(super) fn bounded_failure_result(text: &str, budget: &mut RawFailureBudget) -> Option<String> {
    if text.len() > RAW_FAILURE_PARSE_BYTE_LIMIT {
        return None;
    }
    let Ok(Value::Object(result)) = serde_json::from_str(text) else {
        return None;
    };
    if !failure_result_object(&result) {
        return None;
    }
    let mut redacted =
        homeboy::core::redaction::RedactionPolicy::default().redact_json(&Value::Object(result));
    redact_json_encoded_strings(&mut redacted, 0);
    let serialized = serde_json::to_string(&redacted).ok()?;
    if budget.retained >= RAW_FAILURE_COUNT_LIMIT
        || budget.bytes.saturating_add(serialized.len()) > RAW_FAILURE_BYTE_LIMIT
    {
        return None;
    }
    budget.retained += 1;
    budget.bytes += serialized.len();
    Some(serialized)
}

pub(super) fn is_failure_result(text: &str) -> bool {
    if text.len() > RAW_FAILURE_PARSE_BYTE_LIMIT {
        return false;
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.as_object().map(failure_result_object))
        .unwrap_or(false)
}

pub(super) fn failure_result_object(result: &serde_json::Map<String, Value>) -> bool {
    result.get("success").and_then(Value::as_bool) == Some(false)
        || result
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "blocked" | "error" | "failed" | "failure" | "timed_out" | "timeout"
                )
            })
}

pub(super) fn redact_json_encoded_strings(value: &mut Value, depth: usize) {
    match value {
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| redact_json_encoded_strings(item, depth + 1)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|item| redact_json_encoded_strings(item, depth + 1)),
        Value::String(text) => {
            if text == "[REDACTED]" {
                return;
            }
            let json_shaped = matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'['));
            if !json_shaped {
                return;
            }
            if depth >= RAW_FAILURE_JSON_DEPTH_LIMIT {
                *text = "[omitted JSON beyond redaction depth]".to_string();
                return;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(text) else {
                *text = "[omitted invalid encoded JSON]".to_string();
                return;
            };
            if !matches!(parsed, Value::Array(_) | Value::Object(_)) {
                return;
            }
            let mut parsed =
                homeboy::core::redaction::RedactionPolicy::default().redact_json(&parsed);
            redact_json_encoded_strings(&mut parsed, depth + 1);
            if let Ok(serialized) = serde_json::to_string(&parsed) {
                *text = serialized;
            }
        }
        _ => {}
    }
}

/// Known event streams retain their array/item schema. The owning evidence
/// object receives additive metadata describing the omitted durable events.
pub(super) fn project_heavy_collection(
    fields: &mut serde_json::Map<String, Value>,
    key: &str,
    failure_budget: &mut RawFailureBudget,
) {
    let projection = {
        let Some(items) = fields.get_mut(key).and_then(Value::as_array_mut) else {
            return;
        };
        let projection = (items.len() > COMPACT_REF_LIMIT).then(|| {
            let total_items = items.len();
            let digest = content_hash::sha256_hex(
                serde_json::to_vec(items.as_slice())
                    .as_deref()
                    .unwrap_or_default(),
            );
            items.truncate(COMPACT_REF_LIMIT);
            json!({
                "total_items": total_items,
                "returned_items": COMPACT_REF_LIMIT,
                "omitted_items": total_items - COMPACT_REF_LIMIT,
                "sha256": digest,
            })
        });
        for item in items {
            project_operator_value(item, true, failure_budget);
        }
        projection
    };
    if let Some(projection) = projection {
        fields.insert(format!("{key}_projection"), projection);
    }
}

/// Build the compact, recovery-first `status` summary. Source data is the full
/// run-record `Value` (already enriched with `diagnostic_summary`); the plan is
/// loaded best-effort to map task ids back to issue URLs and prompt titles.
pub(crate) fn compact_aggregate_summary(
    aggregate: &AgentTaskAggregate,
    run_id: Option<&str>,
) -> Value {
    let full = serde_json::to_value(
        homeboy::agents::agent_task_artifacts::reviewer_facing_aggregate(aggregate),
    )
    .unwrap_or(Value::Null);
    let outcomes = full
        .get("outcomes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let tasks = outcomes.iter().take(COMPACT_TASK_LIMIT).map(|outcome| json!({
        "task_id": outcome.get("task_id"),
        "status": outcome.get("status"),
        "summary": bounded_value(outcome.get("summary").unwrap_or(&Value::Null)),
        "failure_classification": outcome.get("failure_classification"),
        "timestamps": compact_fields(outcome, &["created_at", "updated_at", "timestamp", "finished_at"]),
        "artifacts": compact_items(outcome.get("artifacts"), &["schema", "id", "kind", "name", "path", "url", "sha256", "size_bytes"]),
        "artifacts_omitted": compact_duplicate_count(outcome.get("artifacts")),
        "evidence_refs": compact_items(outcome.get("evidence_refs"), &["schema", "kind", "uri", "created_at", "timestamp"]),
        "evidence_refs_omitted": compact_duplicate_count(outcome.get("evidence_refs")),
    })).collect::<Vec<_>>();
    let mut summary = json!({
        "schema": full.get("schema"),
        "view": "summary",
        "plan_id": full.get("plan_id"),
        "status": full.get("status"),
        "totals": full.get("totals"),
        "tasks": tasks,
        "tasks_omitted": outcomes.len().saturating_sub(COMPACT_TASK_LIMIT),
        "failure_reasons": bounded_failure_reasons(&Value::Array(failure_reasons_from_aggregate(aggregate))),
    });
    if let Some(run_id) = run_id {
        summary["run_id"] = json!(run_id);
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id}"));
        summary["evidence_command"] = json!(format!("homeboy runs evidence {run_id}"));
    }
    enforce_compact_status_budget(summary)
}

pub(super) fn enforce_compact_status_budget(mut value: Value) -> Value {
    if serialized_len(&value) <= COMPACT_STATUS_BYTE_LIMIT {
        return value;
    }
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    let mut mandatory_omissions = compact_mandatory_scalars(object);
    let full_command = object
        .get("full_command")
        .and_then(Value::as_str)
        .unwrap_or("homeboy agent-task status <run-id>")
        .to_string();
    let mut omitted = Vec::new();
    // Ordered from supplementary evidence to the broad status tables, then the
    // failure cause: a failed Cook's `failure_context` is what explains it, so
    // it is the last section given up. Core IDs, state, and the full-output
    // command always survive this final pass.
    for section in [
        "durable_candidate",
        "notification_delivery",
        "notification_resolution",
        "transport_recovery",
        "waiting_for",
        "lab_admission_wait",
        "diagnostic_summary",
        "failure_reasons",
        "execution_states",
        "candidate_adoption",
        "finalization",
        "selected_candidate",
        "attempts",
        "provider",
        "remaining_phases",
        "continuation_command",
        "evidence_command",
        "risk_flags",
        "refs",
        "artifact_refs",
        "tasks",
        "queue_visibility",
        "liveness",
        "canonical_candidate",
        "work_summary",
        "execution_budget",
        "cook",
        "timestamps",
        "candidate_selection",
        "identity",
        "runner_probe",
        ACTIONABLE_METADATA_KEY,
        "failure_context",
    ] {
        if let Some(removed) = object.remove(section) {
            omitted.push(json!({ "section": section, "count": compact_section_count(&removed) }));
        }
        if compact_budget_value(object, &full_command, &omitted, &mandatory_omissions)
            <= COMPACT_STATUS_BYTE_LIMIT
        {
            break;
        }
    }
    // Future enrichments must not bypass the ceiling: remove any remaining
    // non-core section in lexical order after the named projection sections.
    let mut remaining_sections = object
        .keys()
        .filter(|key| !compact_mandatory_field(key))
        .cloned()
        .collect::<Vec<_>>();
    remaining_sections.sort();
    for section in remaining_sections {
        if compact_budget_value(object, &full_command, &omitted, &mandatory_omissions)
            <= COMPACT_STATUS_BYTE_LIMIT
        {
            break;
        }
        if let Some(removed) = object.remove(&section) {
            omitted.push(json!({ "section": section, "count": compact_section_count(&removed) }));
        }
    }
    object.insert("full_command".to_string(), Value::String(full_command));
    if !omitted.is_empty() {
        object.insert("omitted_sections".to_string(), Value::Array(omitted));
    }
    if !mandatory_omissions.is_empty() {
        object.insert(
            "mandatory_scalar_omissions".to_string(),
            Value::Array(std::mem::take(&mut mandatory_omissions)),
        );
    }
    value
}

pub(super) fn compact_mandatory_scalars(object: &mut serde_json::Map<String, Value>) -> Vec<Value> {
    let mut omissions = Vec::new();
    for field in [
        "schema",
        "view",
        "run_id",
        "cook_id",
        "latest_run_id",
        "status",
        "state",
    ] {
        let Some(text) = object.get(field).and_then(Value::as_str) else {
            continue;
        };
        if text.len() <= COMPACT_MANDATORY_SCALAR_BYTE_LIMIT {
            continue;
        }
        let bytes = text.len();
        let sha256 = content_hash::sha256_hex(text.as_bytes());
        object.insert(field.to_string(), Value::String(format!("sha256:{sha256}")));
        omissions.push(json!({ "field": field, "bytes": bytes, "sha256": sha256 }));
    }
    if object
        .get("full_command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.len() > COMPACT_MANDATORY_SCALAR_BYTE_LIMIT)
    {
        let command = object["full_command"].as_str().unwrap_or_default();
        omissions.push(json!({
            "field": "full_command",
            "bytes": command.len(),
            "sha256": content_hash::sha256_hex(command.as_bytes()),
        }));
        object.insert(
            "full_command".to_string(),
            json!("homeboy agent-task status <run-id>"),
        );
    }
    omissions
}

pub(super) fn compact_mandatory_field(field: &str) -> bool {
    matches!(
        field,
        "schema"
            | "view"
            | "run_id"
            | "cook_id"
            | "latest_run_id"
            | "status"
            | "state"
            | "lab_transport_failure"
            | "blocker_summary"
            | "next_action"
            | "lab_staging_diagnostic"
            | "full_command"
    )
}

pub(super) fn compact_budget_value(
    object: &serde_json::Map<String, Value>,
    full_command: &str,
    omitted: &[Value],
    mandatory_omissions: &[Value],
) -> usize {
    let mut projected = object.clone();
    projected.insert("full_command".to_string(), json!(full_command));
    if !omitted.is_empty() {
        projected.insert(
            "omitted_sections".to_string(),
            Value::Array(omitted.to_vec()),
        );
    }
    if !mandatory_omissions.is_empty() {
        projected.insert(
            "mandatory_scalar_omissions".to_string(),
            Value::Array(mandatory_omissions.to_vec()),
        );
    }
    serialized_len(&Value::Object(projected))
}

pub(super) fn compact_section_count(value: &Value) -> usize {
    value.as_array().map_or(1, Vec::len)
}

/// The default view for `homeboy agent-task cook`. `--full` is opt-in, so this
/// is what an external orchestrator actually receives back from a cook.
///
/// "Compact" must still mean "actionable". `cook_failure_context` already
/// computes the exact runnable recovery commands for the failed Cook state, and
/// the notification path forwards them verbatim — the pull channel has no reason
/// to be poorer than the push channel from the same computation. Only the
/// runnable/classification fields are surfaced here; full diagnostics and
/// `blocking_claim` stay behind `--full`. Compact output retains only a typed
/// diagnostic cause. Output only grows on the failure path: `failure_context` is `None`
/// whenever `exit_code == 0`.
pub(crate) fn compact_cook_report(value: Value, full: bool) -> Value {
    let mut value = value;
    let candidate_run_id = value
        .pointer("/selected_candidate/run_id")
        .and_then(Value::as_str)
        .or_else(|| value.get("latest_run_id").and_then(Value::as_str))
        .map(str::to_string);
    let candidate_projection = candidate_run_id
        .as_deref()
        .map(compact_cook_candidate_projection)
        .filter(|projection| projection["status"] == "available");
    if full {
        if let Some(candidate) = candidate_projection.as_ref() {
            value["durable_candidate"] = candidate.clone();
        }
        return value;
    }
    let attempts = value
        .get("attempts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let latest_run_id = value.get("latest_run_id").and_then(Value::as_str);
    let mut summary = json!({
        "schema": value.get("schema"),
        "view": "summary",
        "cook_id": value.get("cook_id"),
        "latest_run_id": value.get("latest_run_id"),
        "status": value.get("status"),
        "stop_reason": bounded_value(value.get("stop_reason").unwrap_or(&Value::Null)),
        "terminal_phase": value.get("terminal_phase"),
        "terminal_failure_classification": value.get("terminal_failure_classification"),
        "primary_failure": value.get("primary_failure"),
        "attempts": attempts.iter().take(COMPACT_TASK_LIMIT).map(|attempt| compact_fields(attempt, &["attempt", "run_id", "run_state", "aggregate_path"])).collect::<Vec<_>>(),
        "attempts_omitted": attempts.len().saturating_sub(COMPACT_TASK_LIMIT),
        "finalization": value.get("finalization").map(|finalization| compact_fields(finalization, &["schema", "status", "pr_number", "pr_url", "updated_at", "created_at"])),
        "selected_candidate": value.get("selected_candidate").map(|candidate| compact_fields(candidate, &["latest_attempt_run_id", "run_id", "attempt", "invocation_scoped", "selected_task_id", "selected_candidate_sha", "reason", "incomplete", "skipped_newer_attempts", "applied_promotion"])),
    });
    if let Some(candidate) = candidate_projection {
        summary["durable_candidate"] = candidate.clone();
    }
    // The run ids this invocation actually dispatched, so a caller can tell them
    // apart from the cross-invocation history `latest_run_id` may be drawn from.
    if let Some(invocation_run_ids) = value.get("invocation_run_ids") {
        summary["invocation_run_ids"] = invocation_run_ids.clone();
    }
    // The recovery commands the Cook already computed for its own failed state.
    // Full diagnostics and `blocking_claim` stay behind `--full` and `diagnose`.
    // Compact output retains only the typed diagnostic cause.
    if let Some(failure_context) = value.get("failure_context") {
        summary["failure_context"] = compact_fields(
            failure_context,
            &[
                "latest_run_id",
                "selected_run_id",
                "durable_recipe_ref",
                "lifecycle_state",
                "phase",
                "reason_code",
                "provider_budget_consumed",
                "provider_executions_consumed",
                "recovery_legal",
                "recovery_reason",
            ],
        );
        for field in ["legal_actions", "next_actions"] {
            if let Some(actions) = failure_context.get(field).and_then(Value::as_array) {
                let (samples, omitted) = compact_action_samples(actions);
                if field == "next_actions" {
                    if let Some(next_action) =
                        samples.as_array().and_then(|samples| samples.first())
                    {
                        summary["failure_context"]["next_action"] = next_action.clone();
                    }
                }
                summary["failure_context"][field] = samples;
                summary["failure_context"][format!("{field}_omitted")] = json!(omitted);
            }
        }
        if let Some(diagnostic) = failure_context.get("diagnostic") {
            summary["failure_context"]["diagnostic"] = compact_cook_diagnostic(diagnostic);
        }
    }
    if let Some(run_id) = latest_run_id {
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id}"));
        summary["evidence_command"] = json!(format!("homeboy runs evidence {run_id}"));
    }
    for field in ["provider", "remaining_phases", "continuation_command"] {
        if let Some(value) = value.get(field) {
            summary[field] = bounded_value(value);
        }
    }
    enforce_compact_status_budget(summary)
}

pub(super) fn compact_cook_diagnostic(diagnostic: &Value) -> Value {
    let cause = diagnostic
        .get("deepest_cause")
        .filter(|value| value.is_object())
        .unwrap_or(diagnostic);
    json!({
        "code": cause.get("code"),
        "field": cause.get("field").or_else(|| diagnostic.pointer("/details/field")),
        // Truncate rather than drop: a long cause (e.g. git apply stderr) is
        // exactly the explanation an operator needs.
        "message": cause
            .get("message")
            .and_then(Value::as_str)
            .map(bounded_diagnostic_message)
            .unwrap_or(Value::Null),
    })
}

pub(super) fn compact_items(value: Option<&Value>, fields: &[&str]) -> Value {
    let mut identities = std::collections::HashSet::new();
    let items = value.and_then(Value::as_array).into_iter().flatten();
    Value::Array(
        items
            .filter(|item| {
                compact_identity(item).is_none_or(|identity| identities.insert(identity))
            })
            .take(COMPACT_REF_LIMIT)
            .map(|item| compact_fields(item, fields))
            .collect(),
    )
}

pub(super) fn compact_duplicate_count(value: Option<&Value>) -> usize {
    let mut identities = std::collections::HashSet::new();
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(compact_identity)
        .filter(|identity| !identities.insert(identity.clone()))
        .count()
}

pub(super) fn compact_identity(item: &Value) -> Option<String> {
    item.get("id")
        .and_then(Value::as_str)
        .map(|id| format!("id:{id}"))
        .or_else(|| {
            item.get("uri")
                .and_then(Value::as_str)
                .map(|uri| format!("uri:{uri}"))
        })
}

pub(super) fn compact_fields(value: &Value, fields: &[&str]) -> Value {
    let mut object = serde_json::Map::new();
    let mut omitted_scalars = Vec::new();
    for field in fields {
        if let Some(value) = value.get(*field) {
            if value
                .as_str()
                .is_some_and(|text| text.len() > COMPACT_TEXT_LIMIT)
            {
                omitted_scalars
                    .push(json!({ "field": field, "bytes": value.as_str().map_or(0, str::len) }));
                continue;
            }
            object.insert((*field).to_string(), bounded_value(value));
        }
    }
    if !omitted_scalars.is_empty() {
        object.insert("omitted_scalars".to_string(), Value::Array(omitted_scalars));
    }
    Value::Object(object)
}

/// Recovery actions are executable operator guidance, not evidence. Keep a
/// small copyable sample in the default view and leave the complete list in the
/// explicit `--full` report.
pub(super) fn compact_action_samples(actions: &[Value]) -> (Value, usize) {
    let mut samples = Vec::new();
    let mut omitted = 0;
    for action in actions {
        let Some(action_name) = action.get("action").and_then(Value::as_str) else {
            omitted += 1;
            continue;
        };
        let Some(command) = action.get("command").and_then(Value::as_str) else {
            omitted += 1;
            continue;
        };
        let sample = json!({ "action": action_name, "command": command });
        let sample_bytes = serde_json::to_vec(&sample).map_or(usize::MAX, |bytes| bytes.len());
        if samples.len() >= COMPACT_ACTION_LIMIT || sample_bytes > COMPACT_ACTION_BYTE_LIMIT {
            omitted += 1;
            continue;
        }
        samples.push(sample);
    }
    (Value::Array(samples), omitted)
}

pub(super) fn bounded_failure_reasons(value: &Value) -> Value {
    Value::Array(
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .take(FAILURE_REASON_LIMIT)
                    .map(|item| compact_fields(item, &["task_id", "class", "message", "source"]))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

pub(super) fn bounded_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.len() > COMPACT_TEXT_LIMIT => Value::Null,
        _ => value.clone(),
    }
}

pub(super) fn collected_diagnostic_value(item: CollectedDiagnostic) -> Value {
    collected_diagnostic_value_with_details(item, false)
}

pub(super) fn collected_diagnostic_value_with_details(
    item: CollectedDiagnostic,
    include_details: bool,
) -> Value {
    let owner = diagnostic_owner(&item.class, &item.source, &item.data);
    let mut value = json!({
        "task_id": item.task_id,
        "class": item.class,
        "message": bounded_diagnostic_message(&item.message),
        "source": item.source,
        "owner": owner,
    });
    if include_details && !item.data.is_null() {
        // `--full` is the explicit evidence surface: preserve the complete
        // redacted source facts already retained by the durable record.
        value["details"] = item.data.clone();
    } else if let Some(details) = policy_denial_details(&item.data) {
        value["details"] = details;
    } else if let Some(details) = structured_error_details(&item.data) {
        value["details"] = details;
    } else if item.source == "pre_execution_failure" {
        value["details"] = bounded_diagnostic_value(&item.data).unwrap_or(Value::Null);
    } else if item.source == "lab_preacceptance_transport" {
        value["details"] = bounded_diagnostic_value(&item.data).unwrap_or(Value::Null);
    } else if item.source == "runner_ownership" {
        value["details"] = bounded_diagnostic_value(&item.data).unwrap_or(Value::Null);
    } else if (item.source == "current_lifecycle" && item.class == "lab_staging.json_parse")
        || item.source == "runner_build_skew"
    {
        value["details"] = bounded_diagnostic_value(&item.data).unwrap_or(Value::Null);
    } else if let Some(details) = item.data.get("worktree_provider_failure") {
        value["details"] = details.clone();
    }
    if let Some(field) = item.data.get("field").filter(|field| !field.is_null()) {
        if let Some(field) = bounded_diagnostic_value(field) {
            value["field"] = field;
        }
    }
    if let Some(phase) = item
        .data
        .get("cook_phase")
        .filter(|phase| phase.is_string())
    {
        if let Some(phase) = bounded_diagnostic_value(phase) {
            value["cook_phase"] = phase;
        }
    }
    value
}
