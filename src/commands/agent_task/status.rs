//! Read-side handlers: status, logs, artifacts, list/active/latest, and cancel.
//!
//! `status` returns a compact, recovery-first summary by default (#4396):
//! run id, state, totals, a per-task source table (#4392), deduped patch/changed
//! references, and a prominent risk-flag section (#4398). The full verbose
//! payload is available behind `--full`.

use serde_json::{json, Value};

use homeboy::core::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::core::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy::core::agent_tasks::service as agent_task_service;

use super::super::CmdResult;
use super::args::{CancelArgs, StatusArgs};

/// Cap the number of detail refs rendered in the compact summary so a noisy
/// aggregate cannot flood recovery output. Overflow is reported as an
/// `omitted` count rather than dropped silently.
const COMPACT_REF_LIMIT: usize = 12;

pub(super) fn status(args: StatusArgs) -> CmdResult<Value> {
    let record = agent_task_service::status(&args.run_id)?;
    let mut value = serde_json::to_value(&record).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    if args.full {
        return Ok((value, 0));
    }
    let summary = compact_status_summary(&value, &args.run_id);
    Ok((summary, 0))
}

pub(super) fn list_runs(filter: agent_task_service::AgentTaskDiscoveryFilter) -> CmdResult<Value> {
    let report = agent_task_service::discover_runs(filter)?;
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

pub(super) fn logs(args: StatusArgs) -> CmdResult<Value> {
    let log = agent_task_service::logs(&args.run_id)?;
    let mut value = serde_json::to_value(log).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    Ok((value, 0))
}

pub(super) fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    Ok((serde_json::to_value(artifacts).unwrap_or(Value::Null), 0))
}

pub(super) fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let record = agent_task_service::cancel(&args.run_id, args.reason.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn enrich_with_diagnostic_summary(value: &mut Value, run_id: &str) -> homeboy::core::Result<()> {
    let Some(aggregate) = completed_run_aggregate(run_id).transpose()? else {
        return Ok(());
    };
    if let Some(summary) = diagnostic_summary_from_aggregate(&aggregate) {
        value["diagnostic_summary"] = summary;
    }
    Ok(())
}

pub(crate) fn completed_run_aggregate(
    run_id: &str,
) -> Option<homeboy::core::Result<AgentTaskAggregate>> {
    match agent_task_lifecycle::aggregate_source(run_id) {
        Ok((raw, _path)) => Some(serde_json::from_str(&raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task aggregate".to_string()),
                Some(raw),
            )
        })),
        Err(error) if error.code == homeboy::core::ErrorCode::ValidationInvalidArgument => None,
        Err(error) => Some(Err(error)),
    }
}

pub(crate) fn diagnostic_summary_from_aggregate(aggregate: &AgentTaskAggregate) -> Option<Value> {
    aggregate.outcomes.iter().find_map(|outcome| {
        let diagnostic = outcome.diagnostics.first()?;
        Some(json!({
            "task_id": outcome.task_id,
            "class": diagnostic.class,
            "message": diagnostic.message,
        }))
    })
}

/// Build the compact, recovery-first `status` summary. Source data is the full
/// run-record `Value` (already enriched with `diagnostic_summary`); the plan is
/// loaded best-effort to map task ids back to issue URLs and prompt titles.
fn compact_status_summary(record: &Value, run_id: &str) -> Value {
    let plan = agent_task_lifecycle::load_plan(run_id).ok();
    let task_table = task_source_table(record, plan.as_ref());
    let (refs, refs_omitted) = compact_refs(record);
    let risk_flags = risk_flags(record);

    let mut summary = json!({
        "schema": "homeboy/agent-task-status-summary/v1",
        "run_id": record.get("run_id").cloned().unwrap_or_else(|| json!(run_id)),
        "state": record.get("state").cloned().unwrap_or(Value::Null),
        "totals": record.get("totals").cloned().unwrap_or(Value::Null),
        "tasks": task_table,
        "refs": refs,
        "refs_omitted": refs_omitted,
        "risk_flags": risk_flags,
        "full_command": format!("homeboy agent-task status {run_id} --full"),
    });

    if let Some(diagnostic) = record.get("diagnostic_summary") {
        if !diagnostic.is_null() {
            summary["diagnostic_summary"] = diagnostic.clone();
        }
    }
    if let Some(aggregate_path) = record.get("aggregate_path") {
        if !aggregate_path.is_null() {
            summary["aggregate_path"] = aggregate_path.clone();
        }
    }
    summary
}

/// Map each run-record task to a source label: task id + issue URL (from the
/// plan source refs) + the first sentence/title of the prompt + a brief
/// artifact summary (#4392).
fn task_source_table(record: &Value, plan: Option<&AgentTaskPlan>) -> Value {
    let Some(tasks) = record.get("tasks").and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };

    let rows: Vec<Value> = tasks
        .iter()
        .map(|task| {
            let task_id = task.get("task_id").and_then(Value::as_str).unwrap_or("");
            let state = task.get("state").cloned().unwrap_or(Value::Null);
            let (issue_url, prompt_title) = plan
                .and_then(|plan| plan_task_source(plan, task_id))
                .unwrap_or((None, None));
            let artifact_summary = task_artifact_summary(record, task_id);

            json!({
                "task_id": task_id,
                "state": state,
                "issue_url": issue_url,
                "prompt": prompt_title,
                "artifacts": artifact_summary,
            })
        })
        .collect();

    Value::Array(rows)
}

