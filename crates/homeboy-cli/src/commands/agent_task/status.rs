//! Read-side handlers: status, logs, artifacts, list/active/latest, and cancel.
//!
//! `status` returns a compact, recovery-first summary by default (#4396):
//! run id, state, totals, a per-task source table (#4392), deduped patch/changed
//! references, and a prominent risk-flag section (#4398). The full verbose
//! payload is available behind `--full`.

use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_provider::structured_error::normalized_structured_error;
use homeboy::agents::agent_task_service as agent_task_service_direct;
use homeboy::agents::agent_tasks::lifecycle::{self as agent_task_lifecycle, AgentTaskRunRecord};
use homeboy::agents::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::agents::agent_tasks::{
    AgentTaskEvidenceRef, AgentTaskFailureClassification, AgentTaskOutcomeStatus,
};
use homeboy::core::engine::shell::quote_arg;
use homeboy::core::error::{ActionSafety, ExecutableAction};
use homeboy::core::output::{budget_json_values, OutputBudget};
use homeboy_lab_contract::lab::transport_failure::LabTransportAttemptReceipt;

use super::super::CmdResult;
use super::args::{
    CancelArgs, DiagnoseArgs, EvidenceArgs, LifecycleReadArgs, LogsArgs, QuarantineArgs, RearmArgs,
    ReconcileArgs, ReplayProviderBoundaryArgs, RuntimeRecoverArgs, RuntimeValidateArgs, StatusArgs,
};
use super::candidate::{classify_candidates, CandidateState};
use crate::commands::utils::response::{
    CommandActionableMetadata, CommandAgentTaskRef, CommandArtifactRef, CommandNextAction,
    CommandNextActionKind, CommandResultRefs, CommandRunRef, ACTIONABLE_METADATA_KEY,
};
use crate::commands::utils::watch::{
    parse_duration, watch_loop, WatchConclusion, WatchConfig, WatchPoller, WatchResult,
    TIMEOUT_EXIT_CODE,
};

/// Cap the number of detail refs rendered in the compact summary so a noisy
/// aggregate cannot flood recovery output. Overflow is reported as an
/// `omitted` count rather than dropped silently.
const COMPACT_REF_LIMIT: usize = 12;
const COMPACT_TASK_LIMIT: usize = 12;
const COMPACT_TEXT_LIMIT: usize = 512;
const COMPACT_ACTION_LIMIT: usize = 4;
const COMPACT_ACTION_BYTE_LIMIT: usize = 512;
const COMPACT_STATUS_BYTE_LIMIT: usize = 16 * 1024;
const COMPACT_MANDATORY_SCALAR_BYTE_LIMIT: usize = 512;
const STATUS_WATCH_CHANGE_LIMIT: usize = 12;
const STATUS_WATCH_BYTE_LIMIT: usize = 32 * 1024;
const STATUS_WATCH_CHANGE_BYTE_LIMIT: usize = 8 * 1024;
const STATUS_WATCH_EVENT_BYTE_LIMIT: usize = 4 * 1024;
const STATUS_WATCH_CHANGE_PAYLOAD_BYTE_LIMIT: usize = 2 * 1024;
const BOUNDED_FULL_STATUS_BYTE_LIMIT: usize = 16 * 1024;

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
    let evidence_command = format!("homeboy agent-task evidence {} --full", quote_arg(run_id));
    let mut evidence_refs = stable_evidence_refs(&value);
    if evidence_refs.is_empty() {
        evidence_refs.push(json!({
            "run_id": run_id,
            "ref": format!("homeboy://agent-task/run/{}/evidence", homeboy::core::execution_contract::encode_uri_component(run_id)),
            "command": evidence_command,
            "export_command": format!("{evidence_command} --output <path>"),
        }));
    }
    let output = json!({
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
        "evidence_refs": evidence_refs,
        "output_budget": {
            "max_bytes": BOUNDED_FULL_STATUS_BYTE_LIMIT,
            "lossless_output": "--output <path>",
            "deduplicated": true,
            "truncated": true,
        },
    });
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
fn stable_evidence_refs(value: &Value) -> Vec<Value> {
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

/// Cook IDs are logical candidate readers. Exact attempt IDs remain immutable
/// attempt readers, even when a newer Cook attempt produced no patch.
pub(super) struct CookReaderTarget {
    pub(super) run_id: String,
    pub(super) selection: Option<Value>,
}

pub(super) fn resolve_cook_reader_target(
    run_or_cook_id: &str,
    exact: bool,
) -> homeboy::core::Result<CookReaderTarget> {
    // One store for the whole target resolution. Both branches ask the same
    // question about the same Cook — does its index exist, and what does it
    // say — and separately resolved homes can disagree about the answer
    // (#7505).
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    if exact {
        return Ok(CookReaderTarget {
            run_id: run_or_cook_id.to_string(),
            selection: None,
        });
    }
    if !agent_task_lifecycle::cook_index_exists_in_store(&lifecycle_store, run_or_cook_id)? {
        if let Some(materializing) =
            agent_task_lifecycle::resolve_detached_cook_materializing_attempt_in_store(
                &lifecycle_store,
                run_or_cook_id,
            )?
        {
            return Ok(CookReaderTarget {
                run_id: materializing.run_id.clone(),
                selection: None,
            });
        }
        return Ok(CookReaderTarget {
            run_id: run_or_cook_id.to_string(),
            selection: None,
        });
    }
    let selection = agent_task_service_direct::select_cook_candidate(run_or_cook_id)?;
    if selection.incomplete || selection.run_id.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "cook_id",
            "candidate selection is incomplete after its bounded recovery window",
            Some(run_or_cook_id.to_string()),
            None,
        ));
    }
    Ok(CookReaderTarget {
        run_id: selection.run_id.clone(),
        selection: Some(serde_json::to_value(selection).unwrap_or(Value::Null)),
    })
}

pub(super) fn status(args: StatusArgs) -> CmdResult<Value> {
    if args.watch {
        return watch_status(args);
    }
    status_once(args)
}

fn status_once(args: StatusArgs) -> CmdResult<Value> {
    if let Some(recipe_only) = recipe_only_status(&args.run_id, args.exact)? {
        return Ok((recipe_only, 0));
    }
    let target = resolve_cook_reader_target(&args.run_id, args.exact)?;
    let run = agent_task_service::control_plane_run(&target.run_id)?;
    let exit_code = if args.strict_subject_exit && control_plane_run_requires_action(&run) {
        1
    } else {
        0
    };
    Ok((serde_json::to_value(run).unwrap_or(Value::Null), exit_code))
}

fn control_plane_run_requires_action(
    run: &homeboy_control_plane_contract::ControlPlaneRun,
) -> bool {
    use homeboy_control_plane_contract::{ControlPlaneAction, ControlPlaneActionAvailability};

    run.action_eligibility.as_ref().is_some_and(|report| {
        report.actions.iter().any(|action| {
            matches!(
                action.action,
                ControlPlaneAction::Resume
                    | ControlPlaneAction::Retry
                    | ControlPlaneAction::Review
                    | ControlPlaneAction::Promote
            ) && action.availability == ControlPlaneActionAvailability::Available
        })
    })
}

/// Automatic retention runs while a task completes but can inventory every
/// workspace sharing its roots. Keep that global operational evidence durable
/// and addressable without allowing it to obscure this run's diagnosis.
pub(crate) fn cleanup_evidence_projection(value: &mut Value, run_id: &str) -> Vec<Value> {
    let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let run_ref = homeboy::core::execution_contract::encode_uri_component(run_id);
    [
        "automatic_artifact_retention",
        "automatic_artifact_retention_inaccessible_roots",
    ]
    .into_iter()
    .filter_map(|key| {
        let details = metadata.remove(key)?;
        let count = details
            .get("worktree_count")
            .and_then(Value::as_u64)
            .or_else(|| details.as_array().map(|items| items.len() as u64))
            .or_else(|| details.get("worktrees").and_then(Value::as_array).map(|items| items.len() as u64))
            .unwrap_or(0);
        Some(json!({
            "kind": key,
            "count": count,
            "details_omitted": true,
            "ref": format!("homeboy://agent-task/run/{run_ref}/status#metadata.{key}"),
            "command": format!("homeboy agent-task status {}", quote_arg(run_id)),
            "export_command": format!("homeboy agent-task status {} --output <path>", quote_arg(run_id)),
        }))
    })
    .collect()
}

struct StatusPoller {
    args: StatusArgs,
}

impl WatchPoller for StatusPoller {
    type Item = (Value, i32);

    fn poll(&self, _id: &str) -> homeboy::core::Result<Self::Item> {
        status_once(self.args.clone())
    }

    fn is_terminal(&self, item: &Self::Item) -> bool {
        status_is_terminal(&item.0)
    }
}

fn watch_status(mut args: StatusArgs) -> CmdResult<Value> {
    let interval = parse_duration("--interval", &args.interval)?;
    let timeout = parse_duration("--timeout", &args.timeout)?;
    args.watch = false;
    let poller = StatusPoller { args: args.clone() };
    let started = Instant::now();
    let mut progress = StatusWatchProgress::default();
    let result = watch_loop(
        &poller,
        &args.run_id,
        &WatchConfig {
            interval,
            timeout: Some(timeout),
        },
        std::thread::sleep,
        || started.elapsed(),
        |(snapshot, _), poll| progress.observe(snapshot, poll, |line| eprintln!("{line}")),
    )?;
    Ok(watch_status_output(&args, result, progress))
}

#[derive(Default)]
struct StatusWatchProgress {
    last_change: Option<Value>,
    changes: Vec<Value>,
    omitted: u64,
}

impl StatusWatchProgress {
    fn observe(&mut self, snapshot: &Value, poll: u64, mut emit: impl FnMut(String)) {
        let change = status_change_projection(snapshot);
        if self.last_change.as_ref() == Some(&change) {
            return;
        }
        self.last_change = Some(change);
        emit(emit_status_change_event(
            snapshot,
            poll,
            self.changes.len() >= STATUS_WATCH_CHANGE_LIMIT,
        ));
        if self.changes.len() < STATUS_WATCH_CHANGE_LIMIT {
            self.changes.push(status_watch_change(snapshot, poll));
        } else {
            self.omitted += 1;
        }
    }
}

fn emit_status_change_event(snapshot: &Value, poll: u64, retained_limit_reached: bool) -> String {
    let run_id = status_run_id(snapshot).or_else(|| {
        snapshot
            .pointer("/identity/resolved_run_id")
            .and_then(Value::as_str)
    });
    let mut event = json!({
        "schema": "homeboy/agent-task-status-watch-event/v2",
        "event": "status_changed",
        "run_id": run_id,
        "state": status_run_state(snapshot),
        "poll": poll,
        "change": status_watch_change(snapshot, poll),
        "retained_limit_reached": retained_limit_reached,
        "continuation_command": run_id.map(|run_id| format!(
            "homeboy agent-task status {} --watch", quote_arg(run_id)
        )),
    });
    if serialized_len(&event) > STATUS_WATCH_EVENT_BYTE_LIMIT {
        event["change"] = json!({
            "run_id": run_id,
            "state": status_run_state(snapshot),
            "change_basis": status_change_digest_projection(snapshot),
            "full_status_ref": snapshot.get("full_command"),
        });
    }
    let line = serde_json::to_string(&event).expect("status watch JSONL event serializes");
    if line.len() <= STATUS_WATCH_EVENT_BYTE_LIMIT {
        return line;
    }
    event["change"] = json!({
        "run_id": run_id,
        "state": status_run_state(snapshot),
        "full_status_ref": snapshot.get("full_command"),
    });
    serde_json::to_string(&event).expect("bounded status watch JSONL event serializes")
}

fn status_watch_change(snapshot: &Value, poll: u64) -> Value {
    let change = json!({
        "poll": poll,
        "run_id": status_run_id(snapshot),
        "state": status_run_state(snapshot),
        "phase": snapshot.get("phase"),
        "blocker": snapshot.get("blocker"),
        "heartbeat_at": snapshot.get("heartbeat_at"),
        "candidate": snapshot.get("candidate"),
        "gates": snapshot.get("gates"),
        "publication": snapshot.get("publication"),
        "action_eligibility": snapshot.get("action_eligibility"),
        "change_basis": status_change_projection(snapshot),
    });
    if serialized_len(&change) > STATUS_WATCH_CHANGE_PAYLOAD_BYTE_LIMIT {
        return json!({
            "poll": poll,
            "run_id": status_run_id(snapshot),
            "state": status_run_state(snapshot),
            "change_basis": status_change_digest_projection(snapshot),
        });
    }
    change
}

fn status_change_projection(status: &Value) -> Value {
    json!({
        "state": status_run_state(status),
        "phase": status.get("phase"),
        "blocker": status.get("blocker"),
        "heartbeat_at": status.get("heartbeat_at"),
        "candidate": status.get("candidate"),
        "gates": status.get("gates"),
        "publication": status.get("publication"),
        "action_eligibility": status.get("action_eligibility"),
    })
}

fn status_change_digest_projection(status: &Value) -> Value {
    status_change_projection(status)
}

fn status_is_terminal(status: &Value) -> bool {
    !matches!(
        status_run_state(status).and_then(Value::as_str),
        Some("queued" | "running" | "in_flight")
    )
}

fn status_is_failure(status: &Value) -> bool {
    let Some(state) = status_run_state(status).and_then(Value::as_str) else {
        // Recipe-only Cook status is a successful read whose recovery state is
        // intentionally carried under `status`, not lifecycle `state`.
        return false;
    };
    !matches!(
        state,
        "queued"
            | "running"
            | "in_flight"
            | "succeeded"
            | "review_ready"
            | "draft_published"
            | "green_no_finalize"
            | "no_changes"
            | "intentional_no_change"
    )
}

fn watch_status_output(
    args: &StatusArgs,
    result: WatchResult<(Value, i32)>,
    progress: StatusWatchProgress,
) -> (Value, i32) {
    let timed_out = result.conclusion == WatchConclusion::TimedOut;
    let terminal = result.conclusion == WatchConclusion::Terminal;
    let (observed_latest, status_exit) = result.item;
    let continuation = format!(
        "homeboy agent-task status {} --watch --interval {} --timeout {}",
        quote_arg(&args.run_id),
        args.interval,
        args.timeout
    );
    let total_changes = progress
        .changes
        .len()
        .saturating_add(progress.omitted as usize);
    let (changes, budget) = budget_json_values(
        progress.changes,
        total_changes,
        OutputBudget {
            max_items: STATUS_WATCH_CHANGE_LIMIT,
            max_bytes: STATUS_WATCH_CHANGE_BYTE_LIMIT,
            max_events: None,
            max_seconds: None,
        },
        continuation.clone(),
        format!(
            "homeboy agent-task status {} --output <path>",
            quote_arg(&args.run_id)
        ),
    );
    let latest = status_watch_latest(&observed_latest);
    let terminal_summary =
        terminal.then(|| status_watch_terminal_summary(&observed_latest, &args.run_id));
    let mut output = json!({
        "schema": "homeboy/agent-task-status-watch/v2",
        "command": "agent-task.status.watch",
        "run_id": args.run_id,
        "terminal": terminal,
        "timed_out": timed_out,
        "poll_count": result.poll_count,
        "waited_secs": result.waited.as_secs(),
        "changes": changes,
        "changes_omitted": budget.omitted_items,
        "latest": latest,
        "terminal_summary": terminal_summary,
        "continuation_command": continuation,
        "output_budget": budget,
    });
    enforce_watch_output_budget(&mut output, &continuation);
    let exit_code = if timed_out {
        TIMEOUT_EXIT_CODE
    } else if status_is_failure(&output["latest"]) {
        1
    } else {
        status_exit
    };
    (output, exit_code)
}

fn status_watch_latest(snapshot: &Value) -> Value {
    snapshot.clone()
}

/// Keep the final stdout object bounded as a whole, not merely its changes.
fn enforce_watch_output_budget(output: &mut Value, continuation: &str) {
    while serialized_len(output) > STATUS_WATCH_BYTE_LIMIT {
        let removed_change = output
            .get_mut("changes")
            .and_then(Value::as_array_mut)
            .and_then(Vec::pop)
            .is_some();
        if !removed_change {
            if output["terminal_summary"].is_object()
                && output["terminal_summary"]["schema"]
                    != "homeboy/agent-task-status-watch-terminal-ref/v2"
            {
                output["terminal_summary"] = json!({
                    "schema": "homeboy/agent-task-status-watch-terminal-ref/v2",
                    "run_id": output["run_id"],
                    "state": output["latest"]["state"],
                    "command": format!("homeboy agent-task status {}", quote_arg(output["run_id"].as_str().unwrap_or_default())),
                });
                continue;
            }
            if output["latest"].is_object()
                && output["latest"]["schema"] != "homeboy/agent-task-status-watch-latest-ref/v2"
            {
                output["latest"] = json!({
                    "schema": "homeboy/agent-task-status-watch-latest-ref/v2",
                    "run_id": output["run_id"],
                    "state": output["latest"]["state"],
                    "command": format!("homeboy agent-task status {}", quote_arg(output["run_id"].as_str().unwrap_or_default())),
                });
                continue;
            }
            break;
        }
        let omitted = output["changes_omitted"].as_u64().unwrap_or_default() + 1;
        output["changes_omitted"] = json!(omitted);
        output["output_budget"]["truncated"] = json!(true);
        output["output_budget"]["continuation_command"] = json!(continuation);
    }
}

fn status_watch_terminal_summary(snapshot: &Value, run_id: &str) -> Value {
    json!({
        "schema": "homeboy/agent-task-status-watch-terminal/v1",
        "run_id": status_run_id(snapshot).unwrap_or(run_id),
        "state": status_run_state(snapshot),
        "phase": snapshot.get("phase"),
        "blocker": snapshot.get("blocker"),
        "candidate": snapshot.get("candidate"),
        "gates": snapshot.get("gates"),
        "publication": snapshot.get("publication"),
        "action_eligibility": snapshot.get("action_eligibility"),
    })
}

fn status_run_id(status: &Value) -> Option<&str> {
    status
        .get("run")
        .or_else(|| status.get("run_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            status
                .pointer("/control_plane_run/run")
                .and_then(Value::as_str)
        })
}

fn status_run_state(status: &Value) -> Option<&Value> {
    status
        .get("state")
        .or_else(|| status.pointer("/control_plane_run/state"))
}

pub(super) fn parse_event_cursor(
    cursor: Option<&str>,
    field: &'static str,
) -> homeboy::core::Result<Option<homeboy_control_plane_contract::EventCursor>> {
    cursor
        .map(homeboy_control_plane_contract::EventCursor::new)
        .transpose()
        .map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                field,
                error.to_string(),
                cursor.map(str::to_string),
                None,
            )
        })
}

/// Status is a read-only diagnostic. A recipe-only Cook is recoverable through
/// its exact attempt id, but only `cook-continue` may materialize that attempt.
fn recipe_only_status(run_or_cook_id: &str, exact: bool) -> homeboy::core::Result<Option<Value>> {
    let recipe = match agent_task_service_direct::load_recipe(run_or_cook_id) {
        Ok(recipe) => recipe,
        Err(_) => match agent_task_service_direct::load_recipe_for_attempt(run_or_cook_id)? {
            Some(recipe) => recipe,
            None => return Ok(None),
        },
    };
    let attempt = if recipe.cook_id == run_or_cook_id && !exact {
        recipe
            .attempts
            .last()
            .expect("validated recipe has an attempt")
    } else {
        let Some(attempt) = recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == run_or_cook_id)
        else {
            return Ok(None);
        };
        attempt
    };
    if agent_task_lifecycle::run_record_exists_readonly(&attempt.run_id)? {
        return Ok(None);
    }
    Ok(Some(json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": recipe.cook_id,
        "run_id": attempt.run_id,
        "latest_run_id": attempt.run_id,
        "status": "recipe_only_recovery_required",
        "lifecycle_state": "recipe_persisted_without_lifecycle_record",
        "provider_budget_consumed": false,
        "provider_executions_consumed": 0,
        "guidance": {
            "action": "materialize_recipe_attempt",
            "command": agent_task_service_direct::cook_continue_command(None, &attempt.run_id, false, None),
            "message": "The immutable Cook recipe is durable but its lifecycle record is absent. Continue the exact attempt to materialize the controller lifecycle before provider work."
        }
    })))
}

/// Attach the canonical run resource assembled from the durable snapshot.
fn promotion_state(record: &Value) -> String {
    let raw = record
        .pointer("/metadata/latest_promotion/status")
        .and_then(Value::as_str)
        .unwrap_or("not_attempted");
    raw.to_string()
}

fn promotion_gate_state(record: &Value, promotion: &str) -> &'static str {
    if record
        .pointer("/metadata/latest_promotion/deterministic_gates")
        .and_then(Value::as_array)
        .is_some_and(|gates| {
            gates.iter().any(|gate| {
                gate.get("status").and_then(Value::as_str) == Some("accepted_inherited_failure")
            })
        })
    {
        return "accepted_inherited_failure";
    }
    match promotion {
        "applied" | "verified_no_changes" => "passed",
        "gate_failed" | "no_changes_gate_failed" => "failed",
        "verification_pending" => "pending",
        _ => "not_run",
    }
}

