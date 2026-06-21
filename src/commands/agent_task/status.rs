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
use homeboy::core::agent_tasks::AgentTaskOutcomeStatus;

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
    let failure_reasons = failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        value["failure_reasons"] = Value::Array(failure_reasons);
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

/// Cap the number of surfaced failure reasons so a pathological run with
/// hundreds of nested diagnostics cannot flood the failure summary. Overflow is
/// still available in the full nested payload (`--full` / aggregate file).
const FAILURE_REASON_LIMIT: usize = 8;

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
    let mut collected: Vec<CollectedDiagnostic> = Vec::new();

    // Prefer failed/errored outcomes, but fall back to scanning every outcome so
    // a root cause attached to a non-failed cell is still surfaced.
    let failed_first = aggregate.outcomes.iter().filter(|outcome| {
        matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Failed
                | AgentTaskOutcomeStatus::ProviderError
                | AgentTaskOutcomeStatus::Timeout
                | AgentTaskOutcomeStatus::UnableToRemediate
        )
    });
    let any_failed = failed_first.clone().next().is_some();

    let scan: Vec<&homeboy::core::agent_tasks::AgentTaskOutcome> = if any_failed {
        failed_first.collect()
    } else {
        aggregate.outcomes.iter().collect()
    };

    for outcome in scan {
        for diagnostic in &outcome.diagnostics {
            collected.push(CollectedDiagnostic {
                task_id: outcome.task_id.clone(),
                class: diagnostic.class.clone(),
                message: diagnostic.message.clone(),
                source: "diagnostics".to_string(),
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

    // Dedupe by (class, message) keeping the first occurrence, then order the
    // most actionable root-cause classes first.
    let mut seen = std::collections::HashSet::new();
    let mut deduped: Vec<CollectedDiagnostic> = Vec::new();
    for item in collected {
        let trimmed = item.message.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = (item.class.to_ascii_lowercase(), trimmed.to_string());
        if !seen.insert(key) {
            continue;
        }
        deduped.push(item);
    }

    deduped.sort_by_key(|item| class_priority(&item.class));

    deduped
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

struct CollectedDiagnostic {
    task_id: String,
    class: String,
    message: String,
    source: String,
}

/// Lower number = higher priority. Actionable root-cause classes
/// (validation/fatal/registration/missing-path) are surfaced before generic or
/// transient noise so the first reason an operator sees is the one worth acting
/// on.
fn class_priority(class: &str) -> u8 {
    let class = class.to_ascii_lowercase();
    if class.contains("valid") || class.contains("recipe") || class.contains("schema") {
        0
    } else if class.contains("fatal") || class.contains("error") || class.contains("exception") {
        1
    } else if class.contains("registr")
        || class.contains("provider")
        || class.contains("discovery")
        || class.contains("capability")
    {
        2
    } else if class.contains("missing")
        || class.contains("not_found")
        || class.contains("path")
        || class.contains("io")
    {
        3
    } else {
        9
    }
}

/// Recursively walk a provider-specific JSON value looking for `diagnostics`
/// arrays of objects carrying a `message` (and optional `class`). This is how
/// provider-owned runtime diagnostics get surfaced without the renderer needing
/// to know the exact provider path.
fn collect_nested_diagnostics(
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
        "execution_location": execution_location(record),
        "queue_visibility": queue_visibility(record),
        "full_command": format!("homeboy agent-task status {run_id} --full"),
    });

    if let Some(diagnostic) = record.get("diagnostic_summary") {
        if !diagnostic.is_null() {
            summary["diagnostic_summary"] = diagnostic.clone();
        }
    }
    if let Some(failure_reasons) = record.get("failure_reasons") {
        if failure_reasons
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
        {
            summary["failure_reasons"] = failure_reasons.clone();
        }
    }
    if let Some(aggregate_path) = record.get("aggregate_path") {
        if !aggregate_path.is_null() {
            summary["aggregate_path"] = aggregate_path.clone();
        }
    }
    if let Some(latest_promotion) = record
        .get("metadata")
        .and_then(|metadata| metadata.get("latest_promotion"))
    {
        if !latest_promotion.is_null() {
            summary["latest_promotion"] = latest_promotion.clone();
        }
    }
    summary
}

fn execution_location(record: &Value) -> Value {
    let runner_id = record
        .get("metadata")
        .and_then(|metadata| metadata.get("runner_id"))
        .and_then(Value::as_str)
        .filter(|runner_id| !runner_id.trim().is_empty());
    match runner_id {
        Some(runner_id) => json!(format!("runner:{runner_id}")),
        None => json!("local"),
    }
}

fn queue_visibility(record: &Value) -> Value {
    json!({
        "state": record.get("state").cloned().unwrap_or(Value::Null),
        "totals": record.get("totals").cloned().unwrap_or(Value::Null),
        "commands": [
            "homeboy agent-task list",
            "homeboy agent-task active",
            "homeboy agent-task run-next",
        ],
        "concurrency_note": "Cook/controller concurrency is declared by the queued plan; use `homeboy agent-task status <run-id> --full` to inspect the materialized dispatch settings.",
    })
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
        .and_then(completed_run_aggregate)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_status_surfaces_latest_promotion() {
        let record = json!({
            "run_id": "agent-task-run-1",
            "state": "succeeded",
            "tasks": [],
            "metadata": {
                "latest_promotion": {
                    "schema": "homeboy/agent-task-promotion-status/v1",
                    "status": "applied",
                    "source_run_id": "agent-task-run-1",
                    "patch_artifact_id": "patch.diff",
                    "to_worktree": "homeboy@fix-5055",
                    "operator_notification": {
                        "status": "completed",
                        "message": "patch promoted"
                    }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-1");

        assert_eq!(
            summary["latest_promotion"]["patch_artifact_id"],
            "patch.diff"
        );
        assert_eq!(
            summary["latest_promotion"]["operator_notification"]["status"],
            "completed"
        );
        assert_eq!(
            summary["queue_visibility"]["commands"][0],
            "homeboy agent-task list"
        );
        assert!(summary["queue_visibility"]["concurrency_note"]
            .as_str()
            .unwrap()
            .contains("concurrency"));
        assert_eq!(summary["execution_location"], "local");

        let remote = compact_status_summary(
            &json!({
                "run_id": "agent-task-run-2",
                "state": "running",
                "tasks": [],
                "metadata": { "runner_id": "homeboy-lab" }
            }),
            "agent-task-run-2",
        );
        assert_eq!(remote["execution_location"], "runner:homeboy-lab");
    }
}