/// Resolve a task's issue URL and prompt title from the loaded plan.
fn plan_task_source(
    plan: &AgentTaskPlan,
    task_id: &str,
) -> Option<(Option<String>, Option<String>)> {
    let request = plan.tasks.iter().find(|task| task.task_id == task_id)?;
    let issue_url = request
        .source_refs
        .iter()
        .find(|source| is_issue_uri(&source.uri))
        .or_else(|| request.source_refs.first())
        .map(|source| source.uri.clone());
    let prompt_title = first_sentence(&request.instructions);
    Some((issue_url, prompt_title))
}

fn is_issue_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    lower.contains("/issues/") || lower.contains("/pull/") || lower.contains("github.com")
}

/// First sentence (or first line) of a prompt, trimmed to a recovery-friendly
/// length so the summary stays scannable.
fn first_sentence(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed
        .find(['.', '\n'])
        .map(|index| index + 1)
        .unwrap_or(trimmed.len());
    let sentence = trimmed[..end].trim().trim_end_matches('.').trim();
    const MAX_CHARS: usize = 140;
    let title = if sentence.chars().count() > MAX_CHARS {
        let truncated: String = sentence.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        sentence.to_string()
    };
    (!title.is_empty()).then_some(title)
}

/// Brief per-task artifact summary derived from the run record's deduped
/// `artifact_refs`.
fn task_artifact_summary(record: &Value, task_id: &str) -> Value {
    let refs = record
        .get("artifact_refs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let task_refs: Vec<&Value> = refs
        .iter()
        .filter(|item| item.get("task_id").and_then(Value::as_str) == Some(task_id))
        .collect();
    let mut kinds: Vec<String> = task_refs
        .iter()
        .filter_map(|item| item.get("kind").and_then(Value::as_str).map(str::to_string))
        .collect();
    kinds.sort();
    kinds.dedup();
    json!({
        "count": task_refs.len(),
        "kinds": kinds,
    })
}

/// Deduped, empty-uri-filtered artifact/evidence refs, capped to keep the
/// recovery summary scannable. The full list remains available via `--full`.
fn compact_refs(record: &Value) -> (Value, usize) {
    let refs = record
        .get("artifact_refs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut seen = std::collections::HashSet::new();
    let mut rendered: Vec<Value> = Vec::new();
    let mut total_valid = 0usize;

    for item in refs {
        let uri = item.get("uri").and_then(Value::as_str).unwrap_or("").trim();
        if uri.is_empty() {
            continue;
        }
        if !seen.insert(uri.to_string()) {
            continue;
        }
        total_valid += 1;
        if rendered.len() < COMPACT_REF_LIMIT {
            rendered.push(json!({
                "task_id": item.get("task_id").cloned().unwrap_or(Value::Null),
                "kind": item.get("kind").cloned().unwrap_or(Value::Null),
                "uri": uri,
            }));
        }
    }

    let omitted = total_valid.saturating_sub(rendered.len());
    (Value::Array(rendered), omitted)
}

/// Surface artifact RISK FLAGS prominently (#4398). Flags are derived from the
/// run record's artifact refs and the completed aggregate's artifact metadata,
/// so reviewers see them before promotion/apply instead of digging through
/// buried payloads.
fn risk_flags(record: &Value) -> Value {
    let mut flags: Vec<Value> = Vec::new();

    let run_id = record.get("run_id").and_then(Value::as_str);
    let aggregate = run_id
        .and_then(|run_id| completed_run_aggregate(run_id))
        .and_then(Result::ok);

    let mut has_patch = false;
    let mut has_test_evidence = false;

    if let Some(aggregate) = aggregate.as_ref() {
        for outcome in &aggregate.outcomes {
            for artifact in &outcome.artifacts {
                if artifact.kind == "patch" {
                    has_patch = true;
                    if artifact_is_full_file_rewrite(&artifact.metadata) {
                        flags.push(json!({
                            "flag": "suspicious-full-file-rewrite",
                            "task_id": outcome.task_id,
                            "artifact_id": artifact.id,
                            "detail": "patch artifact metadata marks a full-file rewrite; review the diff scope before applying",
                        }));
                    }
                }
                if value_mentions_redaction(&artifact.metadata) {
                    flags.push(json!({
                        "flag": "secrets-redacted",
                        "task_id": outcome.task_id,
                        "artifact_id": artifact.id,
                        "detail": "artifact metadata contains redacted values; verify no secret leaked into the patch/output",
                    }));
                }
            }
            for evidence in &outcome.evidence_refs {
                if evidence_is_test(&evidence.kind, &evidence.uri) {
                    has_test_evidence = true;
                }
            }
        }
    }

    if has_patch && !has_test_evidence {
        flags.push(json!({
            "flag": "missing-test-evidence",
            "detail": "a patch was produced but no test/transcript evidence ref was recorded; confirm verification before promotion",
        }));
    }

    Value::Array(flags)
}

fn artifact_is_full_file_rewrite(metadata: &Value) -> bool {
    metadata
        .get("full_file_rewrite")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || metadata
            .get("suspicious_full_file_rewrite")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn value_mentions_redaction(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("[redacted]") || lower.contains("redacted")
        }
        Value::Array(items) => items.iter().any(value_mentions_redaction),
        Value::Object(map) => map.values().any(value_mentions_redaction),
        _ => false,
    }
}

fn evidence_is_test(kind: &str, uri: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    let uri = uri.to_ascii_lowercase();
    kind.contains("test")
        || kind.contains("transcript")
        || kind.contains("gate")
        || uri.contains("test")
        || uri.contains("transcript")
}