fn finalization_state(record: &Value) -> String {
    record
        .pointer("/metadata/cook_finalization/status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "review_ready" | "draft_published" => "completed".to_string(),
            "failed" | "finalization_failed" => "finalization_failed".to_string(),
            "pending" | "finalization_pending" => "finalization_pending".to_string(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "not_attempted".to_string())
}

fn attach_durable_read_availability(
    value: &mut Value,
    unavailable_sources: &[agent_task_lifecycle::AgentTaskDurableReadUnavailable],
) {
    if let Value::Object(map) = value {
        map.insert(
            "durable_read".to_string(),
            json!({
                "phase": "controller_local",
                "unavailable_sources": unavailable_sources,
            }),
        );
    }
}

const OPERATOR_HEAVY_FIELDS: &[&str] = &[
    "diff",
    "patch",
    "stdout",
    "stderr",
    "current_diff",
    "transcript",
    "runtime_log",
];
const OPERATOR_HEAVY_COLLECTIONS: &[&str] =
    &["raw_events", "resource_timeline", "cook_resource_timeline"];

pub(crate) fn project_operator_output(value: &mut Value) {
    project_operator_value(value, false);
}

fn project_operator_value(value: &mut Value, evidence_content: bool) {
    match value {
        Value::Array(items) => {
            for item in items {
                project_operator_value(item, evidence_content);
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
                project_heavy_collection(fields, &key);
            }
            for (key, item) in fields.iter_mut() {
                if OPERATOR_HEAVY_FIELDS.contains(&key.as_str())
                    || (evidence_content && key == "body")
                {
                    if let Value::String(text) = item {
                        if text.len() > COMPACT_TEXT_LIMIT && !is_bounded_failure_result(text) {
                            let digest = content_hash::sha256_hex(text.as_bytes());
                            *text = format!("[omitted {} bytes; sha256={digest}]", text.len());
                        }
                    }
                    continue;
                }
                if OPERATOR_HEAVY_COLLECTIONS.contains(&key.as_str()) {
                    continue;
                }
                project_operator_value(item, hydrated_evidence && key == "content");
            }
        }
        _ => {}
    }
}

fn is_bounded_failure_result(text: &str) -> bool {
    if text.len() > 8_192 {
        return false;
    }
    let Ok(Value::Object(result)) = serde_json::from_str(text) else {
        return false;
    };
    let failed = result.get("success").and_then(Value::as_bool) == Some(false)
        || result
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "blocked" | "error" | "failed" | "failure" | "timed_out" | "timeout"
                )
            });
    failed
}

#[cfg(test)]
mod bounded_failure_result_tests {
    use super::{is_bounded_failure_result, COMPACT_TEXT_LIMIT};

    #[test]
    fn retains_code_less_payloads_for_every_liftable_failure_status() {
        for status in [
            "blocked",
            "error",
            "failed",
            "failure",
            "timed_out",
            "timeout",
        ] {
            let payload = serde_json::json!({
                "status": status,
                "message": "x".repeat(COMPACT_TEXT_LIMIT),
            })
            .to_string();
            assert!(payload.len() > COMPACT_TEXT_LIMIT);
            assert!(is_bounded_failure_result(&payload), "status={status}");
        }
    }

    #[test]
    fn retains_success_false_without_a_code() {
        let payload = serde_json::json!({
            "success": false,
            "message": "x".repeat(COMPACT_TEXT_LIMIT),
        })
        .to_string();
        assert!(is_bounded_failure_result(&payload));
    }
}

/// Known event streams retain their array/item schema. The owning evidence
/// object receives additive metadata describing the omitted durable events.
fn project_heavy_collection(fields: &mut serde_json::Map<String, Value>, key: &str) {
    let Some(items) = fields.get_mut(key).and_then(Value::as_array_mut) else {
        return;
    };
    if items.len() <= COMPACT_REF_LIMIT {
        return;
    }
    let total_items = items.len();
    let digest = content_hash::sha256_hex(serde_json::to_vec(items).as_deref().unwrap_or_default());
    items.truncate(COMPACT_REF_LIMIT);
    fields.insert(
        format!("{key}_projection"),
        json!({
            "total_items": total_items,
            "returned_items": COMPACT_REF_LIMIT,
            "omitted_items": total_items - COMPACT_REF_LIMIT,
            "sha256": digest,
        }),
    );
}

pub(super) fn list_runs(
    filter: agent_task_service::AgentTaskDiscoveryFilter,
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let report = agent_task_service_direct::discover_runs_with_options(filter, options)?;
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    attach_agent_task_discovery_actionable(&mut value, None);
    Ok((value, 0))
}

pub(super) fn list_filtered_latest_runs(
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let report = agent_task_service_direct::discover_filtered_latest_run(options)?;
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    attach_agent_task_discovery_actionable(&mut value, None);
    Ok((value, 0))
}

/// `agent-task active`: list queued + running runs, but SEPARATE them into
/// active / stale / suspect / unreconciled buckets so a stale or orphaned
/// `running` record (especially a Lab/offloaded run whose runner process died)
/// is never silently treated as genuinely-active (#5682).
///
/// The base discovery report (with per-run liveness, source, last-update age,
/// and a per-run safe reconcile command) is preserved under `report`, and a
/// `buckets` view groups run ids by classification for an at-a-glance triage.
pub(super) fn list_active(
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let report = agent_task_service_direct::discover_runs_with_options(
        agent_task_service::AgentTaskDiscoveryFilter::Active,
        options,
    )?;
    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);

    let buckets = active_liveness_buckets(&report);
    if let Value::Object(map) = &mut value {
        map.insert("buckets".to_string(), buckets);
        map.insert(
            "reconcile_hint".to_string(),
            json!("run the per-run `commands.reconcile` preview, then repeat it with `--apply` after reviewing authoritative provider state"),
        );
    }
    attach_agent_task_discovery_actionable(&mut value, Some("homeboy agent-task active"));
    Ok((value, 0))
}

/// `agent-task active --reconcile`: preview stale/suspect/unreconciled records
/// across the fleet. `--apply` is required to mutate the previewed set (#10001).
pub(super) fn reconcile_active(dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_service_direct::reconcile_stale_active_runs(dry_run)?;
    let exit = if report.failed > 0 { 1 } else { 0 };
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), exit))
}

/// `agent-task reconcile <run-id>` addresses one exact record or the explicit
/// parent/attempt group named by a logical Cook ID. It previews by default;
/// `--apply` is the explicit operator authorization.
pub(super) fn reconcile_run(args: ReconcileArgs) -> CmdResult<Value> {
    let run_id = args.run_id;
    let (mut value, exit, acknowledgement) = if args.apply {
        let acknowledgement =
            homeboy::agents::orchestration::execute_action_from_current_environment(
                &run_id,
                &homeboy_control_plane_contract::ControlPlaneActionRequest {
                    schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA
                        .to_string(),
                    action: homeboy_control_plane_contract::ControlPlaneAction::Reconcile,
                    idempotency_key: args
                        .idempotency_key
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    actor: "homeboy-cli".to_string(),
                    expected_updated_at: None,
                    parameters: homeboy_control_plane_contract::ControlPlaneActionPayload::empty(),
                    confirmed: true,
                },
            )?;
        let exit = i32::from(matches!(
            acknowledgement.outcome,
            homeboy_control_plane_contract::ControlPlaneActionOutcome::Failed
        ));
        (
            acknowledgement.result.data.clone(),
            exit,
            Some(acknowledgement),
        )
    } else {
        let report = agent_task_service_direct::reconcile_run(&run_id, true)?;
        let exit = i32::from(report.failed > 0);
        (
            serde_json::to_value(report).unwrap_or(Value::Null),
            exit,
            None,
        )
    };
    if let Value::Object(object) = &mut value {
        object.insert("owner".to_string(), json!("durable_agent_tasks"));
        object.insert(
            "scope".to_string(),
            json!(format!("durable run or Cook group `{run_id}`")),
        );
        object.insert(
            "postcondition".to_string(),
            json!(if !args.apply {
                "reports the selected durable records against authoritative provider state without persisted mutation"
            } else {
                "every selected durable record is reconciled to authoritative provider state"
            }),
        );
        if let Some(acknowledgement) = acknowledgement {
            object.insert(
                "action_acknowledgement".to_string(),
                json!(acknowledgement.acknowledgement),
            );
            object.insert(
                "idempotency_key".to_string(),
                json!(acknowledgement.idempotency_key),
            );
        }
    }
    Ok((value, exit))
}

pub(super) fn reconcile_records(dry_run: bool) -> CmdResult<Value> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let report = agent_task_lifecycle::reconcile_record_health_in_store(&lifecycle_store, dry_run)?;
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

/// Group active-run ids by liveness classification for a scannable triage view.
///
/// The bucket names, the "no classification means active" default, and the
/// reconcilability of each bucket all come from [`AgentTaskLiveness`] itself
/// rather than from a hand-rolled match here. The previous version restated the
/// four-way mapping in the CLI, which is precisely the duplication #W3-4 is
/// about: an orchestrator reading `liveness: "suspect"` had to reimplement the
/// same table to know whether it was allowed to reconcile.
fn active_liveness_buckets(report: &agent_task_service::AgentTaskDiscoveryReport) -> Value {
    use agent_task_service_direct::AgentTaskLiveness;

    let mut buckets = serde_json::Map::new();
    for liveness in AgentTaskLiveness::ALL {
        buckets.insert(liveness.as_str().to_string(), Value::Array(Vec::new()));
    }

    for run in &report.runs {
        // A run with no classification (the `all`/`latest` filters do not
        // classify) is treated as active — the behaviour the previous
        // `Some(Active) | None` arm encoded.
        let liveness = run.liveness.unwrap_or(AgentTaskLiveness::Active);
        let entry = json!({
            "run_id": run.run_id,
            "state": run.state,
            "source": run.source,
            "last_update": run.last_update,
            "last_update_age_minutes": run.last_update_age_minutes,
            "stale_reason": run.stale_reason,
            "reconcilable": liveness.is_reconcilable(),
        });
        if let Some(Value::Array(bucket)) = buckets.get_mut(liveness.as_str()) {
            bucket.push(entry);
        }
    }

    Value::Object(buckets)
}

fn lab_transport_receipt_from_details(details: &Value) -> Option<LabTransportAttemptReceipt> {
    serde_json::from_value(details.get("lab_transport_attempt_receipt")?.clone()).ok()
}

fn lab_transport_receipt(record: &AgentTaskRunRecord) -> Option<LabTransportAttemptReceipt> {
    lab_transport_receipt_from_details(record.metadata.pointer("/pre_execution_failure/details")?)
}

fn lab_transport_failure_projection(record: &AgentTaskRunRecord, _run_id: &str) -> Option<Value> {
    let receipt = lab_transport_receipt(record)?;
    let action = lab_transport_repair_action_for_receipt(&receipt);
    Some(json!({
        "receipt": receipt,
        "recovery": {
            "kind": "repair_selected_runner_transport",
            "command": action.command,
        },
    }))
}

fn lab_transport_repair_action(record: &AgentTaskRunRecord) -> Option<CommandNextAction> {
    lab_transport_receipt(record).map(|receipt| lab_transport_repair_action_for_receipt(&receipt))
}

fn lab_transport_repair_action_for_receipt(
    receipt: &LabTransportAttemptReceipt,
) -> CommandNextAction {
    let runner = quote_arg(&receipt.selected_runner);
    CommandNextAction::new(
        format!(
            "diagnose and repair Lab transport for {}",
            receipt.selected_runner
        ),
        format!("homeboy runner doctor {runner} --scope lab-offload --repair"),
    )
    .with_kind(CommandNextActionKind::Repair)
}

const DISCOVERY_NEXT_ACTION_LIMIT: usize = 8;

fn attach_agent_task_discovery_actionable(value: &mut Value, active_command: Option<&str>) {
    let runs = value
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut metadata = CommandActionableMetadata::default();

    if value
        .get("liveness_summary")
        .and_then(Value::as_object)
        .is_some_and(|summary| {
            ["stale", "suspect", "unreconciled"].iter().any(|bucket| {
                summary
                    .get(*bucket)
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    > 0
            })
        })
    {
        metadata.next_actions.push(
            CommandNextAction::new(
                "preview stale record reconciliation",
                "homeboy agent-task active --reconcile --dry-run",
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }
    if let (Some(command), Some(cursor), Some(limit)) = (
        active_command,
        value.get("next_cursor").and_then(Value::as_u64),
        value.get("limit").and_then(Value::as_u64),
    ) {
        metadata.next_actions.push(
            CommandNextAction::new(
                "show next page",
                format!("{command} --limit {limit} --cursor {cursor}"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }

    for run in runs.iter().take(20) {
        let Some(run_id) = run.get("run_id").and_then(Value::as_str) else {
            continue;
        };
        metadata.refs.agent_tasks.push(agent_task_ref(run_id));
        if let Some(commands) = run.get("commands") {
            push_command_action(
                &mut metadata.next_actions,
                "show status",
                commands.get("status"),
                CommandNextActionKind::Show,
            );
            push_command_action(
                &mut metadata.next_actions,
                "show logs",
                commands.get("logs"),
                CommandNextActionKind::Show,
            );
            push_command_action(
                &mut metadata.next_actions,
                "list artifacts",
                commands.get("artifacts"),
                CommandNextActionKind::Artifacts,
            );
            if run
                .get("liveness")
                .and_then(Value::as_str)
                .is_some_and(|liveness| liveness != "active")
            {
                push_command_action(
                    &mut metadata.next_actions,
                    "reconcile stale run",
                    commands.get("reconcile"),
                    CommandNextActionKind::Repair,
                );
            }
        }
    }
    metadata.next_actions.truncate(DISCOVERY_NEXT_ACTION_LIMIT);

    attach_actionable_metadata(value, metadata);
}

fn agent_task_ref(run_id: &str) -> CommandAgentTaskRef {
    CommandAgentTaskRef {
        id: run_id.to_string(),
        source: "homeboy-agent-task-lifecycle".to_string(),
        status_command: format!("homeboy agent-task status {run_id}"),
        logs_command: format!("homeboy agent-task logs {run_id}"),
        review_command: Some(format!("homeboy agent-task review {run_id}")),
    }
}

fn push_command_action(
    actions: &mut Vec<CommandNextAction>,
    label: &str,
    command: Option<&Value>,
    kind: CommandNextActionKind,
) {
    let Some(command) = command.and_then(Value::as_str) else {
        return;
    };
    actions.push(CommandNextAction::new(label, command).with_kind(kind));
}

fn attach_actionable_metadata(value: &mut Value, metadata: CommandActionableMetadata) {
    if metadata.is_empty() {
        return;
    }
    let Value::Object(map) = value else {
        return;
    };
    if let Ok(metadata) = serde_json::to_value(metadata) {
        map.insert(ACTIONABLE_METADATA_KEY.to_string(), metadata);
    }
}

/// Follow-up lifecycle commands retain the exact durable placement decision
/// that authorized their owning Cook attempt.
fn preserve_controller_owner_placement(value: &mut Value, run_id: &str) {
    let recovery_prefix = agent_task_service_direct::cook_recovery_command_prefix(run_id);
    preserve_controller_owner_placement_with_prefix(value, run_id, &recovery_prefix);
}

fn preserve_controller_owner_placement_with_prefix(
    value: &mut Value,
    run_id: &str,
    recovery_prefix: &str,
) {
    match value {
        Value::String(command) => {
            let command_prefix = "homeboy agent-task ";
            let lifecycle_command = command
                .strip_prefix(command_prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .is_some_and(|operation| {
                    matches!(
                        operation,
                        "status"
                            | "logs"
                            | "artifacts"
                            | "evidence"
                            | "diagnose"
                            | "review"
                            | "retry"
                            | "reconcile"
                            | "cancel"
                            | "cook-continue"
                            | "finalize-pr"
                    )
                });
            if lifecycle_command && command.contains(run_id) {
                *command =
                    command.replacen(command_prefix, &format!("{recovery_prefix} agent-task "), 1);
            }
        }
        Value::Array(values) => {
            for value in values {
                preserve_controller_owner_placement_with_prefix(value, run_id, recovery_prefix);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                preserve_controller_owner_placement_with_prefix(value, run_id, recovery_prefix);
            }
        }
        _ => {}
    }
}

pub(super) fn logs(args: LogsArgs) -> CmdResult<Value> {
    let cursor = parse_event_cursor(args.cursor.as_deref(), "cursor")?;
    let events = agent_task_service_direct::logs_from_cursor(&args.run_id, cursor.as_ref())?;
    Ok((serde_json::to_value(events).unwrap_or(Value::Null), 0))
}

pub(super) fn artifacts(args: LifecycleReadArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let mut value = serde_json::to_value(artifacts).unwrap_or(Value::Null);
    if !false {
        attach_collection_budget(
            &mut value,
            "artifacts",
            &format!(
                "homeboy agent-task artifacts {} --full",
                quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy agent-task artifacts {} --full --output <path>",
                quote_arg(&args.run_id)
            ),
        );
    }
    preserve_controller_owner_placement(&mut value, &args.run_id);
    Ok((value, 0))
}

pub(super) fn evidence(args: EvidenceArgs) -> CmdResult<Value> {
    let target = resolve_cook_reader_target(&args.run_id, false)?;
    let run_id = &target.run_id;
    let durable_read = agent_task_lifecycle::durable_local_read(run_id)?;
    let artifacts = agent_task_service::artifacts(run_id)?;
    let aggregate = durable_read.aggregate;
    let failed_tasks = failed_task_statuses(aggregate.as_ref());
    let plan = agent_task_lifecycle::load_plan(run_id).ok();

    let mut hydrated = Vec::new();
    let mut total = 0;
    for (evidence_ref, task_id) in
        evidence_refs_with_tasks(&artifacts.evidence_refs, aggregate.as_ref())
    {
        if args
            .kind
            .as_deref()
            .is_some_and(|kind| evidence_ref.kind != kind)
        {
            continue;
        }
        if args
            .task
            .as_deref()
            .is_some_and(|task| task_id.as_deref() != Some(task))
        {
            continue;
        }
        if args.failure_only
            && !task_id
                .as_deref()
                .is_some_and(|task| failed_tasks.contains_key(task))
        {
            continue;
        }

        total += 1;
        // Count filtered refs without hydrating their payload once the shared
        // collection budget is full.
        if !args.full && hydrated.len() >= OutputBudget::COLLECTION.max_items {
            continue;
        }
        hydrated.push(agent_task_service::hydrate_evidence_ref(
            run_id,
            &evidence_ref,
            task_id.as_deref(),
            plan.as_ref(),
            aggregate.as_ref(),
        ));
    }

    let mut value = serde_json::to_value(AgentTaskEvidenceReport {
        schema: "homeboy/agent-task-evidence/v1",
        run_id: run_id.clone(),
        filters: AgentTaskEvidenceFilters {
            kind: args.kind,
            task: args.task,
            failure_only: args.failure_only,
        },
        count: total,
        evidence_total: total,
        evidence: hydrated,
    })
    .unwrap_or(Value::Null);
    if let Some(selection) = target.selection {
        value["candidate_selection"] = selection;
    }
    attach_durable_read_availability(&mut value, &durable_read.unavailable_sources);
    if !args.full {
        attach_collection_budget(
            &mut value,
            "evidence",
            &format!(
                "homeboy agent-task evidence {} --full",
                quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy agent-task evidence {} --full --output <path>",
                quote_arg(&args.run_id)
            ),
        );
    }
    preserve_controller_owner_placement(&mut value, run_id);
    Ok((value, 0))
}

pub(super) fn diagnose(args: DiagnoseArgs) -> CmdResult<Value> {
    let target = resolve_cook_reader_target(&args.run_id, false)?;
    let run_id = &target.run_id;
    // Diagnosis starts from the bounded controller-local snapshot, then adds a
    // read-only runner snapshot only when that runner owns an abnormal run.
    let durable_read = agent_task_lifecycle::durable_local_read(run_id)?;
    let record = durable_read.record;
    let aggregate = durable_read.aggregate;
    let runner_diagnostic_probe = agent_task_lifecycle::runner_diagnostic_probe(&record);
    let mut hydrated_evidence = Vec::new();
    let mut total_hydrated_evidence = 0;
    // The current promotion lifecycle denial is the active blocker. Older
    // controller failures remain in the diagnostic chain as causal history.
    let current_lifecycle_diagnostic = current_lifecycle_diagnostic(&record);
    let mut nested_reasons = current_lifecycle_diagnostic
        .clone()
        .into_iter()
        .chain(persisted_cook_failure_diagnostic(&record))
        .collect::<Vec<_>>();
    let runner_cancellation = runner_cancellation_diagnostic(&record);
    let causal_phase = runner_cancellation
        .as_ref()
        .and_then(|diagnostic| diagnostic.data["causal_phase"].as_str())
        .map(str::to_string);
    nested_reasons.extend(runner_cancellation.clone());
    nested_reasons.extend(
        aggregate
            .as_ref()
            .map(aggregate_failure_diagnostics)
            .unwrap_or_default(),
    );
    let mut diagnostic_truncations = Vec::new();
    let mut diagnostic_budget = DiagnosticCollectionBudget::default();

    if let Some(aggregate) = aggregate.as_ref() {
        for outcome in &aggregate.outcomes {
            for evidence in &outcome.evidence_refs {
                total_hydrated_evidence += 1;
                if let Some(truncation) = collect_hydrated_evidence_diagnostics(
                    &outcome.task_id,
                    evidence,
                    &mut nested_reasons,
                    &mut diagnostic_budget,
                ) {
                    diagnostic_truncations.push(truncation);
                }
                if args.full || hydrated_evidence.len() < OutputBudget::COLLECTION.max_items {
                    if let Some(summary) =
                        agent_task_service::hydrate_evidence_summary(&outcome.task_id, evidence)
                    {
                        hydrated_evidence.push(summary);
                    }
                }
            }
        }
    }

    let ranked_reasons = ranked_diagnostics(nested_reasons);
    let root_cause = ranked_reasons
        .first()
        .cloned()
        .map(|item| collected_diagnostic_value_with_details(item, false));
    let diagnostic_chain = ranked_reasons
        .into_iter()
        .take(FAILURE_REASON_LIMIT)
        .map(|item| collected_diagnostic_value_with_details(item, false))
        .collect::<Vec<_>>();

    let missing_artifacts = aggregate
        .as_ref()
        .map(missing_artifact_summaries)
        .unwrap_or_default();
    let mut causal_chain = aggregate
        .as_ref()
        .map(causal_chain_from_aggregate)
        .unwrap_or_default();
    if causal_chain.is_empty() {
        if let Some(diagnostic) = runner_cancellation.as_ref() {
            causal_chain.push(json!({
                "task_id": diagnostic.task_id,
                "surface": "runner",
                "phase": diagnostic.data["causal_phase"],
                "status": record.state,
                "failure_classification": "runner_cancellation",
            }));
        }
    }
    let retry = retry_replay_action(&record);
    let next_commands = diagnose_next_commands(
        &record,
        retry.action.as_ref(),
        retry.continuation.as_ref(),
        current_lifecycle_diagnostic.is_some(),
    );

    let mut value = json!({
        "schema": "homeboy/agent-task-diagnose/v1",
        "run_id": record.run_id.clone(),
        "state": record.state,
        "root_cause": root_cause,
        "diagnostic_chain": diagnostic_chain,
        "causal_chain": causal_chain,
        "missing_artifacts": missing_artifacts.clone(),
        "hydrated_evidence": hydrated_evidence,
        "hydrated_evidence_total": total_hydrated_evidence,
        "diagnostic_collection": diagnostic_collection_projection(&diagnostic_truncations),
        "causal_phase": causal_phase,
        "runner_diagnostic_probe": runner_diagnostic_probe,
        "continuation_admission": record.metadata.get("cook_continuation_admission"),
        "retry_replay": retry.projection(),
        "next_commands": next_commands,
    });
    if let Some(projection) = lab_transport_failure_projection(&record, run_id) {
        value["lab_transport_failure"] = projection;
    }
    attach_durable_read_availability(&mut value, &durable_read.unavailable_sources);
    attach_cook_completion(&mut value, &record);
    if let Some(selection) = target.selection {
        value["candidate_selection"] = selection;
    }
    if !args.full {
        attach_collection_budget(
            &mut value,
            "hydrated_evidence",
            &format!("homeboy agent-task diagnose {} --full", quote_arg(run_id)),
            &format!(
                "homeboy agent-task diagnose {} --full --output <path>",
                quote_arg(run_id)
            ),
        );
    }
    // The diagnosis is only useful if it leaves the caller with the exact next
    // command for THIS failure. Everything above is already computed; this
    // projects it into the shared actionable envelope instead of discarding it.
    attach_diagnose_actionable(
        &mut value,
        &record,
        aggregate.as_ref(),
        &missing_artifacts,
        record.runner_id(),
        retry.action.as_ref(),
        retry.continuation.as_ref(),
        runner_cancellation.is_some(),
        current_lifecycle_diagnostic.is_some(),
    );
    let recovery_prefix =
        agent_task_service_direct::cook_recovery_command_prefix_for_record(&record);
    preserve_controller_owner_placement_with_prefix(&mut value, run_id, &recovery_prefix);
    Ok((value, 0))
}

/// These fields are deliberately separate from secondary compact tables. They
/// survive the byte-budget trim and give both JSON and human renderers the same
/// terminal causal answer and exactly one immediately executable action.
fn attach_cook_completion(value: &mut Value, record: &AgentTaskRunRecord) {
    let Ok(Some(recipe)) = agent_task_service::load_recipe_for_attempt(&record.run_id) else {
        return;
    };
    let selected_candidate = record
        .metadata
        .get("cook_id")
        .and_then(Value::as_str)
        .and_then(|cook_id| agent_task_service_direct::select_cook_candidate(cook_id).ok())
        .and_then(|selection| serde_json::to_value(selection).ok());
    let finalization_requested = recipe.finalization["no_finalize"] != true;
    if let Some(completion) = agent_task_service_direct::cook_completion(
        selected_candidate.as_ref(),
        finalization_requested,
        // `cook_completion` resolves this receipt from the selected candidate
        // whenever selection exists. This fallback only serves pre-index reports.
        record.metadata.get("cook_finalization"),
        Some(&record.run_id),
    ) {
        let mut completion = serde_json::to_value(completion).expect("completion serializes");
        // This legacy top-level field remains for existing consumers, but its
        // candidate fact belongs to the Cook rather than this queried attempt.
        completion["scope"] = json!("cook");
        completion["context"] = json!("selected_cook_candidate");
        value["cook_completion"] = completion;
    }
    // The completion projection answers whether a PR exists; it does not carry
    // the PR's identity. Project that beside it so `status` can answer "is there
    // a PR, and where" without a second command (#12571).
    if let Some(pr_url) = cook_finalization_pr_url(record) {
        value["pr_url"] = Value::String(pr_url.to_string());
    }
}

/// The published pull request for this Cook attempt, read from the durable
/// finalization receipt that `has_finalized_pr` accepts as publication proof.
fn cook_finalization_pr_url(record: &AgentTaskRunRecord) -> Option<&str> {
    let finalization = record.metadata.get("cook_finalization")?;
    let url = finalization
        .get("pr_url")
        .or_else(|| finalization.get("pull_request_url"))
        .and_then(Value::as_str)?
        .trim();
    (!url.is_empty()).then_some(url)
}

/// Versioned semantic boundary between one queried lifecycle attempt and the
/// Cook-wide candidate that can survive later empty or cancelled retries.
/// Legacy top-level fields intentionally retain their historical projection.
/// Basis marker for `next_actions`: the diagnosis mapped a typed failure
/// classification (or a concrete missing-artifact set) to specific commands.
const DIAGNOSE_ACTION_BASIS_DIAGNOSIS: &str = "diagnosis";
/// Basis marker for `next_actions`: nothing in the diagnosis was specific
/// enough to act on, so the generic recovery set is emitted as an explicit
/// fallback rather than as the only behavior.
const DIAGNOSE_ACTION_BASIS_FALLBACK: &str = "generic_fallback";
/// Basis marker for a recoverable canonical candidate that takes precedence
/// over a later failed provider attempt.
const DIAGNOSE_ACTION_BASIS_CANDIDATE: &str = "canonical_candidate";

/// One failed task and how its executor classified the failure. This is the
/// typed input to the classification→action table: it is read from durable
/// outcome state, never from provider prose, so an emitted action can always be
/// substantiated.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosedFailure {
    task_id: String,
    classification: AgentTaskFailureClassification,
    phase: Option<String>,
    provider_boundary_exists: bool,
    controller_runtime_recovery_available: bool,
    provider_budget_consumed: bool,
}

/// Collect the distinct failure classifications a run actually recorded, first
/// implicated task wins. Successful and no-op outcomes are never a failure
/// signal even if a stale classification survived on them.
fn diagnosed_failures(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Vec<DiagnosedFailure> {
    let mut failures: Vec<DiagnosedFailure> = Vec::new();
    for outcome in &aggregate.outcomes {
        if matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        ) {
            continue;
        }
        let Some(classification) = outcome.failure_classification else {
            continue;
        };
        if failures
            .iter()
            .any(|failure| failure.classification == classification)
        {
            continue;
        }
        failures.push(DiagnosedFailure {
            task_id: outcome.task_id.clone(),
            classification,
            phase: outcome
                .metadata
                .get("phase")
                .and_then(Value::as_str)
                .map(str::to_string),
            provider_boundary_exists: outcome
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == "executor-input"),
            controller_runtime_recovery_available: controller_runtime_recovery_available(record),
            provider_budget_consumed: outcome.metadata["provider_budget_consumed"] == true,
        });
    }
    failures
}

fn attach_diagnose_actionable(
    value: &mut Value,
    record: &AgentTaskRunRecord,
    aggregate: Option<&AgentTaskAggregate>,
    missing_artifacts: &[Value],
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
    continuation_action: Option<&CommandNextAction>,
    runner_cancellation: bool,
    current_lifecycle_denial: bool,
) {
    let run_id = record.run_id.as_str();
    if let Some(action) = lab_transport_repair_action(record) {
        if let Value::Object(map) = value {
            map.insert(
                "next_action_basis".to_string(),
                Value::String(DIAGNOSE_ACTION_BASIS_DIAGNOSIS.to_string()),
            );
        }
        attach_actionable_metadata(
            value,
            CommandActionableMetadata {
                run: Some(diagnose_run_ref(record, runner_id)),
                refs: CommandResultRefs {
                    agent_tasks: vec![agent_task_ref(run_id)],
                    ..Default::default()
                },
                next_actions: vec![action],
                ..Default::default()
            },
        );
        return;
    }
    let candidate_payload = candidate_result_payload(
        &serde_json::to_value(record).unwrap_or(Value::Null),
        aggregate,
    );
    let candidate_recoverable = classify_candidates(&candidate_payload)
        .state()
        .is_available();
    let failures = aggregate
        .map(|aggregate| diagnosed_failures(record, aggregate))
        .unwrap_or_default();
    let (next_actions, basis) = if current_lifecycle_denial {
        (
            current_lifecycle_next_actions(record),
            DIAGNOSE_ACTION_BASIS_DIAGNOSIS,
        )
    } else if let Some(continuation) = continuation_action {
        (vec![continuation.clone()], DIAGNOSE_ACTION_BASIS_CANDIDATE)
    } else if candidate_recoverable {
        (
            vec![CommandNextAction::new(
                "review the canonical candidate",
                format!("homeboy agent-task review {}", quote_arg(run_id)),
            )
            .with_kind(CommandNextActionKind::Show)],
            DIAGNOSE_ACTION_BASIS_CANDIDATE,
        )
    } else {
        diagnose_next_actions(
            run_id,
            &failures,
            missing_artifacts,
            runner_id,
            retry_action,
            runner_cancellation,
        )
    };
    if let Value::Object(map) = value {
        map.insert(
            "next_action_basis".to_string(),
            Value::String(basis.to_string()),
        );
    }
    let metadata = CommandActionableMetadata {
        run: Some(diagnose_run_ref(record, runner_id)),
        refs: CommandResultRefs {
            agent_tasks: vec![agent_task_ref(run_id)],
            ..Default::default()
        },
        next_actions,
        artifacts: aggregate.map(diagnose_artifact_refs).unwrap_or_default(),
        evidence: aggregate.map(diagnose_evidence_refs).unwrap_or_default(),
    };
    attach_actionable_metadata(value, metadata);
}

/// Derive the recovery commands for this exact failure. Returns the generic set
/// ONLY when neither a classification nor a missing-artifact set produced a
/// specific step, and labels which of the two happened.
fn diagnose_next_actions(
    run_id: &str,
    failures: &[DiagnosedFailure],
    missing_artifacts: &[Value],
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
    runner_cancellation: bool,
) -> (Vec<CommandNextAction>, &'static str) {
    let mut actions: Vec<CommandNextAction> = Vec::new();
    for failure in failures {
        for action in classification_next_actions(run_id, failure, runner_id, retry_action) {
            push_unique_next_action(&mut actions, action);
        }
    }
    for action in missing_artifact_next_actions(run_id, missing_artifacts) {
        push_unique_next_action(&mut actions, action);
    }
    if runner_cancellation {
        for action in runner_cancellation_next_actions(run_id, runner_id, retry_action) {
            push_unique_next_action(&mut actions, action);
        }
    }
    if actions.is_empty() {
        return (
            generic_diagnose_next_actions(run_id, retry_action),
            DIAGNOSE_ACTION_BASIS_FALLBACK,
        );
    }
    (actions, DIAGNOSE_ACTION_BASIS_DIAGNOSIS)
}

/// A pre-provider runner cancellation is not an unclassified provider failure.
/// Inspect the owning runner first; retry is included only when the persisted
/// replay admission already proved it safe.
fn runner_cancellation_next_actions(
    run_id: &str,
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let mut actions = vec![CommandNextAction::new(
        "show the durable runner cancellation evidence",
        format!("homeboy agent-task status {run}"),
    )
    .with_kind(CommandNextActionKind::Show)];
    actions.extend(lost_runner_actions(runner_id));
    actions.extend(retry_action.cloned());
    actions
}

fn push_unique_next_action(actions: &mut Vec<CommandNextAction>, action: CommandNextAction) {
    if actions
        .iter()
        .any(|existing| existing.command == action.command)
    {
        return;
    }
    actions.push(action);
}

/// The classification→action table. Each arm answers "what do I run next for
/// THIS kind of failure?" with commands that exist in the CLI surface and are
/// scoped to the run and task the diagnosis implicates.
fn classification_next_actions(
    run_id: &str,
    failure: &DiagnosedFailure,
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let task = quote_arg(&failure.task_id);
    let failure_evidence = CommandNextAction::new(
        format!("show failure evidence for {}", failure.task_id),
        format!("homeboy agent-task evidence {run} --task {task} --failure-only"),
    )
    .with_kind(CommandNextActionKind::Show);
    let retry = retry_action.cloned();
    let review = CommandNextAction::new(
        "review the candidate this attempt left behind",
        format!("homeboy agent-task review {run}"),
    )
    .with_kind(CommandNextActionKind::Show);
    let list_providers = CommandNextAction::new(
        "list registered providers",
        "homeboy agent-task providers".to_string(),
    )
    .with_kind(CommandNextActionKind::Show);

    if failure.phase.as_deref() == Some("controller_admission") {
        let mut actions = vec![failure_evidence];
        if failure.controller_runtime_recovery_available {
            actions.push(
                CommandNextAction::new(
                    "recover the pinned controller runtime from a trusted source checkout",
                    format!(
                        "homeboy agent-task runtime-recover {run} --source <trusted-source-checkout>"
                    ),
                )
                .with_kind(CommandNextActionKind::Repair),
            );
        }
        actions.push(
            CommandNextAction::new(
                "show the controller admission record and runtime pin",
                format!("homeboy agent-task status {run}"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
        return actions;
    }

    if failure.phase.as_deref() == Some("interrupted_owner") {
        let budget_label = if failure.provider_budget_consumed {
            "provider budget was consumed and in-flight work may be duplicated"
        } else {
            "provider budget was not consumed"
        };
        let mut actions = vec![
            failure_evidence,
            CommandNextAction::new(
                format!(
                    "show the interrupted-owner stop reason and candidate harvest evidence; {budget_label}"
                ),
                format!("homeboy agent-task status {run}"),
            )
            .with_kind(CommandNextActionKind::Show),
        ];
        if let Some(retry) = retry {
            actions.push(
                CommandNextAction::new(
                    format!("retry this Cook; {budget_label}"),
                    retry.command.clone(),
                )
                .with_kind(CommandNextActionKind::Repair),
            );
        }
        return actions;
    }

    match failure.classification {
        // The provider itself errored or was not resolvable: prove which
        // provider was asked for, and whether it is registered and ready.
        AgentTaskFailureClassification::Provider => {
            let mut actions = vec![failure_evidence, list_providers];
            actions.extend(provider_readiness_actions(runner_id));
            actions.extend(retry);
            actions
        }
        // Documented as safe to retry with bounded backoff: lead with the retry.
        AgentTaskFailureClassification::Transient => {
            retry.into_iter().chain([failure_evidence]).collect()
        }
        // A wall-clock timeout can still have left a complete candidate patch,
        // so review comes before spending another attempt.
        AgentTaskFailureClassification::Timeout => {
            let mut actions = vec![failure_evidence, review];
            actions.extend(retry);
            actions
        }
        // A silent hang: the durable record is likely still `running` and the
        // owning runner is the thing to interrogate.
        AgentTaskFailureClassification::Stalled => {
            let mut actions = vec![CommandNextAction::new(
                "reconcile the stalled run against authoritative state",
                format!("homeboy agent-task reconcile {run} --dry-run"),
            )
            .with_kind(CommandNextActionKind::Repair)];
            actions.extend(lost_runner_actions(runner_id));
            actions.push(failure_evidence);
            actions.extend(retry);
            actions
        }
        // Throttled: the evidence carries the retry-after hint, and another
        // registered provider may be able to take the work now.
        AgentTaskFailureClassification::RateLimited => {
            let mut actions = vec![
                failure_evidence,
                CommandNextAction::new(
                    "list registered providers to rotate to",
                    "homeboy agent-task providers".to_string(),
                )
                .with_kind(CommandNextActionKind::Show),
            ];
            actions.extend(retry);
            actions
        }
        // This account cannot satisfy another request until its quota, billing,
        // or credentials are repaired. Show alternative providers rather than
        // offering a same-provider retry.
        AgentTaskFailureClassification::ProviderAccountBlocked => vec![
            failure_evidence,
            CommandNextAction::new(
                "list registered providers to rotate to",
                "homeboy agent-task providers".to_string(),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
        // Policy refused this request. Retrying an identical request is denied
        // identically, so no retry action is emitted.
        AgentTaskFailureClassification::PolicyDenied => vec![
            failure_evidence,
            CommandNextAction::new(
                "show the full run record including the policy that denied it",
                format!("homeboy agent-task status {run}"),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
        // A required capability/tool was not resolvable: the readiness chain is
        // the repair, not another attempt.
        AgentTaskFailureClassification::CapabilityMissing => {
            let mut actions = vec![
                failure_evidence,
                CommandNextAction::new(
                    "show provider declarations and discovery diagnostics",
                    "homeboy agent-task providers --full".to_string(),
                )
                .with_kind(CommandNextActionKind::Show),
            ];
            actions.extend(provider_readiness_actions(runner_id));
            actions
        }
        // The request the provider received was malformed. Replaying the
        // boundary shows the exact rejected input; a retry would resend it.
        AgentTaskFailureClassification::InvalidInput => {
            let mut actions = vec![failure_evidence];
            if failure.provider_boundary_exists {
                actions.push(
                    CommandNextAction::new(
                        format!("replay the provider boundary for {}", failure.task_id),
                        format!("homeboy agent-task replay-provider-boundary {run} --task {task}"),
                    )
                    .with_kind(CommandNextActionKind::Show),
                );
            }
            actions
        }
        // The work ran and failed (gate/verify failure, harvest failure,
        // required typed artifacts missing): show what the failing step
        // recorded and what it produced before deciding to retry.
        AgentTaskFailureClassification::ExecutionFailed => {
            let mut actions = vec![
                failure_evidence,
                review,
                CommandNextAction::new(
                    "list the artifacts the run produced",
                    format!("homeboy agent-task artifacts {run} --full"),
                )
                .with_kind(CommandNextActionKind::Artifacts),
            ];
            actions.extend(retry);
            actions
        }
        // Deliberately unmapped: an unclassified failure has no substantiable
        // specific step, so the caller gets the explicit generic fallback.
        AgentTaskFailureClassification::Unknown => Vec::new(),
    }
}

/// Provider/runner readiness chain for the runner that owns the run. Emitted
/// only when a runner id is known, because `agent-task doctor` requires one.
fn provider_readiness_actions(runner_id: Option<&str>) -> Vec<CommandNextAction> {
    let Some(runner_id) = runner_id else {
        return Vec::new();
    };
    let runner = quote_arg(runner_id);
    vec![
        CommandNextAction::new(
            format!("check provider and runner readiness on {runner_id}"),
            format!("homeboy agent-task doctor --runner {runner}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            format!("repair provider and runner readiness on {runner_id}"),
            format!("homeboy agent-task doctor --runner {runner} --repair"),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

/// Runner session inspection and repair for a run whose runner went quiet.
fn lost_runner_actions(runner_id: Option<&str>) -> Vec<CommandNextAction> {
    let Some(runner_id) = runner_id else {
        return Vec::new();
    };
    let runner = quote_arg(runner_id);
    vec![
        CommandNextAction::new(
            format!("show runner session state for {runner_id}"),
            format!("homeboy runner status {runner}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            format!("diagnose and repair runner {runner_id}"),
            format!("homeboy runner doctor {runner} --scope lab-offload --repair"),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

/// Actions for the specific artifacts a task declared and did not produce.
fn missing_artifact_next_actions(
    run_id: &str,
    missing_artifacts: &[Value],
) -> Vec<CommandNextAction> {
    if missing_artifacts.is_empty() {
        return Vec::new();
    }
    let run = quote_arg(run_id);
    let mut actions = vec![CommandNextAction::new(
        "list the artifacts the run actually produced",
        format!("homeboy agent-task artifacts {run} --full"),
    )
    .with_kind(CommandNextActionKind::Artifacts)];
    for entry in missing_artifacts.iter().take(COMPACT_REF_LIMIT) {
        let Some(task_id) = entry.get("task_id").and_then(Value::as_str) else {
            continue;
        };
        let names = entry
            .get("missing")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if names.is_empty() {
            continue;
        }
        let task = quote_arg(task_id);
        actions.push(
            CommandNextAction::new(
                format!("show the declarations {task_id} was given for {names}"),
                format!("homeboy agent-task replay-provider-boundary {run} --task {task}"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
        actions.push(
            CommandNextAction::new(
                format!("show failure evidence for {task_id}"),
                format!("homeboy agent-task evidence {run} --task {task} --failure-only"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }
    actions
}

/// The pre-existing static set, retained as the explicit fallback for a failure
/// the diagnosis could not classify.
fn generic_diagnose_next_actions(
    run_id: &str,
    retry_action: Option<&CommandNextAction>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let mut actions = vec![
        CommandNextAction::new(
            "show the full run record",
            format!("homeboy agent-task status {run}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "list the artifacts the run produced",
            format!("homeboy agent-task artifacts {run}"),
        )
        .with_kind(CommandNextActionKind::Artifacts),
        CommandNextAction::new("review run", format!("homeboy agent-task review {run}"))
            .with_kind(CommandNextActionKind::Show),
    ];
    actions.extend(retry_action.cloned());
    actions
}

/// Read-only handoff commands remain legal for every persisted promotion, gate,
/// or finalization denial. A retry of an earlier provider attempt is not current
/// recovery guidance after a later lifecycle denial.
fn current_lifecycle_next_actions(record: &AgentTaskRunRecord) -> Vec<CommandNextAction> {
    let run_id = &record.run_id;
    let run = quote_arg(run_id);
    let mut actions = vec![
        CommandNextAction::new(
            "show the current promotion, gate, or finalization denial",
            format!("homeboy agent-task status {run}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "review the promoted candidate and its gate proof",
            format!("homeboy agent-task review {run}"),
        )
        .with_kind(CommandNextActionKind::Show),
    ];
    let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) else {
        return actions;
    };
    let Some(status) = current_lifecycle_status(record) else {
        return actions;
    };
    let Some(recovery) =
        agent_task_service_direct::cook_failure_context(cook_id, Some(run_id), status)
    else {
        return actions;
    };
    for action in recovery.next_actions {
        if action.action == "status" || action.action == "diagnose" {
            continue;
        }
        push_unique_next_action(
            &mut actions,
            CommandNextAction::new(
                format!("{} through the Cook recovery handoff", action.action),
                action.command,
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }
    actions
}

fn diagnose_run_ref(record: &AgentTaskRunRecord, runner_id: Option<&str>) -> CommandRunRef {
    let run = quote_arg(&record.run_id);
    CommandRunRef {
        id: record.run_id.clone(),
        kind: "agent_task".to_string(),
        source: "homeboy-agent-task-lifecycle".to_string(),
        location: Some(
            runner_id
                .map(|runner_id| format!("runner:{runner_id}"))
                .unwrap_or_else(|| "local".to_string()),
        ),
        started_at: Some(record.submitted_at.clone()),
        updated_at: record.updated_at.clone(),
        finished_at: None,
        status_command: format!("homeboy agent-task status {run}"),
        watch_command: format!("homeboy agent-task status {run} --watch"),
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::time::Duration;

    fn args() -> StatusArgs {
        StatusArgs {
            run_id: "run-1".to_string(),
            watch: true,
            interval: "250ms".to_string(),
            timeout: "2m".to_string(),
            ..Default::default()
        }
    }

    fn result(state: &str, conclusion: WatchConclusion) -> WatchResult<(Value, i32)> {
        WatchResult {
            item: (json!({ "run_id": "run-1", "state": state }), 0),
            conclusion,
            poll_count: 3,
            waited: Duration::from_secs(1),
        }
    }

    #[test]
    fn changed_status_emission_is_live_and_skips_identical_heartbeat_polls() {
        let mut progress = StatusWatchProgress::default();
        let mut emitted = Vec::new();
        let running = json!({ "run_id": "run-1", "state": "running", "progress": 1 });
        let succeeded = json!({ "run_id": "run-1", "state": "succeeded", "progress": 2 });

        progress.observe(&running, 1, |line| emitted.push(line));
        progress.observe(&running, 2, |line| emitted.push(line));
        progress.observe(&succeeded, 3, |line| emitted.push(line));

        let first: Value = serde_json::from_str(&emitted[0]).expect("first JSONL event");
        let second: Value = serde_json::from_str(&emitted[1]).expect("second JSONL event");
        assert_eq!(first["schema"], "homeboy/agent-task-status-watch-event/v2");
        assert_eq!(first["event"], "status_changed");
        assert_eq!(first["poll"], 1);
        assert_eq!(first["run_id"], "run-1");
        assert_eq!(first["state"], "running");
        assert_eq!(first["change"]["state"], "running");
        assert_eq!(first["change"]["change_basis"]["state"], "running");
        assert!(first["change"]["status"].is_null());
        assert_eq!(
            first["continuation_command"],
            "homeboy agent-task status run-1 --watch"
        );
        assert_eq!(second["poll"], 3);
        assert_eq!(second["state"], "succeeded");
        assert_eq!(progress.changes.len(), 2);
        assert_eq!(progress.changes[0]["poll"], 1);
        assert_eq!(progress.changes[1]["poll"], 3);
    }

    #[test]
    fn changed_status_ignores_volatile_read_metadata() {
        let mut progress = StatusWatchProgress::default();
        let mut emitted = Vec::new();
        progress.observe(
            &json!({ "run_id": "run-1", "state": "running", "updated_at": "2026-08-13T00:00:00Z" }),
            1,
            |line| emitted.push(line),
        );
        progress.observe(
            &json!({ "run_id": "run-1", "state": "running", "updated_at": "2026-08-13T00:00:01Z" }),
            2,
            |line| emitted.push(line),
        );

        assert_eq!(emitted.len(), 1);
    }

    #[test]
    fn changed_status_retention_and_live_output_are_bounded() {
        let mut progress = StatusWatchProgress::default();
        let mut emitted = Vec::new();
        for poll in 1..=(STATUS_WATCH_CHANGE_LIMIT as u64 + 2) {
            progress.observe(
                &json!({ "run_id": "run-1", "state": "running", "heartbeat_at": format!("2026-08-13T00:00:{poll:02}Z") }),
                poll,
                |line| emitted.push(line),
            );
        }
        let terminal_poll = STATUS_WATCH_CHANGE_LIMIT as u64 + 3;
        progress.observe(
            &json!({ "run_id": "run-1", "state": "succeeded", "heartbeat_at": "2026-08-13T00:01:00Z" }),
            terminal_poll,
            |line| emitted.push(line),
        );

        assert_eq!(progress.changes.len(), STATUS_WATCH_CHANGE_LIMIT);
        assert_eq!(progress.omitted, 3);
        assert_eq!(emitted.len(), STATUS_WATCH_CHANGE_LIMIT + 3);
        let first_overflow: Value = serde_json::from_str(&emitted[STATUS_WATCH_CHANGE_LIMIT])
            .expect("first overflow event");
        assert_eq!(first_overflow["poll"], STATUS_WATCH_CHANGE_LIMIT as u64 + 1);
        assert_eq!(first_overflow["change"]["state"], "running");
        assert!(first_overflow["change"]["change_basis"].is_object());
        let overflow: Value =
            serde_json::from_str(emitted.last().unwrap()).expect("overflow event");
        assert_eq!(overflow["retained_limit_reached"], true);
        assert_eq!(overflow["state"], "succeeded");
        assert_eq!(overflow["poll"], terminal_poll);
    }

    #[test]
    fn watch_result_has_a_total_byte_budget() {
        let oversized = json!({
            "run_id": "run-1",
            "state": "running",
            "diagnostics": "x".repeat(STATUS_WATCH_BYTE_LIMIT * 2),
        });
        let (output, _) = watch_status_output(
            &args(),
            WatchResult {
                item: (oversized.clone(), 0),
                conclusion: WatchConclusion::TimedOut,
                poll_count: 1,
                waited: Duration::from_secs(1),
            },
            StatusWatchProgress {
                changes: vec![status_watch_change(&oversized, 1)],
                ..Default::default()
            },
        );

        assert!(serialized_len(&output) <= STATUS_WATCH_BYTE_LIMIT);
        assert_eq!(output["output_budget"]["truncated"], true);
        assert_eq!(output["latest"]["state"], "running");
        assert!(output["terminal_summary"].is_null());
    }

    #[test]
    fn terminal_summary_uses_terminal_durable_state_counts_and_diagnostic() {
        let terminal = json!({
            "run_id": "run-1",
            "state": "cancelled",
            "phase": "terminal",
            "blocker": { "code": "controller_preflight", "message": "workspace is unavailable" },
            "gates": { "state": "blocked" },
            "metadata": { "large": "x".repeat(COMPACT_STATUS_BYTE_LIMIT) },
        });
        let (output, exit) = watch_status_output(
            &args(),
            WatchResult {
                item: (terminal.clone(), 0),
                conclusion: WatchConclusion::Terminal,
                poll_count: 2,
                waited: Duration::from_secs(1),
            },
            StatusWatchProgress {
                changes: vec![status_watch_change(&terminal, 1)],
                ..Default::default()
            },
        );

        assert_eq!(exit, 1);
        assert_eq!(output["terminal_summary"]["state"], "cancelled");
        assert_eq!(output["terminal_summary"]["phase"], "terminal");
        assert_eq!(
            output["terminal_summary"]["blocker"]["code"],
            "controller_preflight"
        );
        assert_eq!(output["terminal_summary"]["gates"]["state"], "blocked");
        assert!(serialized_len(&output) <= STATUS_WATCH_BYTE_LIMIT);
    }

    #[test]
    fn canonical_gate_changes_emit_a_new_event() {
        let mut progress = StatusWatchProgress::default();
        let mut emitted = Vec::new();
        progress.observe(
            &json!({ "run_id": "run-1", "state": "running", "gates": { "state": "pending" } }),
            1,
            |line| emitted.push(line),
        );
        progress.observe(
            &json!({ "run_id": "run-1", "state": "running", "gates": { "state": "failed" } }),
            2,
            |line| emitted.push(line),
        );

        assert_eq!(emitted.len(), 2);
        let second: Value = serde_json::from_str(&emitted[1]).expect("gate change event");
        assert_eq!(second["change"]["gates"]["state"], "failed");
    }

    #[test]
    fn fixed_watch_sections_fall_back_to_typed_refs_within_total_budget() {
        let mut output = json!({
            "run_id": "run-1",
            "changes": [],
            "changes_omitted": 0,
            "latest": { "schema": "homeboy/agent-task-status-watch-latest/v2", "state": "failed", "detail": "x".repeat(STATUS_WATCH_BYTE_LIMIT) },
            "terminal_summary": { "schema": "homeboy/agent-task-status-watch-terminal/v1", "detail": "x".repeat(STATUS_WATCH_BYTE_LIMIT) },
            "output_budget": {},
        });
        enforce_watch_output_budget(&mut output, "homeboy agent-task status run-1 --watch");

        assert!(serialized_len(&output) <= STATUS_WATCH_BYTE_LIMIT);
        assert_eq!(
            output["terminal_summary"]["schema"],
            "homeboy/agent-task-status-watch-terminal-ref/v2"
        );
        assert_eq!(
            output["latest"]["schema"],
            "homeboy/agent-task-status-watch-latest-ref/v2"
        );
    }

    struct ScriptedStatusPoller {
        snapshots: RefCell<VecDeque<Value>>,
    }

    impl ScriptedStatusPoller {
        fn new(snapshots: Vec<Value>) -> Self {
            Self {
                snapshots: RefCell::new(snapshots.into()),
            }
        }
    }

    impl WatchPoller for ScriptedStatusPoller {
        type Item = (Value, i32);

        fn poll(&self, _id: &str) -> homeboy::core::Result<Self::Item> {
            let mut snapshots = self.snapshots.borrow_mut();
            let snapshot = if snapshots.len() > 1 {
                snapshots.pop_front().expect("scripted snapshot")
            } else {
                snapshots.front().expect("scripted snapshot").clone()
            };
            Ok((snapshot, 0))
        }

        fn is_terminal(&self, item: &Self::Item) -> bool {
            status_is_terminal(&item.0)
        }
    }

    #[test]
    fn scripted_watch_emits_progress_before_returning_timeout_partial_status() {
        let poller = ScriptedStatusPoller::new(vec![
            json!({ "run_id": "run-1", "state": "queued" }),
            json!({ "run_id": "run-1", "state": "running" }),
        ]);
        let clock = Cell::new(Duration::ZERO);
        let mut progress = StatusWatchProgress::default();
        let mut emitted = Vec::new();
        let result = watch_loop(
            &poller,
            "run-1",
            &WatchConfig {
                interval: Duration::from_secs(1),
                timeout: Some(Duration::from_secs(2)),
            },
            |duration| clock.set(clock.get() + duration),
            || clock.get(),
            |(snapshot, _), poll| progress.observe(snapshot, poll, |line| emitted.push(line)),
        )
        .expect("scripted status watch");
        let (output, exit) = watch_status_output(&args(), result, progress);

        assert_eq!(exit, TIMEOUT_EXIT_CODE);
        assert_eq!(output["latest"]["state"], "running");
        assert_eq!(output["changes"].as_array().unwrap().len(), 2);
        assert_eq!(
            emitted.len(),
            2,
            "progress was emitted before timeout returned"
        );
        let queued: Value = serde_json::from_str(&emitted[0]).expect("queued event");
        let running: Value = serde_json::from_str(&emitted[1]).expect("running event");
        assert_eq!(queued["state"], "queued");
        assert_eq!(running["state"], "running");
    }

    #[test]
    fn terminal_success_returns_latest_status_and_zero() {
        let (output, exit) = watch_status_output(
            &args(),
            result("succeeded", WatchConclusion::Terminal),
            StatusWatchProgress {
                changes: vec![json!({ "poll": 1, "status": { "state": "succeeded" } })],
                ..Default::default()
            },
        );

        assert_eq!(exit, 0);
        assert_eq!(output["terminal"], true);
        assert_eq!(output["latest"]["state"], "succeeded");
        assert_eq!(output["changes"].as_array().unwrap().len(), 1);
        assert_eq!(
            output["continuation_command"],
            "homeboy agent-task status run-1 --watch --interval 250ms --timeout 2m"
        );
    }

    #[test]
    fn terminal_failure_and_cancellation_exit_nonzero() {
        for state in ["failed", "cancelled", "gate_failed", "finalization_failed"] {
            let (_, exit) = watch_status_output(
                &args(),
                result(state, WatchConclusion::Terminal),
                StatusWatchProgress::default(),
            );
            assert_eq!(exit, 1, "{state}");
        }
    }

    #[test]
    fn recipe_only_watch_uses_the_successful_one_shot_status_exit() {
        let (output, exit) = watch_status_output(
            &args(),
            WatchResult {
                item: (
                    json!({
                        "schema": "homeboy/agent-task-cook/v1",
                        "run_id": "run-1",
                        "status": "recipe_only_recovery_required"
                    }),
                    0,
                ),
                conclusion: WatchConclusion::Terminal,
                poll_count: 1,
                waited: Duration::ZERO,
            },
            StatusWatchProgress::default(),
        );

        assert_eq!(exit, 0);
        assert_eq!(output["latest"]["status"], "recipe_only_recovery_required");
    }

    #[test]
    fn timeout_retains_partial_runner_status_and_uses_shared_exit_code() {
        let (output, exit) = watch_status_output(
            &args(),
            WatchResult {
                item: (
                    json!({
                        "run_id": "run-1",
                        "state": "running",
                        "reconciled": true,
                        "runner_probe": { "performed": true }
                    }),
                    0,
                ),
                conclusion: WatchConclusion::TimedOut,
                poll_count: 5,
                waited: Duration::from_secs(120),
            },
            StatusWatchProgress::default(),
        );

        assert_eq!(exit, TIMEOUT_EXIT_CODE);
        assert_eq!(output["terminal"], false);
        assert_eq!(output["timed_out"], true);
        assert_eq!(output["latest"]["reconciled"], true);
        assert_eq!(output["latest"]["runner_probe"]["performed"], true);
    }

    #[test]
    fn canonical_snapshot_is_retained_without_projection_loss() {
        let snapshot = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "run-1",
            "state": "succeeded",
            "phase": "terminal"
        });
        let (output, exit) = watch_status_output(
            &args(),
            WatchResult {
                item: (snapshot.clone(), 0),
                conclusion: WatchConclusion::Terminal,
                poll_count: 1,
                waited: Duration::ZERO,
            },
            StatusWatchProgress {
                changes: vec![status_watch_change(&snapshot, 1)],
                ..Default::default()
            },
        );

        assert_eq!(exit, 0);
        assert_eq!(output["latest"]["run"], "run-1");
        assert_eq!(output["latest"]["phase"], "terminal");
    }

    #[test]
    fn terminality_keeps_active_states_open_and_accepts_unknown_terminal_states() {
        for state in ["queued", "running", "in_flight"] {
            assert!(!status_is_terminal(&json!({ "state": state })), "{state}");
        }
        assert!(status_is_terminal(&json!({ "state": "succeeded" })));
        assert!(status_is_terminal(&json!({ "state": "failed" })));
        assert!(status_is_terminal(&json!({ "state": "cancelled" })));
    }
}

fn diagnose_artifact_refs(aggregate: &AgentTaskAggregate) -> Vec<CommandArtifactRef> {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| outcome.artifacts.iter())
        .take(COMPACT_REF_LIMIT)
        .map(|artifact| CommandArtifactRef {
            id: artifact.id.clone(),
            kind: artifact.kind.clone(),
            uri: artifact
                .url
                .clone()
                .or_else(|| artifact.path.clone())
                .unwrap_or_default(),
            semantic_key: artifact.semantic_key.clone(),
        })
        .collect()
}

fn diagnose_evidence_refs(aggregate: &AgentTaskAggregate) -> Vec<CommandArtifactRef> {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| {
            outcome
                .evidence_refs
                .iter()
                .map(move |evidence| (outcome.task_id.as_str(), evidence))
        })
        .take(COMPACT_REF_LIMIT)
        .map(|(task_id, evidence)| CommandArtifactRef {
            id: format!("{task_id}:{}", evidence.kind),
            kind: evidence.kind.clone(),
            uri: evidence.uri.clone(),
            semantic_key: None,
        })
        .collect()
}

#[cfg(test)]
mod diagnose_actionable_tests {
    use super::*;

    fn failure(classification: AgentTaskFailureClassification) -> DiagnosedFailure {
        DiagnosedFailure {
            task_id: "task-a".to_string(),
            classification,
            phase: None,
            provider_boundary_exists: true,
            controller_runtime_recovery_available: false,
            provider_budget_consumed: false,
        }
    }

    fn commands(actions: &[CommandNextAction]) -> Vec<&str> {
        actions
            .iter()
            .map(|action| action.command.as_str())
            .collect()
    }

    fn repair_commands(actions: &[CommandNextAction]) -> Vec<&str> {
        actions
            .iter()
            .filter(|action| matches!(action.kind, Some(CommandNextActionKind::Repair)))
            .map(|action| action.command.as_str())
            .collect()
    }

    fn actions_for(
        classification: AgentTaskFailureClassification,
        runner_id: Option<&str>,
    ) -> Vec<CommandNextAction> {
        let retry = owner_bound_retry_action("run-1", None, json!({ "placement": "local" }));
        let (actions, basis) = diagnose_next_actions(
            "run-1",
            &[failure(classification)],
            &[],
            runner_id,
            Some(&retry),
            false,
        );
        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        actions
    }

    #[test]
    fn a_provider_failure_asks_which_provider_was_registered_before_retrying() {
        let actions = actions_for(AgentTaskFailureClassification::Provider, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task providers",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec!["homeboy --placement local agent-task retry run-1 --run"]
        );
    }

    #[test]
    fn a_transient_failure_leads_with_the_retry_it_is_documented_to_survive() {
        let actions = actions_for(AgentTaskFailureClassification::Transient, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy --placement local agent-task retry run-1 --run",
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
            ]
        );
    }

    #[test]
    fn a_timeout_reviews_the_candidate_before_spending_another_attempt() {
        let actions = actions_for(AgentTaskFailureClassification::Timeout, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task review run-1",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_stalled_run_reconciles_and_repairs_the_runner_that_went_quiet() {
        let actions = actions_for(AgentTaskFailureClassification::Stalled, Some("homeboy-lab"));

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner status homeboy-lab",
                "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_stalled_run_without_a_known_runner_never_names_one() {
        let actions = actions_for(AgentTaskFailureClassification::Stalled, None);

        assert!(!commands(&actions)
            .iter()
            .any(|command| command.starts_with("homeboy runner ")));
    }

    #[test]
    fn a_rate_limited_failure_offers_provider_rotation() {
        let actions = actions_for(AgentTaskFailureClassification::RateLimited, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task providers",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_policy_denial_never_suggests_replaying_the_denied_request() {
        let actions = actions_for(AgentTaskFailureClassification::PolicyDenied, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task status run-1",
            ]
        );
        assert!(repair_commands(&actions).is_empty());
    }

    #[test]
    fn a_missing_capability_runs_the_readiness_repair_chain_not_another_attempt() {
        let actions = actions_for(
            AgentTaskFailureClassification::CapabilityMissing,
            Some("homeboy-lab"),
        );

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task providers --full",
                "homeboy agent-task doctor --runner homeboy-lab",
                "homeboy agent-task doctor --runner homeboy-lab --repair",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec!["homeboy agent-task doctor --runner homeboy-lab --repair"]
        );
    }

    #[test]
    fn provider_malformed_input_with_a_boundary_replays_the_rejected_input() {
        let actions = actions_for(AgentTaskFailureClassification::InvalidInput, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task replay-provider-boundary run-1 --task task-a",
            ]
        );
        assert!(repair_commands(&actions).is_empty());
    }

    #[test]
    fn invalid_input_without_a_provider_boundary_never_offers_replay() {
        let retry = owner_bound_retry_action("run-1", None, json!({ "placement": "local" }));
        let failure = DiagnosedFailure {
            task_id: "task-a".to_string(),
            classification: AgentTaskFailureClassification::InvalidInput,
            phase: None,
            provider_boundary_exists: false,
            controller_runtime_recovery_available: false,
            provider_budget_consumed: false,
        };

        let (actions, basis) =
            diagnose_next_actions("run-1", &[failure], &[], None, Some(&retry), false);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        assert_eq!(
            commands(&actions),
            vec!["homeboy agent-task evidence run-1 --task task-a --failure-only"]
        );
    }

    #[test]
    fn controller_admission_uses_runtime_recovery_only_when_a_pin_is_recoverable() {
        let failure = DiagnosedFailure {
            task_id: "task-a".to_string(),
            classification: AgentTaskFailureClassification::InvalidInput,
            phase: Some("controller_admission".to_string()),
            provider_boundary_exists: false,
            controller_runtime_recovery_available: true,
            provider_budget_consumed: false,
        };

        let (actions, basis) = diagnose_next_actions("run-1", &[failure], &[], None, None, false);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task runtime-recover run-1 --source <trusted-source-checkout>",
                "homeboy agent-task status run-1",
            ]
        );
    }

    #[test]
    fn an_execution_failure_shows_the_failing_step_and_what_it_produced() {
        let actions = actions_for(AgentTaskFailureClassification::ExecutionFailed, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task review run-1",
                "homeboy agent-task artifacts run-1 --full",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn an_unclassifiable_failure_falls_back_to_the_generic_set() {
        let (actions, basis) = diagnose_next_actions(
            "run-1",
            &[failure(AgentTaskFailureClassification::Unknown)],
            &[],
            None,
            None,
            false,
        );

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task status run-1",
                "homeboy agent-task artifacts run-1",
                "homeboy agent-task review run-1",
            ]
        );
    }

    #[test]
    fn a_run_with_no_diagnosis_at_all_falls_back_to_the_generic_set() {
        let (actions, basis) = diagnose_next_actions("run-1", &[], &[], None, None, false);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(actions.len(), 3);
    }

    #[test]
    fn missing_artifacts_name_the_task_and_the_artifacts_that_were_not_produced() {
        let missing = vec![json!({
            "task_id": "task-b",
            "missing": ["concept_packet", "design_packet"],
        })];

        let (actions, basis) = diagnose_next_actions("run-1", &[], &missing, None, None, false);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task artifacts run-1 --full",
                "homeboy agent-task replay-provider-boundary run-1 --task task-b",
                "homeboy agent-task evidence run-1 --task task-b --failure-only",
            ]
        );
        assert!(actions[1].label.contains("concept_packet, design_packet"));
    }

    #[test]
    fn classification_and_missing_artifact_actions_are_merged_without_duplicates() {
        let missing = vec![json!({ "task_id": "task-a", "missing": ["patch"] })];

        let (actions, _) = diagnose_next_actions(
            "run-1",
            &[failure(AgentTaskFailureClassification::ExecutionFailed)],
            &missing,
            None,
            None,
            false,
        );

        let commands = commands(&actions);
        assert_eq!(
            commands
                .iter()
                .filter(|command| **command
                    == "homeboy agent-task evidence run-1 --task task-a --failure-only")
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| **command == "homeboy agent-task artifacts run-1 --full")
                .count(),
            1
        );
    }

    #[test]
    fn run_and_task_ids_are_shell_quoted_in_every_emitted_command() {
        let (actions, _) = diagnose_next_actions(
            "run with spaces",
            &[DiagnosedFailure {
                task_id: "task with spaces".to_string(),
                classification: AgentTaskFailureClassification::InvalidInput,
                phase: None,
                provider_boundary_exists: true,
                controller_runtime_recovery_available: false,
                provider_budget_consumed: false,
            }],
            &[],
            None,
            None,
            false,
        );

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence 'run with spaces' --task 'task with spaces' --failure-only",
                "homeboy agent-task replay-provider-boundary 'run with spaces' --task 'task with spaces'",
            ]
        );
    }

    #[test]
    fn runner_cancellation_uses_runner_evidence_before_a_proven_replay() {
        let retry = owner_bound_retry_action("run-1", Some("homeboy-lab"), json!({}));
        let (actions, basis) =
            diagnose_next_actions("run-1", &[], &[], Some("homeboy-lab"), Some(&retry), true);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task status run-1",
                "homeboy runner status homeboy-lab",
                "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
                "homeboy --runner homeboy-lab agent-task retry run-1 --run",
            ]
        );
    }
}

/// Apply the shared output primitive to a JSON collection while retaining every
/// command's established field names and schema.
fn attach_collection_budget(
    value: &mut Value,
    field: &str,
    continue_command: &str,
    export_command: &str,
) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let values = map
        .get_mut(field)
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    let total = map
        .get(&format!("{field}_total"))
        .or_else(|| map.get("total"))
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .unwrap_or(values.len());
    let (bounded, metadata) = budget_json_values(
        values,
        total,
        OutputBudget::COLLECTION,
        continue_command,
        export_command,
    );
    map.insert(field.to_string(), Value::Array(bounded));
    map.insert(
        "output_budget".to_string(),
        serde_json::to_value(metadata).unwrap_or(Value::Null),
    );
}

pub(super) fn recover_runtime(args: RuntimeRecoverArgs) -> CmdResult<Value> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let recovered = homeboy::agents::agent_task_lifecycle::recover_controller_runtime_in_store(
        &lifecycle_store,
        &args.run_id,
        args.artifact.as_deref().map(std::path::Path::new),
        args.source.as_deref().map(std::path::Path::new),
    )?;
    Ok((
        json!({ "schema": "homeboy/controller-runtime-recovery/v1", "run_id": args.run_id, "recovered": recovered }),
        0,
    ))
}

pub(super) fn validate_runtime(args: RuntimeValidateArgs) -> CmdResult<Value> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let record = homeboy::agents::agent_task_lifecycle::validate_controller_runtime_in_store(
        &lifecycle_store,
        &args.run_id,
    )?;
    Ok((
        json!({ "schema": "homeboy/controller-runtime-validation/v1", "run_id": record.run_id, "state": record.state, "executable": true }),
        0,
    ))
}

pub(super) fn replay_provider_boundary(args: ReplayProviderBoundaryArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let plan = agent_task_lifecycle::load_plan(&args.run_id).ok();
    let evidence_entries = evidence_refs_with_tasks(&artifacts.evidence_refs, aggregate.as_ref());
    let candidates = evidence_entries
        .into_iter()
        .filter(|(evidence_ref, task_id)| {
            evidence_ref.kind == "executor-input"
                && args
                    .task
                    .as_deref()
                    .is_none_or(|requested| task_id.as_deref() == Some(requested))
        })
        .collect::<Vec<_>>();

    let candidate_count = candidates.len();
    let hydrate = |evidence_ref: &AgentTaskEvidenceRef, task_id: &Option<String>| {
        agent_task_service::hydrate_evidence_ref(
            &args.run_id,
            evidence_ref,
            task_id.as_deref(),
            plan.as_ref(),
            aggregate.as_ref(),
        )
    };
    let Some((evidence_ref, task_id, hydrated)) = candidates
        .iter()
        .filter(|(evidence_ref, _)| !evidence_ref.uri.contains("/plan#"))
        .find_map(|(evidence_ref, task_id)| {
            let hydrated = hydrate(evidence_ref, task_id);
            (hydrated.status == "ok"
                && !hydrated.truncated
                && hydrated.content.get("format").and_then(Value::as_str) == Some("json"))
            .then(|| (evidence_ref.clone(), task_id.clone(), hydrated))
        })
        .or_else(|| {
            candidates.first().map(|(evidence_ref, task_id)| {
                let hydrated = hydrate(evidence_ref, task_id);
                (evidence_ref.clone(), task_id.clone(), hydrated)
            })
        })
    else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "agent-task replay-provider-boundary",
            "no executor-input evidence was found for this run",
            Some(args.run_id),
            Some(vec![
                "Run the task through an executor that records latest raw executor input evidence.".to_string(),
                "Pass --task when inspecting a multi-task run and only one task should be selected.".to_string(),
            ]),
        ));
    };

    let input = hydrated_executor_input_value(&hydrated)?;
    let boundary = provider_boundary_projection(&input);
    let report = json!({
        "schema": "homeboy/agent-task-provider-boundary-replay/v1",
        "run_id": args.run_id,
        "task_id": task_id,
        "selected_evidence": {
            "kind": evidence_ref.kind,
            "label": evidence_ref.label,
            "uri": evidence_ref.uri,
        },
        "selection": {
            "matching_executor_input_count": candidate_count,
            "rule": "prefer concrete executor-input evidence over plan-level input refs; otherwise first matching ref wins",
        },
        "normalized_provider_boundary": boundary,
    });
    let evidence_uri = agent_task_service::persist_provider_boundary_replay_evidence(&report);
    let report = match evidence_uri {
        Some(uri) => {
            let mut report = report;
            report["typed_evidence"] = json!({
                "kind": "provider-boundary-replay",
                "uri": uri,
                "label": "provider boundary replay inspection",
            });
            report
        }
        None => report,
    };

    Ok((report, 0))
}

/// Cancellation is only partly synchronous, so `cancel` reports what actually
/// happened rather than an unqualified success word.
///
/// The canonical control-plane action returns as soon as the cancellation
/// *request* is durable. For a controller-owned staging job that is strictly an
/// acknowledgement — `controller_job_cancellation` is persisted with phase
/// `requested` and the controller keeps tearing its provider down afterwards —
/// and for a run whose provider tree is not reachable from this host the durable
/// terminal state is published by whoever owns it. Reporting `succeeded` for
/// that acknowledgement is what made #12572 dishonest: the word claimed the run
/// was cancelled while its process tree was still alive.
///
/// So this waits for the durable record to converge, and bounds that wait. The
/// bound sits well above controller-local teardown (process termination allows a
/// 2s SIGTERM grace plus a 2s SIGKILL reap grace) and well below the two-minute
/// wrapper timeouts operators and agents run `cancel` under, so the command
/// always answers before its caller gives up on it.
const CANCEL_TERMINAL_WAIT: Duration = Duration::from_secs(15);

/// Poll interval inside [`CANCEL_TERMINAL_WAIT`]. Each poll is a reconciling
/// read, so it stays coarse rather than hammering the durable store.
const CANCEL_TERMINAL_POLL_INTERVAL: Duration = Duration::from_secs(1);

const CANCELLATION_SCHEMA: &str = "homeboy/agent-task-cancellation/v1";

pub(super) fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let idempotency_key = args
        .idempotency_key
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let acknowledgement = homeboy::agents::orchestration::execute_action_from_current_environment(
        &args.run_id,
        &homeboy_control_plane_contract::ControlPlaneActionRequest {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: homeboy_control_plane_contract::ControlPlaneAction::Cancel,
            idempotency_key,
            actor: "homeboy-cli".to_string(),
            expected_updated_at: None,
            parameters: homeboy_control_plane_contract::ControlPlaneActionPayload {
                schema: homeboy_control_plane_contract::CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA
                    .to_string(),
                data: json!({ "reason": args.reason }),
            },
            confirmed: true,
        },
    )?;
    let record = agent_task_lifecycle::status(acknowledgement.run.as_str())?;
    if record.state.is_terminal() {
        return Ok(attach_action_acknowledgement(
            cancel_output(
                &args.run_id,
                record,
                CancelOutcome::Terminal {
                    waited: Duration::ZERO,
                    polls: 0,
                },
            ),
            &acknowledgement,
        ));
    }
    // A provider that reserved a terminal result before cancellation could apply
    // deliberately keeps the run joinable for that import, so there is nothing to
    // converge on and nothing to wait for. Say that instead of polling a record
    // cancellation was intentionally not applied to.
    if record
        .metadata
        .get("cancellation_deferred_for_terminal_provider")
        .is_some()
    {
        return Ok(attach_action_acknowledgement(
            cancel_output(
                &args.run_id,
                record,
                CancelOutcome::DeferredForTerminalProvider,
            ),
            &acknowledgement,
        ));
    }
    Ok(attach_action_acknowledgement(
        wait_for_cancellation_to_settle(&args.run_id, record),
        &acknowledgement,
    ))
}

fn attach_action_acknowledgement(
    (mut value, exit_code): (Value, i32),
    acknowledgement: &homeboy_control_plane_contract::ControlPlaneActionAcknowledgement,
) -> (Value, i32) {
    if let Some(cancellation) = value.get_mut("cancellation").and_then(Value::as_object_mut) {
        cancellation.insert(
            "acknowledgement".to_string(),
            json!(acknowledgement.acknowledgement),
        );
        cancellation.insert(
            "idempotency_key".to_string(),
            json!(acknowledgement.idempotency_key),
        );
    }
    (value, exit_code)
}

/// Poll the durable record of a run whose cancellation was accepted.
///
/// This is the reconciling read rather than a raw record read on purpose: an
/// asynchronously cancelled controller-owned job converges through
/// `reconcile_controller_job_cancellation`, which only runs on that path. The
/// runner probe is disabled so an unavailable runner cannot stretch each poll
/// and eat the bound.
struct CancelTerminalPoller;

impl WatchPoller for CancelTerminalPoller {
    type Item = AgentTaskRunRecord;

    fn poll(&self, run_id: &str) -> homeboy::core::Result<Self::Item> {
        Ok(agent_task_lifecycle::reconcile_status_with_options(
            run_id,
            agent_task_lifecycle::AgentTaskStatusOptions {
                runner_probe: agent_task_lifecycle::AgentTaskRunnerProbe::Never,
            },
        )?
        .record)
    }

    fn is_terminal(&self, item: &Self::Item) -> bool {
        item.state.is_terminal()
    }
}

/// Why `agent-task cancel` stopped waiting.
enum CancelOutcome {
    /// The run is durably terminal: either it already was when the cancellation
    /// request returned, or it converged inside the bounded wait.
    Terminal { waited: Duration, polls: u64 },
    /// A provider reserved a terminal result first, so cancellation was
    /// deliberately not applied and the run stays joinable for that import.
    DeferredForTerminalProvider,
    /// Cancellation is durably requested, but its teardown is owned elsewhere
    /// and had not converged when the bound expired.
    Requested {
        waited: Duration,
        polls: u64,
        /// Set when the bounded observation itself failed. The cancellation
        /// request is already durable, so a failed *read* of its convergence is
        /// reported here rather than as a failed cancellation.
        observation_error: Option<String>,
    },
}

/// Wait a bounded time for an accepted cancellation to become durably terminal.
fn wait_for_cancellation_to_settle(
    requested_run_id: &str,
    accepted: AgentTaskRunRecord,
) -> (Value, i32) {
    let run_id = accepted.run_id.clone();
    // A command that is about to block for seconds says so, so the wait is
    // legible instead of looking like the #12572 hang it replaces.
    eprintln!(
        "Cancellation of agent-task run {run_id} was accepted; waiting up to {}s for its durable terminal state.",
        CANCEL_TERMINAL_WAIT.as_secs()
    );
    let started = Instant::now();
    let waited = watch_loop(
        &CancelTerminalPoller,
        &run_id,
        &WatchConfig {
            interval: CANCEL_TERMINAL_POLL_INTERVAL,
            timeout: Some(CANCEL_TERMINAL_WAIT),
        },
        std::thread::sleep,
        || started.elapsed(),
        |_, _| {},
    );
    match waited {
        Ok(result) if result.timed_out() => cancel_output(
            requested_run_id,
            result.item,
            CancelOutcome::Requested {
                waited: result.waited,
                polls: result.poll_count,
                observation_error: None,
            },
        ),
        Ok(result) => cancel_output(
            requested_run_id,
            result.item,
            CancelOutcome::Terminal {
                waited: result.waited,
                polls: result.poll_count,
            },
        ),
        // The cancellation request is already durable. A failed observation of
        // its convergence is an unconverged wait, never a failed cancellation.
        Err(error) => cancel_output(
            requested_run_id,
            accepted,
            CancelOutcome::Requested {
                waited: started.elapsed(),
                polls: 0,
                observation_error: Some(error.message),
            },
        ),
    }
}

/// Project one cancellation attempt onto the durable record it acted on.
///
/// Every field is additive: the serialized record keeps its historical shape and
/// gains `cancellation`, `summary`, and — only when the bound expired — the
/// `timed_out` command status that stops this from reading as a completed
/// cancellation.
fn cancel_output(
    requested_run_id: &str,
    record: AgentTaskRunRecord,
    outcome: CancelOutcome,
) -> (Value, i32) {
    let run_id = record.run_id.clone();
    let state = run_state_name(record.state);
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    surface_cancellation_recovery(&mut value);
    let (cancellation, summary, exit_code) =
        cancellation_projection(requested_run_id, &run_id, &state, &outcome);
    let status_command = cancellation["status_command"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let converged = cancellation["terminal"].as_bool().unwrap_or(false);
    if let Value::Object(fields) = &mut value {
        fields.insert("cancellation".to_string(), cancellation);
        fields.insert("summary".to_string(), json!(&summary));
        if exit_code != 0 {
            fields.insert("status".to_string(), json!("timed_out"));
        }
    }

    let mut metadata = CommandActionableMetadata {
        refs: CommandResultRefs {
            agent_tasks: vec![agent_task_ref(&run_id)],
            ..Default::default()
        },
        next_actions: vec![CommandNextAction::new("show status", status_command)
            .with_kind(CommandNextActionKind::Show)],
        ..Default::default()
    };
    if !converged {
        metadata.next_actions.push(
            CommandNextAction::new(
                "reconcile run",
                format!(
                    "homeboy agent-task reconcile {} --dry-run",
                    quote_arg(&run_id)
                ),
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }
    attach_actionable_metadata(&mut value, metadata);
    (value, exit_code)
}

/// Build the cancellation projection, its operator-facing summary, and the exit
/// code for one outcome. Pure so every reported wording is directly testable.
fn cancellation_projection(
    requested_run_id: &str,
    run_id: &str,
    state: &str,
    outcome: &CancelOutcome,
) -> (Value, String, i32) {
    let status_command = format!("homeboy agent-task status {}", quote_arg(run_id));
    let mut cancellation = json!({
        "schema": CANCELLATION_SCHEMA,
        "requested_run_id": requested_run_id,
        "run_id": run_id,
        "state": state,
        "accepted": true,
        "wait_timeout_secs": CANCEL_TERMINAL_WAIT.as_secs(),
        "status_command": status_command,
    });
    let wait_accounting = match outcome {
        CancelOutcome::Terminal { waited, polls } => Some((*waited, *polls)),
        CancelOutcome::Requested { waited, polls, .. } => Some((*waited, *polls)),
        CancelOutcome::DeferredForTerminalProvider => None,
    };
    if let Some((waited, polls)) = wait_accounting {
        cancellation["waited_secs"] = json!(waited.as_secs());
        cancellation["poll_count"] = json!(polls);
    }

    let (outcome_name, terminal, summary, exit_code) = match outcome {
        CancelOutcome::Terminal { .. } if state == "cancelled" => (
            "cancelled",
            true,
            format!(
                "Cancellation of agent-task run {run_id} took effect: its durable state is cancelled."
            ),
            0,
        ),
        CancelOutcome::Terminal { .. } => (
            "terminal_without_cancellation",
            true,
            format!(
                "Cancellation of agent-task run {run_id} was requested, but the run reached terminal \
                 state `{state}` instead of cancelled; that terminal result is authoritative."
            ),
            0,
        ),
        CancelOutcome::DeferredForTerminalProvider => (
            "deferred_for_terminal_provider",
            false,
            format!(
                "Cancellation of agent-task run {run_id} was deliberately not applied: a provider had \
                 already reserved a terminal result, so the run stays joinable for that import. \
                 Check `{status_command}`."
            ),
            0,
        ),
        CancelOutcome::Requested {
            observation_error, ..
        } => {
            let mut summary = format!(
                "Cancellation of agent-task run {run_id} was accepted and its teardown is still in \
                 flight: the run did not reach a terminal state within {}s. Check \
                 `{status_command}`.",
                CANCEL_TERMINAL_WAIT.as_secs()
            );
            if let Some(error) = observation_error {
                cancellation["observation_error"] = json!(error);
                summary = format!("{summary} Observing that convergence failed: {error}.");
            }
            ("cancellation_requested", false, summary, TIMEOUT_EXIT_CODE)
        }
    };
    cancellation["outcome"] = json!(outcome_name);
    cancellation["terminal"] = json!(terminal);
    cancellation["message"] = json!(&summary);
    (cancellation, summary, exit_code)
}

fn run_state_name(state: agent_task_lifecycle::AgentTaskRunState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|state| state.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod cancellation_outcome_tests {
    use super::*;

    #[test]
    fn a_converged_cancellation_reports_the_cancelled_state_and_succeeds() {
        let (projection, summary, exit_code) = cancellation_projection(
            "cook-12572",
            "agent-task-12572",
            "cancelled",
            &CancelOutcome::Terminal {
                waited: Duration::from_secs(2),
                polls: 3,
            },
        );

        assert_eq!(exit_code, 0);
        assert_eq!(projection["schema"], CANCELLATION_SCHEMA);
        assert_eq!(projection["outcome"], "cancelled");
        assert_eq!(projection["terminal"], true);
        assert_eq!(projection["requested_run_id"], "cook-12572");
        assert_eq!(projection["run_id"], "agent-task-12572");
        assert_eq!(projection["waited_secs"], 2);
        assert_eq!(projection["poll_count"], 3);
        assert_eq!(projection["message"], summary);
        assert!(
            summary.contains("agent-task-12572"),
            "unexpected: {summary}"
        );
    }

    /// The #12572 acceptance: an unconverged cancellation must not be reported
    /// with a success word, and it must name the run plus a next command.
    #[test]
    fn an_unconverged_cancellation_times_out_with_the_run_id_and_a_next_command() {
        let (projection, summary, exit_code) = cancellation_projection(
            "agent-task-12572",
            "agent-task-12572",
            "running",
            &CancelOutcome::Requested {
                waited: CANCEL_TERMINAL_WAIT,
                polls: 15,
                observation_error: None,
            },
        );

        assert_eq!(exit_code, TIMEOUT_EXIT_CODE);
        assert_eq!(projection["outcome"], "cancellation_requested");
        assert_eq!(projection["terminal"], false);
        assert_eq!(projection["accepted"], true);
        assert_eq!(projection["state"], "running");
        assert_eq!(
            projection["wait_timeout_secs"],
            CANCEL_TERMINAL_WAIT.as_secs()
        );
        assert_eq!(
            projection["status_command"],
            "homeboy agent-task status agent-task-12572"
        );
        assert!(
            summary.contains("agent-task-12572"),
            "unexpected: {summary}"
        );
        assert!(
            summary.contains("homeboy agent-task status agent-task-12572"),
            "unexpected: {summary}"
        );
        assert!(!summary.contains("succeeded"), "unexpected: {summary}");
    }

    #[test]
    fn a_failed_convergence_observation_is_not_a_failed_cancellation() {
        let (projection, summary, exit_code) = cancellation_projection(
            "agent-task-12572",
            "agent-task-12572",
            "running",
            &CancelOutcome::Requested {
                waited: Duration::from_secs(1),
                polls: 1,
                observation_error: Some("daemon unreachable".to_string()),
            },
        );

        assert_eq!(exit_code, TIMEOUT_EXIT_CODE);
        assert_eq!(projection["outcome"], "cancellation_requested");
        assert_eq!(projection["accepted"], true);
        assert_eq!(projection["observation_error"], "daemon unreachable");
        assert!(
            summary.contains("daemon unreachable"),
            "unexpected: {summary}"
        );
    }

    /// A cancellation that lost the race to a terminal provider result must not
    /// claim the run was cancelled.
    #[test]
    fn a_run_that_went_terminal_another_way_is_reported_as_such() {
        let (projection, summary, exit_code) = cancellation_projection(
            "agent-task-12572",
            "agent-task-12572",
            "succeeded",
            &CancelOutcome::Terminal {
                waited: Duration::from_secs(1),
                polls: 2,
            },
        );

        assert_eq!(exit_code, 0);
        assert_eq!(projection["outcome"], "terminal_without_cancellation");
        assert_eq!(projection["terminal"], true);
        assert!(summary.contains("succeeded"), "unexpected: {summary}");
    }

    #[test]
    fn a_deferred_cancellation_reports_the_deferral_without_a_wait() {
        let (projection, summary, exit_code) = cancellation_projection(
            "agent-task-12572",
            "agent-task-12572",
            "running",
            &CancelOutcome::DeferredForTerminalProvider,
        );

        assert_eq!(exit_code, 0);
        assert_eq!(projection["outcome"], "deferred_for_terminal_provider");
        assert_eq!(projection["terminal"], false);
        assert!(projection.get("waited_secs").is_none());
        assert!(
            summary.contains("deliberately not applied"),
            "unexpected: {summary}"
        );
    }
}

pub(super) fn quarantine(args: QuarantineArgs) -> CmdResult<Value> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let record = agent_task_lifecycle::quarantine_queued_run_exact_in_store(
        &lifecycle_store,
        &args.run_id,
        &args.reason,
    )?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

pub(super) fn rearm(args: RearmArgs) -> CmdResult<Value> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let record =
        agent_task_lifecycle::rearm_quarantined_run_in_store(&lifecycle_store, &args.run_id)?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn hydrated_executor_input_value(
    result: &agent_task_service::AgentTaskHydratedEvidence,
) -> homeboy::core::Result<Value> {
    if result.status != "ok" {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "executor-input evidence",
            "selected executor-input evidence could not be hydrated",
            result.error.clone(),
            None,
        ));
    }
    if result.truncated {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "executor-input evidence",
            "selected executor-input evidence is truncated and cannot be replayed deterministically",
            Some(result.uri.clone()),
            None,
        ));
    }
    match result.content.get("format").and_then(Value::as_str) {
        Some("json") => Ok(result.content.get("value").cloned().unwrap_or(Value::Null)),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "executor-input evidence",
            "selected executor-input evidence is not JSON",
            Some(result.uri.clone()),
            None,
        )),
    }
}

fn provider_boundary_projection(input: &Value) -> Value {
    let mut executor_config = input
        .get("executor")
        .and_then(|executor| executor.get("config"))
        .cloned()
        .unwrap_or(Value::Null);
    normalize_runtime_env_path_aliases_for_replay(&mut executor_config);
    let runtime_task = input
        .get("inputs")
        .and_then(|inputs| inputs.get("runtime_task"))
        .cloned()
        .unwrap_or(Value::Null);
    let package_descriptor = runtime_task
        .pointer("/input/package")
        .or_else(|| input.pointer("/inputs/package_descriptor"))
        .or_else(|| input.pointer("/metadata/package_descriptor"))
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "runtime_task": runtime_task,
        "provider_config": executor_config,
        "runtime_component_paths": executor_config.get("runtime_component_paths").cloned().unwrap_or(Value::Null),
        "runtime_env": executor_config.get("runtime_env").cloned().unwrap_or(Value::Null),
        "artifact_declarations": input.get("artifact_declarations").cloned().unwrap_or(Value::Null),
        "package_descriptor": package_descriptor,
    })
}

fn normalize_runtime_env_path_aliases_for_replay(config: &mut Value) {
    let aliases = runtime_env_path_aliases_for_replay(config);
    if aliases.is_empty() {
        return;
    }

    let Some(root) = config.as_object_mut() else {
        return;
    };
    let component_paths = root
        .get("runtime_component_paths")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if component_paths.is_empty() {
        return;
    }

    if !root.get("runtime_env").is_some_and(Value::is_object) {
        root.insert(
            "runtime_env".to_string(),
            Value::Object(serde_json::Map::new()),
        );
    }

    let Some(runtime_env) = root.get_mut("runtime_env").and_then(Value::as_object_mut) else {
        return;
    };
    for (component_key, env_name) in aliases {
        if let Some(selected_path) = component_paths
            .get(&component_key)
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            runtime_env.insert(env_name, Value::String(selected_path));
        }
    }
}

fn runtime_env_path_aliases_for_replay(config: &Value) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    if let Some(map) = config
        .get("runtime_env_path_aliases")
        .and_then(Value::as_object)
    {
        for (component_key, env_names) in map {
            collect_runtime_env_aliases_for_replay(component_key, env_names, &mut aliases);
        }
    }
    if let Some(map) = config.get("runtime_env_aliases").and_then(Value::as_object) {
        for (env_name, component_key) in map {
            if let Some(component_key) = component_key.as_str() {
                aliases.push((component_key.to_string(), env_name.to_string()));
            }
        }
    }
    aliases
}

fn collect_runtime_env_aliases_for_replay(
    component_key: &str,
    value: &Value,
    aliases: &mut Vec<(String, String)>,
) {
    match value {
        Value::String(env_name) => aliases.push((component_key.to_string(), env_name.to_string())),
        Value::Array(items) => {
            for item in items {
                if let Some(env_name) = item.as_str() {
                    aliases.push((component_key.to_string(), env_name.to_string()));
                }
            }
        }
        _ => {}
    }
}

#[derive(Serialize)]
struct AgentTaskEvidenceReport {
    schema: &'static str,
    run_id: String,
    filters: AgentTaskEvidenceFilters,
    count: usize,
    evidence_total: usize,
    evidence: Vec<agent_task_service::AgentTaskHydratedEvidence>,
}

#[derive(Serialize)]
struct AgentTaskEvidenceFilters {
    kind: Option<String>,
    task: Option<String>,
    failure_only: bool,
}

fn failed_task_statuses(
    aggregate: Option<&AgentTaskAggregate>,
) -> HashMap<String, AgentTaskOutcomeStatus> {
    aggregate
        .into_iter()
        .flat_map(|aggregate| aggregate.outcomes.iter())
        .filter(|outcome| {
            matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Failed
                    | AgentTaskOutcomeStatus::ProviderError
                    | AgentTaskOutcomeStatus::Timeout
                    | AgentTaskOutcomeStatus::UnableToRemediate
            )
        })
        .map(|outcome| (outcome.task_id.clone(), outcome.status))
        .collect()
}

fn evidence_refs_with_tasks(
    refs: &[AgentTaskEvidenceRef],
    aggregate: Option<&AgentTaskAggregate>,
) -> Vec<(AgentTaskEvidenceRef, Option<String>)> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    if let Some(aggregate) = aggregate {
        for outcome in &aggregate.outcomes {
            for evidence_ref in &outcome.evidence_refs {
                if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
                    entries.push((evidence_ref.clone(), Some(outcome.task_id.clone())));
                }
            }
            if let Some(workflow) = &outcome.workflow {
                for step in &workflow.steps {
                    for evidence_ref in &step.artifact_refs {
                        if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
                            entries.push((evidence_ref.clone(), Some(outcome.task_id.clone())));
                        }
                    }
                }
            }
        }
    }
    for evidence_ref in refs {
        if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
            entries.push((
                evidence_ref.clone(),
                agent_task_service::evidence_ref_task_id(evidence_ref),
            ));
        }
    }
    entries
}

/// Hoist live-cancellation recovery details to the top level of the cancel
/// response so an operator sees the exact safe commands + process identifiers
/// without digging through `metadata` (#5680 acceptance: never force manual
/// process spelunking).
fn surface_cancellation_recovery(value: &mut Value) {
    let metadata = value.get("metadata").cloned().unwrap_or(Value::Null);

    if let Some(live) = metadata.get("live_cancellation").cloned() {
        value["live_cancellation"] = live;
    }

    if let Some(unsupported) = metadata.get("live_cancellation_unsupported").cloned() {
        let recovery_commands = unsupported
            .get("recovery_commands")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let reason = unsupported
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("live cancellation is not available for this provider on this host");
        value["live_cancellation_unsupported"] = unsupported.clone();
        value["recovery"] = json!({
            "message": format!(
                "Live cancellation could not signal the provider process tree directly: {reason}. Run the commands below to terminate it safely.",
            ),
            "owner_pid": unsupported.get("owner_pid").cloned().unwrap_or(Value::Null),
            "runner_id": unsupported.get("runner_id").cloned().unwrap_or(Value::Null),
            "runner_job_id": unsupported.get("runner_job_id").cloned().unwrap_or(Value::Null),
            "recovery_commands": recovery_commands,
        });
    }
}

fn collect_hydrated_evidence_diagnostics(
    task_id: &str,
    evidence: &AgentTaskEvidenceRef,
    diagnostics: &mut Vec<CollectedDiagnostic>,
    budget: &mut DiagnosticCollectionBudget,
) -> Option<Value> {
    if budget.evidence_refs >= DIAGNOSTIC_EVIDENCE_REF_LIMIT {
        return Some(budget.truncation(task_id, evidence, "evidence_refs"));
    }
    budget.evidence_refs += 1;
    let Some(value) = agent_task_service_direct::hydrate_evidence_diagnostics(evidence) else {
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

const DIAGNOSTIC_EVIDENCE_REF_LIMIT: usize = 64;
const DIAGNOSTIC_COLLECTION_COUNT_LIMIT: usize = 256;
const DIAGNOSTIC_COLLECTION_BYTE_LIMIT: usize = 256 * 1024;

#[derive(Default)]
struct DiagnosticCollectionBudget {
    evidence_refs: usize,
    diagnostics: usize,
    bytes: usize,
}

impl DiagnosticCollectionBudget {
    fn truncation(&self, task_id: &str, evidence: &AgentTaskEvidenceRef, reason: &str) -> Value {
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

fn diagnostic_collection_projection(truncations: &[Value]) -> Value {
    json!({
        "truncated_evidence": truncations.len(),
        "truncations": truncations.iter().take(COMPACT_REF_LIMIT).collect::<Vec<_>>(),
        "truncations_omitted": truncations.len().saturating_sub(COMPACT_REF_LIMIT),
    })
}

pub(crate) fn completed_run_aggregate(
    run_id: &str,
) -> Option<homeboy::core::Result<AgentTaskAggregate>> {
    match agent_task_lifecycle::durable_local_read(run_id) {
        Ok(snapshot) => snapshot.aggregate.map(Ok),
        Err(error) if error.code == homeboy::core::ErrorCode::ValidationInvalidArgument => None,
        Err(error) => Some(Err(error)),
    }
}

pub(crate) fn diagnostic_summary_from_aggregate(aggregate: &AgentTaskAggregate) -> Option<Value> {
    ranked_diagnostics(aggregate_failure_diagnostics(aggregate))
        .into_iter()
        .map(collected_diagnostic_value)
        .next()
}

/// Project terminal execution facts into stable machine-readable states. These
/// fields deliberately derive from typed outcome and lifecycle values, never
/// provider summary prose or diagnostic messages.
pub(crate) fn execution_states_from_aggregate(
    aggregate: &AgentTaskAggregate,
    record: &Value,
) -> Value {
    let review =
        homeboy::agents::agent_tasks::AgentTaskAggregateReport::from(aggregate.outcomes.clone());
    let canonical = classify_candidates(&candidate_result_payload(record, Some(aggregate)));
    let candidate_state = canonical.state();
    let provider = aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            let state = if matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Failed
                    | AgentTaskOutcomeStatus::ProviderError
                    | AgentTaskOutcomeStatus::Timeout
                    | AgentTaskOutcomeStatus::UnableToRemediate
                    | AgentTaskOutcomeStatus::Cancelled
            ) {
                "failed"
            } else {
                "succeeded"
            };
            json!({
                "task_id": outcome.task_id,
                "state": state,
                "outcome_status": outcome.status,
                "failure_classification": outcome.failure_classification,
            })
        })
        .collect::<Vec<_>>();
    let candidates = review
        .tasks
        .iter()
        .map(|task| {
            let reason_code = if task.status == AgentTaskOutcomeStatus::NoOp {
                "no_changes_produced"
            } else if task.decision
                == homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::ApplyCandidate
                && candidate_state != CandidateState::PatchAvailable
            {
                candidate_state.as_str()
            } else {
                match task.decision {
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::NoOp => {
                    "no_changes_produced"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::ApplyCandidate => {
                    "patch_available"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::RetryCandidate => {
                    "provider_retry_required"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::IssueReportCandidate => {
                    "issue_report_required"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::ReviewCandidate => {
                    "review_required"
                }
                }
            };
            json!({
                "task_id": task.task_id,
                "state": task.decision,
                "reason_code": reason_code,
            })
        })
        .collect::<Vec<_>>();
    let promotion_status = promotion_state(record);
    let finalization_status = finalization_state(record);
    let patch_promoted = matches!(
        promotion_status.as_str(),
        "verification_pending" | "applied" | "gate_failed"
    );

    json!({
        "schema": "homeboy/agent-task-execution-states/v1",
        "provider": provider,
        "candidate": {
            "state": candidate_state.as_str(),
            "tasks": candidates,
            "scan": {
                "attempts_omitted": canonical.attempts_omitted,
                "outcomes_omitted": canonical.outcomes_omitted,
                "artifacts_omitted": canonical.artifacts_omitted,
                "degraded": canonical.is_degraded(),
            },
        },
        "gate": {
            "state": promotion_gate_state(record, &promotion_status),
        },
        "promotion": {
            "state": promotion_status,
            "patch_promoted": patch_promoted,
        },
        "finalization": { "state": finalization_status },
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

fn aggregate_failure_diagnostics(aggregate: &AgentTaskAggregate) -> Vec<CollectedDiagnostic> {
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
    let scan: Vec<&homeboy::agents::agent_tasks::AgentTaskOutcome> = failed_first.collect();

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

fn ranked_diagnostics(collected: Vec<CollectedDiagnostic>) -> Vec<CollectedDiagnostic> {
    // Dedupe by (class, message) keeping the first occurrence, then order the
    // most actionable root-cause diagnostics first.
    let mut deduped: Vec<CollectedDiagnostic> = Vec::new();
    for item in collected {
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

fn merge_diagnostic_data(existing: &mut Value, incoming: Value) {
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

fn diagnostic_data_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |value| value.len())
}

#[derive(Clone)]
struct CollectedDiagnostic {
    task_id: String,
    class: String,
    message: String,
    source: String,
    data: Value,
}

/// Lower number = higher priority. Explicit provider-contract failures must
/// precede an otherwise successful provider-process observation: the latter
/// describes the wrapper, not why the task failed.
fn diagnostic_priority(item: &CollectedDiagnostic) -> (u8, u8) {
    let class = item.class.to_ascii_lowercase();
    let text = format!("{} {}", item.class, item.message).to_ascii_lowercase();
    let priority = if item.source == "current_lifecycle" {
        0
    } else if is_policy_denial(&class, &text) {
        0
    } else if is_required_output_diagnostic(&class) {
        1
    } else if is_provider_structured_error(&class) {
        // The provider's own terminal error event, already normalized by the
        // provider adapter: the most specific execution-layer cause there is.
        1
    } else if is_provider_contract_diagnostic(&class) {
        2
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
    (priority, u8::from(item.source != "hydrated_process_stream"))
}

/// Process streams are untrusted wrapper output. When a stream is JSON, its
/// diagnostics are the execution-layer cause and therefore rank ahead of the
/// wrapper's outcome-normalization consequence. A stream carrying a
/// normalized structured provider error (already converted by the provider
/// adapter) is promoted with its message, status code, retryability, and
/// account/quota classification instead of collapsing to an exit code
/// (#13703).
fn collect_process_stream_diagnostics(
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
fn structured_error_diagnostic(task_id: &str, error: &Value) -> CollectedDiagnostic {
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
        "evidence_refs": compact_items(outcome.get("evidence_refs"), &["schema", "kind", "uri", "created_at", "timestamp"]),
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
        summary["evidence_command"] = json!(format!("homeboy agent-task evidence {run_id}"));
    }
    enforce_compact_status_budget(summary)
}

fn enforce_compact_status_budget(mut value: Value) -> Value {
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
    // Ordered from supplementary evidence to the broad status tables. Core IDs,
    // state, and the full-output command always survive this final pass.
    for section in [
        "notification_delivery",
        "notification_resolution",
        "transport_recovery",
        "diagnostic_summary",
        "failure_reasons",
        "execution_states",
        "candidate_adoption",
        "cook_finalization",
        "latest_promotion",
        "moving_base_recovery",
        "failure_context",
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

fn compact_mandatory_scalars(object: &mut serde_json::Map<String, Value>) -> Vec<Value> {
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

fn compact_mandatory_field(field: &str) -> bool {
    matches!(
        field,
        "schema"
            | "view"
            | "run_id"
            | "cook_id"
            | "latest_run_id"
            | "status"
            | "state"
            | "status_scope"
            | "lab_transport_failure"
            | "full_command"
    )
}

fn compact_budget_value(
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

fn compact_section_count(value: &Value) -> usize {
    value.as_array().map_or(1, Vec::len)
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
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
    if full {
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
        "selected_candidate": value.get("selected_candidate").map(|candidate| compact_fields(candidate, &["latest_attempt_run_id", "run_id", "attempt", "invocation_scoped", "selected_task_id", "selected_artifact_id", "reason", "incomplete", "skipped_newer_attempts", "applied_promotion"])),
    });
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
    // The promotion report this carries is full evidence, so keep only the
    // blocker and the continuation the caller is expected to act on.
    if let Some(moving_base_recovery) = value.get("moving_base_recovery") {
        summary["moving_base_recovery"] = compact_fields(
            moving_base_recovery,
            &[
                "run_id",
                "prior_verified_base",
                "blocker",
                "continuation",
                "base_movements",
            ],
        );
    }
    if let Some(run_id) = latest_run_id {
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id}"));
        summary["evidence_command"] = json!(format!("homeboy agent-task evidence {run_id}"));
    }
    for field in ["provider", "remaining_phases", "continuation_command"] {
        if let Some(value) = value.get(field) {
            summary[field] = bounded_value(value);
        }
    }
    enforce_compact_status_budget(summary)
}

fn compact_cook_diagnostic(diagnostic: &Value) -> Value {
    let cause = diagnostic
        .get("deepest_cause")
        .filter(|value| value.is_object())
        .unwrap_or(diagnostic);
    json!({
        "code": cause.get("code"),
        "field": cause.get("field").or_else(|| diagnostic.pointer("/details/field")),
        "message": bounded_value(cause.get("message").unwrap_or(&Value::Null)),
    })
}

fn persisted_cook_failure_diagnostic(record: &AgentTaskRunRecord) -> Option<CollectedDiagnostic> {
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
                .unwrap_or("local Cook observer was interrupted during provider execution")
                .to_string(),
            source: "interrupted_owner".to_string(),
            data: failure.clone(),
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
                }),
            });
        }
        let provider_failure =
            homeboy::core::worktree_provider::compact_worktree_provider_failure_details(details);
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
                "worktree_provider_failure": provider_failure,
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

fn current_lifecycle_diagnostic(record: &AgentTaskRunRecord) -> Option<CollectedDiagnostic> {
    let promotion = record.metadata.get("latest_promotion")?;
    let promotion_status = promotion.get("status").and_then(Value::as_str);
    if matches!(
        promotion_status,
        Some("gate_failed" | "no_changes_gate_failed")
    ) {
        let gate = promotion
            .get("deterministic_gates")
            .or_else(|| promotion.get("gate_results"))
            .and_then(Value::as_array)
            .and_then(|gates| {
                gates.iter().find(|gate| {
                    matches!(
                        gate.get("status").and_then(Value::as_str),
                        Some("failed" | "failure")
                    )
                })
            });
        let gate_name = gate
            .and_then(|gate| gate.get("name").or_else(|| gate.get("command")))
            .and_then(Value::as_str)
            .unwrap_or("deterministic gate");
        return Some(CollectedDiagnostic {
            task_id: "promotion".to_string(),
            class: "agent_task.promotion_gate_failed".to_string(),
            message: gate
                .and_then(|gate| gate.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .unwrap_or_else(|| format!("Deterministic promotion gate failed: {gate_name}")),
            source: "current_lifecycle".to_string(),
            data: promotion.clone(),
        });
    }
    let finalization = record.metadata.get("cook_finalization")?;
    if !matches!(
        finalization.get("status").and_then(Value::as_str),
        Some("failed" | "finalization_failed")
    ) {
        return None;
    }
    Some(CollectedDiagnostic {
        task_id: "finalization".to_string(),
        class: finalization
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("agent_task.finalization_failed")
            .to_string(),
        message: finalization
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Cook finalization failed after promotion.")
            .to_string(),
        source: "current_lifecycle".to_string(),
        data: finalization.clone(),
    })
}

fn current_lifecycle_status(record: &AgentTaskRunRecord) -> Option<&'static str> {
    match record
        .metadata
        .pointer("/latest_promotion/status")
        .and_then(Value::as_str)
    {
        Some("gate_failed") => Some("gate_failed"),
        Some("no_changes_gate_failed" | "no_op_gate_failed") => Some("no_op_gate_failed"),
        _ if matches!(
            record
                .metadata
                .pointer("/cook_finalization/status")
                .and_then(Value::as_str),
            Some("failed" | "finalization_failed")
        ) =>
        {
            Some("finalization_failed")
        }
        _ => None,
    }
}

/// Cancellation is controller-owned lifecycle evidence, even when the runner
/// never produced an aggregate. Preserve its typed reason rather than treating
/// a pre-provider cancellation as an unclassified provider failure.
fn runner_cancellation_diagnostic(record: &AgentTaskRunRecord) -> Option<CollectedDiagnostic> {
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
            "provider_executions_consumed": record.metadata.get("provider_executions_consumed"),
            "runner_execution_status": record.metadata.pointer("/runner_execution_record/status"),
        }),
    })
}

fn compact_items(value: Option<&Value>, fields: &[&str]) -> Value {
    Value::Array(
        value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(COMPACT_REF_LIMIT)
                    .map(|item| compact_fields(item, fields))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn compact_fields(value: &Value, fields: &[&str]) -> Value {
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
fn compact_action_samples(actions: &[Value]) -> (Value, usize) {
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

fn bounded_failure_reasons(value: &Value) -> Value {
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

fn bounded_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.len() > COMPACT_TEXT_LIMIT => Value::Null,
        _ => value.clone(),
    }
}

/// Terminality of an untyped durable record, delegating to the single
/// definition in `AgentTaskRunState::is_terminal`.
///
/// `status` reads records as untyped JSON, so the state has to be parsed back
/// into the lifecycle enum instead of compared against an inline string list.
/// An inline list is exactly how `partial_failure`, `partial_recoverable`, and
/// `candidate_recoverable` runs came to be reported as `liveness.status:
/// "active"` forever — the list named only `succeeded`/`failed`/`cancelled`, so
/// an orchestrator polling liveness never saw those runs finish.
///
/// An absent or unparseable state is non-terminal: a record this build does not
/// understand keeps its prior reading rather than being asserted as finished.
fn candidate_result_payload(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Value {
    let mut payload = record.clone();
    if let Some(aggregate) = aggregate {
        payload["aggregate"] = serde_json::to_value(aggregate).unwrap_or(Value::Null);
    }
    payload
}

fn collected_diagnostic_value(item: CollectedDiagnostic) -> Value {
    collected_diagnostic_value_with_details(item, false)
}

fn collected_diagnostic_value_with_details(
    item: CollectedDiagnostic,
    include_details: bool,
) -> Value {
    let owner = diagnostic_owner(&item.class, &item.source, &item.data);
    let mut value = json!({
        "task_id": item.task_id,
        "class": item.class,
        "message": bounded_diagnostic_value(&Value::String(item.message)).unwrap_or(Value::Null),
        "source": item.source,
        "owner": owner,
    });
    if include_details && !item.data.is_null() {
        if let Some(details) = bounded_diagnostic_value(&item.data) {
            value["details"] = details;
        }
    } else if let Some(details) = policy_denial_details(&item.data) {
        value["details"] = details;
    } else if let Some(details) = structured_error_details(&item.data) {
        value["details"] = details;
    } else if item.source == "pre_execution_failure" {
        value["details"] = bounded_diagnostic_value(&item.data).unwrap_or(Value::Null);
    } else if item.source == "lab_preacceptance_transport" {
        value["details"] = bounded_diagnostic_value(&item.data).unwrap_or(Value::Null);
    } else if let Some(details) = item.data.get("worktree_provider_failure") {
        value["details"] = details.clone();
    }
    if let Some(field) = item.data.get("field").filter(|field| !field.is_null()) {
        if let Some(field) = bounded_diagnostic_value(field) {
            value["field"] = field;
        }
    }
    value
}

fn is_successful_process_exit(text: &str) -> bool {
    text.contains("exit status 0")
        || text.contains("exited with status 0")
        || text.contains("exit_code: 0")
        || text.contains("exit_code=0")
}

fn is_policy_denial(class: &str, text: &str) -> bool {
    class.contains("policy_denied")
        || class.contains("command_denied")
        || text.contains("policy denied")
        || text.contains("tool denied")
}

fn is_required_output_diagnostic(class: &str) -> bool {
    class.contains("required_output_missing")
}

fn is_provider_structured_error(class: &str) -> bool {
    class.contains("provider.structured_error")
}

fn is_provider_contract_diagnostic(class: &str) -> bool {
    class.contains("provider_outcome_contract_violation")
        || class.contains("outcome_contract_violation")
}

/// Preserve only the stable, actionable denial fields. Provider diagnostic data
/// may contain arbitrary command output, which does not belong in a root cause.
fn policy_denial_details(data: &Value) -> Option<Value> {
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
fn structured_error_details(data: &Value) -> Option<Value> {
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

fn structured_details(data: &Value, fields: &[&str]) -> Option<Value> {
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

fn bounded_diagnostic_value(value: &Value) -> Option<Value> {
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

fn diagnostic_owner(class: &str, source: &str, data: &Value) -> &'static str {
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

fn controller_runtime_recovery_available(record: &AgentTaskRunRecord) -> bool {
    let runtime = record
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY);
    runtime
        .and_then(|runtime| runtime.pointer("/originating/pinned_executable"))
        .and_then(Value::as_str)
        .is_some_and(|path| !path.trim().is_empty())
        && runtime
            .and_then(|runtime| runtime.pointer("/originating/sha256"))
            .and_then(Value::as_str)
            .is_some_and(|digest| !digest.trim().is_empty())
}

fn missing_artifact_summaries(aggregate: &AgentTaskAggregate) -> Vec<Value> {
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

fn causal_chain_from_aggregate(aggregate: &AgentTaskAggregate) -> Vec<Value> {
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

struct RetryReplayAction {
    owner: Value,
    readiness: &'static str,
    reason: Option<String>,
    admission: Value,
    action: Option<CommandNextAction>,
    continuation: Option<CommandNextAction>,
}

impl RetryReplayAction {
    fn unavailable(owner: Value, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            owner,
            readiness: "unavailable",
            admission: json!({ "admitted": false, "reason": reason }),
            reason: Some(reason),
            action: None,
            continuation: None,
        }
    }

    fn projection(&self) -> Value {
        json!({
            "owner": self.owner,
            "readiness": self.readiness,
            "reason": self.reason,
            "admission": self.admission,
            "action": self.action.as_ref().and_then(|action| action.action.clone()),
        })
    }
}

/// Render retry only from the durable owner and replay contract. The command and
/// typed action share this one admission decision, so a human hint cannot claim
/// a retry exists when route materialization will reject it.
fn retry_replay_action(record: &AgentTaskRunRecord) -> RetryReplayAction {
    let runner_id = record.runner_id().filter(|id| !id.trim().is_empty());
    let local_owner = match runner_id {
        Some(runner_id) => json!({ "placement": "runner", "runner_id": runner_id }),
        None => json!({ "placement": "local" }),
    };
    let plan = match agent_task_lifecycle::load_plan(&record.run_id) {
        Ok(plan) => plan,
        Err(error) => {
            return RetryReplayAction::unavailable(
                local_owner,
                format!("persisted replay plan is unavailable: {}", error.message),
            );
        }
    };
    match cook_continuation_action(record) {
        Ok(true) => {
            return RetryReplayAction {
                owner: local_owner,
                readiness: "unavailable",
                reason: Some(
                    "the authenticated review-form continuation must resume through Cook"
                        .to_string(),
                ),
                admission: json!({
                    "admitted": false,
                    "reason": "the authenticated review-form continuation must resume through Cook",
                }),
                action: None,
                continuation: Some(cook_continuation_command(&record.run_id)),
            };
        }
        Ok(false) => {}
        Err(error) => {
            return RetryReplayAction::unavailable(
                local_owner,
                format!(
                    "cannot validate whether this run requires Cook continuation: {}",
                    error.message
                ),
            );
        }
    }
    let has_generic_lab_replay = plan.metadata.get("generic_lab_command_replay").is_some();
    // Cook-owned retries stay on the controller lifecycle. Lab routing makes
    // the same Cook-first decision before it considers generic replay.
    if record.metadata["cook_id"].is_string() && has_generic_lab_replay {
        return RetryReplayAction::unavailable(
            local_owner,
            "Cook-owned generic Lab replay must continue through the controller Cook lifecycle",
        );
    }
    let generic_lab_replay = has_generic_lab_replay;
    let admission = if generic_lab_replay {
        // Status advertises the exact persisted generic replay, rather than a
        // Cook-derived retry plan that may no longer carry its workspace proof.
        crate::commands::infra::route::validate_generic_lab_command_replay_workspace(&plan)
            .and_then(|()| {
                agent_task_service_direct::retry_admission_with_preflight(
                    &record.run_id,
                    |_| Ok(()),
                )
            })
    } else {
        agent_task_service_direct::retry_admission(&record.run_id)
    };
    if let Err(error) = admission {
        return RetryReplayAction::unavailable(
            local_owner,
            format!("retry admission is unavailable: {}", error.message),
        );
    }
    if !plan_has_retry_materialization_identity(&plan) {
        return RetryReplayAction::unavailable(
            local_owner,
            "persisted replay plan has no materialization identity",
        );
    }
    let (owner, action) = if generic_lab_replay {
        let owner = json!({ "placement": "lab" });
        let action = lab_replay_retry_action(&record.run_id, owner.clone());
        (owner, action)
    } else {
        let owner = local_owner;
        let action = owner_bound_retry_action(&record.run_id, runner_id, owner.clone());
        (owner, action)
    };
    RetryReplayAction {
        owner,
        readiness: "ready",
        reason: None,
        admission: json!({ "admitted": true }),
        action: Some(action),
        continuation: None,
    }
}

fn lab_replay_retry_action(run_id: &str, owner: Value) -> CommandNextAction {
    let args = vec![
        "--placement".to_string(),
        "lab".to_string(),
        "agent-task".to_string(),
        "retry".to_string(),
        run_id.to_string(),
        "--run".to_string(),
    ];
    CommandNextAction::from_action(
        ExecutableAction::new(
            "agent-task.retry.lab-replay.v1",
            "retry the Lab replay from its persisted workspace",
            "homeboy",
            args,
            ActionSafety::Mutating,
        )
        .with_evidence(json!({
            "schema": "homeboy/agent-task-retry-replay/v1",
            "owner": owner,
            "replay_ready": true,
            "materialization_identity": true,
        })),
    )
}

/// A promoted review-form follow-up retains the candidate through Cook's
/// authenticated continuation route. Generic retry replays its obsolete clean
/// workspace attestation and is therefore not a legal recovery action.
fn cook_continuation_action(record: &AgentTaskRunRecord) -> homeboy::core::Result<bool> {
    let Some(recipe) = agent_task_service_direct::load_recipe_for_attempt(&record.run_id)? else {
        return Ok(false);
    };
    agent_task_service_direct::validate_recipe_attempt_record(&recipe, &record.run_id, record)?;
    let Some(attempt) = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == record.run_id)
    else {
        return Ok(false);
    };
    agent_task_service_direct::terminal_review_form_continuation_is_eligible(&attempt.plan, record)
}

fn cook_continuation_command(run_id: &str) -> CommandNextAction {
    CommandNextAction::new(
        "resume the authenticated Cook continuation",
        agent_task_service_direct::cook_continue_command(None, run_id, false, None),
    )
    .with_kind(CommandNextActionKind::Repair)
}

fn owner_bound_retry_action(
    run_id: &str,
    runner_id: Option<&str>,
    owner: Value,
) -> CommandNextAction {
    let mut args = Vec::new();
    if let Some(runner_id) = runner_id {
        args.extend(["--runner".to_string(), runner_id.to_string()]);
    } else {
        args.extend(["--placement".to_string(), "local".to_string()]);
    }
    args.extend([
        "agent-task".to_string(),
        "retry".to_string(),
        run_id.to_string(),
        "--run".to_string(),
    ]);
    let action = ExecutableAction::new(
        "agent-task.retry.owner-bound.v1",
        "retry the run from its persisted plan",
        "homeboy",
        args,
        ActionSafety::Mutating,
    )
    .with_evidence(json!({
        "schema": "homeboy/agent-task-retry-replay/v1",
        "owner": owner,
        "replay_ready": true,
        "materialization_identity": true,
    }));
    CommandNextAction::from_action(action)
}

fn plan_has_retry_materialization_identity(plan: &AgentTaskPlan) -> bool {
    plan.tasks.iter().any(|task| {
        task.workspace
            .root
            .as_deref()
            .or_else(|| {
                task.executor
                    .config
                    .get("workspace_root")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                task.metadata
                    .get("workspace")
                    .and_then(|workspace| workspace.get("root"))
                    .and_then(Value::as_str)
            })
            .is_some_and(|root| !root.trim().is_empty())
    }) || plan
        .metadata
        .get("generic_lab_command_replay")
        .is_some_and(|replay| {
            replay.get("schema").and_then(Value::as_str)
                == Some("homeboy/generic-lab-command-replay/v1")
                && replay
                    .get("normalized_args")
                    .and_then(Value::as_array)
                    .is_some_and(|args| !args.is_empty())
                && replay
                    .pointer("/materialization/canonical_root")
                    .and_then(Value::as_str)
                    .is_some_and(|root| !root.trim().is_empty())
                && replay
                    .pointer("/materialization/content_identity")
                    .and_then(Value::as_str)
                    .is_some_and(|identity| !identity.trim().is_empty())
        })
}

fn diagnose_next_commands(
    record: &AgentTaskRunRecord,
    retry_action: Option<&CommandNextAction>,
    continuation_action: Option<&CommandNextAction>,
    current_lifecycle_denial: bool,
) -> Vec<String> {
    let owner = record
        .runner_id()
        .map(|runner| format!("--runner {runner}"))
        .unwrap_or_else(|| "--placement local".to_string());
    let run_id = &record.run_id;
    if current_lifecycle_denial {
        return current_lifecycle_next_actions(record)
            .into_iter()
            .map(|action| action.command)
            .collect();
    }
    let mut commands = vec![
        format!("homeboy {owner} agent-task diagnose {run_id} --full"),
        format!("homeboy {owner} agent-task artifacts {run_id}"),
        format!("homeboy {owner} agent-task review {run_id}"),
    ];
    commands.extend(
        continuation_action
            .or(retry_action)
            .map(|action| action.command.clone()),
    );
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::Error;

    #[test]
    fn stale_generic_lab_replay_status_has_no_executable_action() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::write(workspace.path().join("workspace.txt"), "recorded")
                .expect("write workspace");
            let recorded_identity =
                homeboy::runner::generic_lab_replay_artifact_identity(workspace.path())
                    .expect("record replay artifact identity");
            std::fs::write(workspace.path().join("workspace.txt"), "current")
                .expect("change workspace");
            let mut plan = AgentTaskPlan::new("stale-generic-lab-replay", Vec::new());
            plan.metadata["generic_lab_command_replay"] = json!({
                "schema": "homeboy/generic-lab-command-replay/v1",
                "normalized_args": ["homeboy", "bench"],
                "materialization": {
                    "canonical_root": workspace.path(),
                    "content_identity": recorded_identity,
                },
            });
            agent_task_lifecycle::submit_plan(&plan, Some("stale-generic-lab-replay"))
                .expect("persist generic replay");
            agent_task_lifecycle::record_pre_execution_failure(
                "stale-generic-lab-replay",
                &plan,
                "lab_daemon_admission",
                &Error::internal_unexpected("daemon unavailable").with_retryable(true),
            )
            .expect("record retryable failure");

            let record = agent_task_lifecycle::reconcile_status("stale-generic-lab-replay")
                .expect("load replay record");
            let retry = retry_replay_action(&record);

            assert_eq!(retry.readiness, "ready");
            assert!(retry.action.is_some());
        });
    }

    #[test]
    fn cook_owned_generic_lab_replay_status_has_no_lab_action() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");
            let mut plan = AgentTaskPlan::new("cook-owned-generic-lab-replay", Vec::new());
            plan.metadata["generic_lab_command_replay"] = json!({
                "schema": "homeboy/generic-lab-command-replay/v1",
                "normalized_args": ["homeboy", "bench"],
                "materialization": {
                    "canonical_root": workspace.path(),
                    "content_identity": "snapshot:recorded",
                },
            });
            agent_task_lifecycle::submit_plan(&plan, Some("cook-owned-generic-lab-replay"))
                .expect("persist generic replay");
            agent_task_lifecycle::rewrite_record_for_test(
                "cook-owned-generic-lab-replay",
                |record| {
                    record.metadata["cook_id"] = json!("cook-owned-generic-lab-replay");
                },
            )
            .expect("mark replay Cook-owned");
            agent_task_lifecycle::record_pre_execution_failure(
                "cook-owned-generic-lab-replay",
                &plan,
                "lab_daemon_admission",
                &Error::internal_unexpected("daemon unavailable").with_retryable(true),
            )
            .expect("record retryable failure");

            let record = agent_task_lifecycle::reconcile_status("cook-owned-generic-lab-replay")
                .expect("load replay record");
            let retry = retry_replay_action(&record);

            assert_eq!(retry.readiness, "unavailable");
            assert!(retry.action.is_none());
            assert!(retry
                .reason
                .unwrap()
                .contains("Cook-owned generic Lab replay"));
        });
    }

    #[test]
    fn placement_rewrite_threads_one_resolved_prefix_through_nested_commands() {
        let mut value = json!({
            "command": "homeboy agent-task status cook-1",
            "nested": ["homeboy agent-task finalize-pr --recover cook-1"],
        });

        preserve_controller_owner_placement_with_prefix(
            &mut value,
            "cook-1",
            "homeboy --placement local",
        );

        assert_eq!(
            value["command"],
            "homeboy --placement local agent-task status cook-1"
        );
        assert_eq!(
            value["nested"][0],
            "homeboy --placement local agent-task finalize-pr --recover cook-1"
        );
    }

    #[test]
    fn actionable_diagnostics_outrank_a_successful_provider_process_exit() {
        let diagnostics = ranked_diagnostics(vec![
            CollectedDiagnostic {
                task_id: "cook".to_string(),
                class: "provider.process_exit".to_string(),
                message: "OpenCode CLI exited with status 0".to_string(),
                source: "diagnostics".to_string(),
                data: Value::Null,
            },
            CollectedDiagnostic {
                task_id: "cook".to_string(),
                class: "agent_task.provider_outcome_contract_violation".to_string(),
                message: "Provider result violates the required output contract.".to_string(),
                source: "diagnostics".to_string(),
                data: Value::Null,
            },
            CollectedDiagnostic {
                task_id: "cook".to_string(),
                class: "agent_task.required_output_missing".to_string(),
                message: "Required output review_form was not produced.".to_string(),
                source: "diagnostics".to_string(),
                data: Value::Null,
            },
            CollectedDiagnostic {
                task_id: "cook".to_string(),
                class: "agent_tool.command_denied".to_string(),
                message: "Tool 'grep' was denied by the external-directory permission policy."
                    .to_string(),
                source: "diagnostics".to_string(),
                data: json!({
                    "tool": "grep",
                    "permission": "external_directory_read",
                    "requested_path": "/Users/chubes/Developer/homeboy",
                    "canonical_path": "/Users/chubes/Developer/homeboy@fix-11827"
                }),
            },
        ]);

        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.class.as_str())
                .collect::<Vec<_>>(),
            vec![
                "agent_tool.command_denied",
                "agent_task.required_output_missing",
                "agent_task.provider_outcome_contract_violation",
                "provider.process_exit",
            ]
        );
        assert_eq!(
            collected_diagnostic_value(diagnostics[0].clone())["details"]["canonical_path"],
            "/Users/chubes/Developer/homeboy@fix-11827"
        );
    }

    #[test]
    fn duplicate_diagnostics_merge_richer_denial_data() {
        let diagnostics = ranked_diagnostics(vec![
            CollectedDiagnostic {
                task_id: "cook".to_string(),
                class: "agent_tool.command_denied".to_string(),
                message: "grep was denied".to_string(),
                source: "diagnostics".to_string(),
                data: json!({ "tool": "grep" }),
            },
            CollectedDiagnostic {
                task_id: "cook".to_string(),
                class: "agent_tool.command_denied".to_string(),
                message: "grep was denied".to_string(),
                source: "diagnostics".to_string(),
                data: json!({
                    "permission": "external_directory_read",
                    "canonical_path": "/worktree/homeboy@fix-11827"
                }),
            },
        ]);

        assert_eq!(diagnostics.len(), 1);
        let value = collected_diagnostic_value(diagnostics[0].clone());
        assert_eq!(value["details"]["tool"], "grep");
        assert_eq!(value["details"]["permission"], "external_directory_read");
        assert_eq!(
            value["details"]["canonical_path"],
            "/worktree/homeboy@fix-11827"
        );
    }

    #[test]
    fn duplicate_diagnostics_from_different_tasks_remain_distinct() {
        let diagnostics = ranked_diagnostics(vec![
            CollectedDiagnostic {
                task_id: "task-a".to_string(),
                class: "agent_tool.command_denied".to_string(),
                message: "grep was denied".to_string(),
                source: "diagnostics".to_string(),
                data: json!({ "tool": "grep" }),
            },
            CollectedDiagnostic {
                task_id: "task-b".to_string(),
                class: "agent_tool.command_denied".to_string(),
                message: "grep was denied".to_string(),
                source: "diagnostics".to_string(),
                data: json!({ "canonical_path": "/other-worktree" }),
            },
        ]);

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].task_id, "task-a");
        assert_eq!(diagnostics[1].task_id, "task-b");
    }

    #[test]
    fn evidence_diagnostic_collection_stops_at_the_global_ref_budget() {
        let directory = tempfile::tempdir().expect("evidence directory");
        let mut diagnostics = Vec::new();
        let mut budget = DiagnosticCollectionBudget::default();
        let mut truncations = Vec::new();

        for index in 0..=DIAGNOSTIC_EVIDENCE_REF_LIMIT {
            let path = directory.path().join(format!("evidence-{index}.json"));
            std::fs::write(
                &path,
                json!({ "diagnostics": [{ "class": "provider", "message": format!("failure-{index}") }] }).to_string(),
            )
            .expect("write evidence");
            let evidence = AgentTaskEvidenceRef {
                kind: "executor-result".to_string(),
                uri: format!("file://{}", path.display()),
                label: None,
            };
            if let Some(truncation) = collect_hydrated_evidence_diagnostics(
                "cook",
                &evidence,
                &mut diagnostics,
                &mut budget,
            ) {
                truncations.push(truncation);
            }
        }

        assert_eq!(diagnostics.len(), DIAGNOSTIC_EVIDENCE_REF_LIMIT);
        assert_eq!(truncations.len(), 1);
        assert_eq!(truncations[0]["truncation"]["reason"], "evidence_refs");
    }

    #[test]
    fn policy_denial_details_are_bounded_for_default_status() {
        let value = collected_diagnostic_value(CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "agent_tool.command_denied".to_string(),
            message: "denied".to_string(),
            source: "diagnostics".to_string(),
            data: json!({ "canonical_path": "x".repeat(COMPACT_TEXT_LIMIT + 1) }),
        });

        assert!(
            value["details"].get("canonical_path").is_none(),
            "oversized diagnostic text is omitted atomically"
        );
    }

    #[test]
    fn retry_action_is_typed_and_bound_to_the_controller() {
        let action = owner_bound_retry_action("run-1", None, json!({ "placement": "local" }));

        assert_eq!(
            action.command,
            "homeboy --placement local agent-task retry run-1 --run"
        );
        assert_eq!(
            action.action.as_ref().unwrap().id,
            "agent-task.retry.owner-bound.v1"
        );
        assert_eq!(
            action.action.as_ref().unwrap().evidence.as_ref().unwrap()["owner"]["placement"],
            "local"
        );
    }

    #[test]
    fn generic_lab_replay_status_advertises_only_the_lab_retry_action() {
        let action = lab_replay_retry_action("run-1", json!({ "placement": "lab" }));

        assert_eq!(
            action.command,
            "homeboy --placement lab agent-task retry run-1 --run"
        );
        assert_eq!(
            action.action.as_ref().unwrap().id,
            "agent-task.retry.lab-replay.v1"
        );
        assert_eq!(
            action.action.as_ref().unwrap().evidence.as_ref().unwrap()["owner"]["placement"],
            "lab"
        );
    }

    #[test]
    fn diagnose_commands_preserve_the_controller_owner_placement() {
        let record: AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": "controller-run",
            "plan_id": "plan",
            "state": "failed",
            "submitted_at": "2026-08-05T00:00:00Z",
            "plan_path": "plan.json",
            "metadata": {}
        }))
        .expect("minimal durable record");

        let commands = diagnose_next_commands(&record, None, None, false);

        assert!(commands
            .iter()
            .all(|command| command.starts_with("homeboy --placement local agent-task")));
    }

    #[test]
    fn retry_action_is_typed_and_bound_to_its_runner() {
        let action = owner_bound_retry_action(
            "run-1",
            Some("lab-a"),
            json!({ "placement": "runner", "runner_id": "lab-a" }),
        );

        assert_eq!(
            action.command,
            "homeboy --runner lab-a agent-task retry run-1 --run"
        );
        assert_eq!(
            action.action.as_ref().unwrap().evidence.as_ref().unwrap()["owner"]["runner_id"],
            "lab-a"
        );
    }

    #[test]
    fn unavailable_retry_has_a_reason_and_no_executable_action() {
        let retry = RetryReplayAction::unavailable(
            json!({ "placement": "runner", "runner_id": "lab-a" }),
            "source workspace no longer matches the persisted Lab replay identity",
        );

        assert_eq!(retry.projection()["readiness"], "unavailable");
        assert!(retry.projection()["action"].is_null());
        assert!(retry.projection()["reason"]
            .as_str()
            .unwrap()
            .contains("no longer matches"));
        assert_eq!(retry.projection()["admission"]["admitted"], false);
    }

    #[test]
    fn cook_continuation_replaces_generic_retry_in_diagnose_commands() {
        let record: AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": "review-form-timeout",
            "plan_id": "plan",
            "state": "partial_failure",
            "submitted_at": "2026-08-08T00:00:00Z",
            "plan_path": "plan.json",
            "metadata": {}
        }))
        .expect("minimal durable record");
        let retry = RetryReplayAction {
            owner: json!({ "placement": "local" }),
            readiness: "unavailable",
            reason: Some(
                "the authenticated review-form continuation must resume through Cook".to_string(),
            ),
            admission: json!({ "admitted": false }),
            action: None,
            continuation: Some(
                CommandNextAction::new(
                    "resume the authenticated Cook continuation",
                    "homeboy agent-task cook-continue review-form-timeout",
                )
                .with_kind(CommandNextActionKind::Repair),
            ),
        };

        let commands = diagnose_next_commands(
            &record,
            retry.action.as_ref(),
            retry.continuation.as_ref(),
            false,
        );

        assert_eq!(retry.projection()["readiness"], "unavailable");
        assert!(retry.projection()["action"].is_null());
        assert!(
            commands.contains(&"homeboy agent-task cook-continue review-form-timeout".to_string())
        );
        assert!(commands
            .iter()
            .all(|command| !command.contains("agent-task retry")));
    }

    #[test]
    fn compact_cook_report_is_bounded_and_full_remains_retrievable() {
        let attempts = (0..(COMPACT_TASK_LIMIT + 3))
            .map(|attempt| {
                json!({
                    "attempt": attempt,
                    "run_id": format!("run-{attempt}"),
                    "run_state": "failed",
                    "promotion": { "nested_evidence": "x".repeat(COMPACT_TEXT_LIMIT + 1) }
                })
            })
            .collect::<Vec<_>>();
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-1",
            "latest_run_id": "run-14",
            "status": "failed",
            "stop_reason": "x".repeat(COMPACT_TEXT_LIMIT + 1),
            "attempts": attempts,
            "finalization": { "status": "blocked", "nested_evidence": { "large": "x".repeat(COMPACT_TEXT_LIMIT + 1) } },
            "selected_candidate": { "latest_attempt_run_id": "run-14", "run_id": "run-12", "selected_task_id": "task-12", "selected_artifact_id": "patch-12", "reason": "canonical", "skipped_newer_attempts": [{"run_id":"run-14","reason":"malformed"}] }
        });

        let compact = compact_cook_report(report.clone(), false);
        assert_eq!(compact["schema"], report["schema"]);
        assert_eq!(compact["cook_id"], "cook-1");
        assert_eq!(
            compact["attempts"].as_array().unwrap().len(),
            COMPACT_TASK_LIMIT
        );
        assert_eq!(compact["attempts_omitted"], 3);
        assert!(compact["attempts"][0].get("promotion").is_none());
        assert_eq!(compact["selected_candidate"]["run_id"], "run-12");
        assert_eq!(
            compact["selected_candidate"]["selected_artifact_id"],
            "patch-12"
        );
        assert_eq!(compact["full_command"], "homeboy agent-task status run-14");
        assert!(
            compact.get("failure_context").is_none(),
            "a report without failure_context must not grow one"
        );
        assert_eq!(compact_cook_report(report.clone(), true), report);
    }

    #[test]
    fn adopted_three_gate_finalization_projection_is_bounded_and_actionable_before_evidence() {
        let gate = json!({
            "name": "cargo test",
            "status": "passed",
            "evidence_refs": [{ "uri": "homeboy://evidence/three-gate-proof" }],
            "environment": { "PATH": "x".repeat(8 * 1024) },
            "proof": { "stdout": "x".repeat(8 * 1024) },
        });
        let report = json!({
            "schema": "homeboy/agent-task-finalization-report/v1",
            "run_id": "adopted-three-gate",
            "status": "review_ready",
            "pr_url": "https://github.com/Extra-Chill/homeboy/pull/422",
            "handoff": { "finalize_command": "homeboy agent-task finalize-pr --recover adopted-three-gate" },
            "gates": [gate.clone(), gate.clone(), gate],
            "promotion": { "gates": (0..100).map(|_| json!({ "proof": "x".repeat(1024) })).collect::<Vec<_>>() },
        });

        let projected = bounded_full_operation_report(report, "finalize-pr");
        let serialized = serde_json::to_vec(&projected).expect("projection serializes");
        let early =
            std::str::from_utf8(&serialized[..serialized.len().min(1024)]).expect("json is utf8");

        assert!(serialized.len() <= BOUNDED_FULL_STATUS_BYTE_LIMIT);
        assert_eq!(
            projected["schema"],
            "homeboy/agent-task-finalization-report/v1"
        );
        assert_eq!(projected["actionable"]["terminal_state"], "review_ready");
        assert_eq!(
            projected["actionable"]["pr_url"],
            "https://github.com/Extra-Chill/homeboy/pull/422"
        );
        assert_eq!(
            projected["actionable"]["next_action"]["command"],
            "homeboy agent-task finalize-pr --recover adopted-three-gate"
        );
        assert_eq!(projected["evidence_refs"].as_array().unwrap().len(), 1);
        assert_eq!(
            projected["evidence_refs"][0]["ref"],
            "homeboy://evidence/three-gate-proof"
        );
        assert!(early.contains("https://github.com/Extra-Chill/homeboy/pull/422"));
        assert!(early.contains("finalize-pr --recover adopted-three-gate"));

        let oversized = bounded_full_operation_report(
            json!({
                "schema": "x".repeat(BOUNDED_FULL_STATUS_BYTE_LIMIT * 2),
                "run_id": "r".repeat(BOUNDED_FULL_STATUS_BYTE_LIMIT * 2),
                "status": "failed",
                "handoff": { "finalize_command": "x".repeat(BOUNDED_FULL_STATUS_BYTE_LIMIT * 2) },
            }),
            "finalize-pr",
        );
        assert!(serialized_len(&oversized) <= BOUNDED_FULL_STATUS_BYTE_LIMIT);
        assert_eq!(oversized["actionable"]["terminal_state"], "failed");
        assert_eq!(oversized["schema"], Value::Null);
    }

    /// #11113: `cook_failure_context` already computes the runnable recovery
    /// commands, and the notification path forwards them. The default (compact)
    /// view discarded the whole thing, so the pull channel was strictly poorer
    /// than the push channel from the same computation.
    #[test]
    fn compact_cook_report_keeps_failure_context_actionable() {
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-11113",
            "latest_run_id": "run-2",
            "history_run_ids": ["run-1", "run-2"],
            "invocation_run_ids": ["run-2"],
            "status": "durable_failure",
            "attempts": [],
            "failure_context": {
                "cook_id": "cook-11113",
                "latest_run_id": "run-2",
                "selected_run_id": "run-1",
                "durable_recipe_ref": "homeboy://agent-task/cooks/cook-11113/recipe",
                "lifecycle_state": "Failed",
                "phase": "promotion",
                "reason_code": "operation_in_progress",
                "provider_budget_consumed": true,
                "provider_executions_consumed": 3,
                "recovery_legal": true,
                "recovery_reason": "y".repeat(COMPACT_TEXT_LIMIT + 1),
                "legal_actions": [
                    { "action": "status", "command": "homeboy agent-task status run-2" },
                    { "action": "diagnose", "command": "homeboy agent-task diagnose run-2" }
                ],
                "next_actions": [
                    { "action": "status", "command": "homeboy agent-task status run-2" }
                ],
                "diagnostic": {
                    "code": "promotion_rejected",
                    "evidence": "private gate output",
                    "deepest_cause": {
                        "code": "validation.invalid_argument",
                        "field": "promotion_provider.response",
                        "message": "provider rejected the request"
                    }
                },
                "blocking_claim": { "state": "Running", "evidence": "private claim payload" }
            }
        });

        let compact = compact_cook_report(report.clone(), false);
        let context = &compact["failure_context"];

        assert_eq!(context["phase"], "promotion");
        assert_eq!(context["reason_code"], "operation_in_progress");
        assert_eq!(context["recovery_legal"], true);
        assert_eq!(context["provider_executions_consumed"], 3);
        assert_eq!(
            context["legal_actions"][1]["command"],
            "homeboy agent-task diagnose run-2"
        );
        assert_eq!(
            context["next_actions"][0]["command"],
            "homeboy agent-task status run-2"
        );
        assert_eq!(compact["invocation_run_ids"], json!(["run-2"]));

        // Private evidence stays behind --full; compact retains only the typed cause.
        assert_eq!(
            context["diagnostic"],
            json!({
                "code": "validation.invalid_argument",
                "field": "promotion_provider.response",
                "message": "provider rejected the request"
            })
        );
        assert!(
            context.get("blocking_claim").is_none(),
            "blocking_claim carries claim payload evidence"
        );

        assert!(
            context.get("recovery_reason").is_none(),
            "oversized prose is omitted atomically instead of splitting Unicode text"
        );
        assert_eq!(
            context["omitted_scalars"][0],
            json!({ "field": "recovery_reason", "bytes": COMPACT_TEXT_LIMIT + 1 })
        );

        assert_eq!(compact_cook_report(report.clone(), true), report);
    }

    #[test]
    fn compact_cook_report_bounds_recovery_action_samples_and_keeps_full_report() {
        let mut actions = (0..COMPACT_ACTION_LIMIT)
            .map(|index| {
                json!({
                    "action": format!("recover-{index}"),
                    "command": format!("homeboy agent-task recover run-1 --step={index}"),
                    "private_evidence": "must only appear in --full"
                })
            })
            .collect::<Vec<_>>();
        actions.extend((0..20).map(|index| {
            json!({
                "action": format!("oversized-{index}"),
                "command": format!("homeboy agent-task recover run-1 --note={}", "x".repeat(COMPACT_ACTION_BYTE_LIMIT * 4)),
            })
        }));
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-action-budget",
            "latest_run_id": "run-1",
            "status": "durable_failure",
            "attempts": [],
            "failure_context": {
                "phase": "promotion",
                "reason_code": "operation_in_progress",
                "recovery_legal": true,
                "recovery_reason": "choose a recovery command",
                "legal_actions": actions,
                "next_actions": actions
            }
        });

        let compact = compact_cook_report(report.clone(), false);
        let context = &compact["failure_context"];

        for field in ["legal_actions", "next_actions"] {
            assert_eq!(
                context[field].as_array().unwrap().len(),
                COMPACT_ACTION_LIMIT
            );
            assert_eq!(
                context[format!("{field}_omitted")],
                json!(20),
                "the compact view must disclose every omitted recovery action"
            );
            assert!(context[field][0].get("private_evidence").is_none());
            assert_eq!(
                context[field][0]["command"], "homeboy agent-task recover run-1 --step=0",
                "sampled commands remain exactly runnable"
            );
        }
        assert_eq!(
            context["next_action"]["command"],
            "homeboy agent-task recover run-1 --step=0"
        );
        assert!(
            serde_json::to_vec(&compact).unwrap().len() < 6 * 1024,
            "large recovery lists must stay within the compact terminal-output budget"
        );
        assert_eq!(compact_cook_report(report.clone(), true), report);
    }

    #[test]
    fn compact_cook_report_omits_oversized_unicode_action_without_splitting_it() {
        let command = format!("homeboy agent-task retry run-1 --note={}", "👩‍💻".repeat(100));
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-unicode",
            "latest_run_id": "run-1",
            "status": "durable_failure",
            "attempts": [],
            "failure_context": {
                "legal_actions": [{ "action": "retry", "command": command }],
                "next_actions": [{ "action": "retry", "command": command }]
            }
        });

        let compact = compact_cook_report(report.clone(), false);
        let context = &compact["failure_context"];

        assert!(context["legal_actions"].as_array().unwrap().is_empty());
        assert!(context["next_actions"].as_array().unwrap().is_empty());
        assert_eq!(context["legal_actions_omitted"], 1);
        assert_eq!(context["next_actions_omitted"], 1);
        assert!(context.get("next_action").is_none());
        assert!(serde_json::to_vec(&compact).unwrap().len() < 2 * 1024);
        assert_eq!(compact_cook_report(report.clone(), true), report);
    }

    #[test]
    fn compact_cook_report_enforces_final_byte_budget_after_all_projections() {
        let large = "x".repeat(COMPACT_STATUS_BYTE_LIMIT);
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-budget",
            "latest_run_id": "run-budget",
            "status": "durable_failure",
            "stop_reason": large,
            "terminal_phase": "promotion",
            "attempts": (0..COMPACT_TASK_LIMIT).map(|index| json!({ "attempt": index, "run_id": format!("run-{index}"), "run_state": "failed", "aggregate_path": large })).collect::<Vec<_>>(),
            "finalization": { "status": "blocked", "evidence": large },
            "selected_candidate": { "run_id": "run-budget", "reason": large },
            "failure_context": {
                "phase": "promotion",
                "reason_code": "gate_failed",
                "recovery_reason": large,
                "legal_actions": [{ "action": "status", "command": "homeboy agent-task status run-budget" }]
            },
            "moving_base_recovery": { "blocker": large },
            "provider": { "evidence": large },
            "remaining_phases": [large],
            "continuation_command": large
        });

        let compact = compact_cook_report(report, false);
        let omitted = compact["omitted_sections"]
            .as_array()
            .expect("omission metadata");

        assert!(serialized_len(&compact) <= COMPACT_STATUS_BYTE_LIMIT);
        assert_eq!(compact["schema"], "homeboy/agent-task-cook/v1");
        assert_eq!(compact["cook_id"], "cook-budget");
        assert_eq!(compact["status"], "durable_failure");
        assert_eq!(
            compact["full_command"],
            "homeboy agent-task status run-budget"
        );
        assert!(omitted.iter().any(|item| item["section"] == "provider"));
    }

    /// The moving-base recovery carries a full promotion report. Compact keeps
    /// the blocker and the continuation, not the evidence.
    #[test]
    fn compact_cook_report_keeps_moving_base_continuation_without_promotion_evidence() {
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-11113-moving-base",
            "latest_run_id": "run-3",
            "status": "candidate_recoverable",
            "attempts": [],
            "moving_base_recovery": {
                "schema": "homeboy/agent-task-cook-moving-base-recovery/v1",
                "cook_id": "cook-11113-moving-base",
                "run_id": "run-3",
                "prior_verified_base": "abc123",
                "blocker": "base moved during promotion",
                "continuation": "homeboy agent-task cook-continue run-3",
                "base_movements": 2,
                "promotion": { "nested_evidence": "x".repeat(COMPACT_TEXT_LIMIT + 1) },
                "passed_gates": { "nested_evidence": "x".repeat(COMPACT_TEXT_LIMIT + 1) }
            }
        });

        let compact = compact_cook_report(report.clone(), false);
        let recovery = &compact["moving_base_recovery"];

        assert_eq!(recovery["blocker"], "base moved during promotion");
        assert_eq!(
            recovery["continuation"],
            "homeboy agent-task cook-continue run-3"
        );
        assert_eq!(recovery["base_movements"], 2);
        assert!(recovery.get("promotion").is_none());
        assert!(recovery.get("passed_gates").is_none());

        assert_eq!(compact_cook_report(report.clone(), true), report);
    }
}
