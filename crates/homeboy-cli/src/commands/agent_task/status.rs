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
use std::time::Instant;

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
use homeboy::runner::runners::{self as runner, RunnerKind};

use super::super::CmdResult;
use super::args::{
    CancelArgs, DiagnoseArgs, EvidenceArgs, LifecycleReadArgs, LogsArgs, QuarantineArgs, RearmArgs,
    ReplayProviderBoundaryArgs, RuntimeRecoverArgs, RuntimeValidateArgs, StatusArgs,
};
use super::candidate::{canonical_candidate_projection, classify_candidates, CandidateState};
use crate::commands::utils::response::{
    CommandActionableMetadata, CommandAgentTaskRef, CommandArtifactRef, CommandNextAction,
    CommandNextActionKind, CommandResultRefs, CommandRunRef, ACTIONABLE_METADATA_KEY,
};
use crate::commands::utils::watch::{
    parse_duration, watch_loop, WatchConfig, WatchConclusion, WatchPoller, WatchResult,
    TIMEOUT_EXIT_CODE,
};

/// Cap the number of detail refs rendered in the compact summary so a noisy
/// aggregate cannot flood recovery output. Overflow is reported as an
/// `omitted` count rather than dropped silently.
const COMPACT_REF_LIMIT: usize = 12;
const COMPACT_TASK_LIMIT: usize = 12;
const COMPACT_TEXT_LIMIT: usize = 512;
const STATUS_WATCH_CHANGE_LIMIT: usize = 12;

/// Cook IDs are logical candidate readers. Exact attempt IDs remain immutable
/// attempt readers, even when a newer Cook attempt produced no patch.
pub(super) struct CookReaderTarget {
    pub(super) run_id: String,
    pub(super) selection: Option<Value>,
    pub(super) cook_alias: Option<Value>,
    pub(super) exact: bool,
}

pub(super) fn resolve_cook_reader_target(
    run_or_cook_id: &str,
    exact: bool,
) -> homeboy::core::Result<CookReaderTarget> {
    if exact {
        let cook_alias = agent_task_lifecycle::cook_index_exists(run_or_cook_id)?
            .then(|| agent_task_lifecycle::cook_index(run_or_cook_id))
            .transpose()?
            .map(|index| {
                json!({
                    "cook_id": index.cook_id,
                    "latest_attempt_run_id": index.latest_run_id,
                })
            });
        return Ok(CookReaderTarget {
            run_id: run_or_cook_id.to_string(),
            selection: None,
            cook_alias,
            exact: true,
        });
    }
    if !agent_task_lifecycle::cook_index_exists(run_or_cook_id)? {
        return Ok(CookReaderTarget {
            run_id: run_or_cook_id.to_string(),
            selection: None,
            cook_alias: None,
            exact: false,
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
        cook_alias: Some(json!({
            "cook_id": selection.cook_id,
            "latest_attempt_run_id": selection.latest_attempt_run_id,
        })),
        selection: Some(serde_json::to_value(selection).unwrap_or(Value::Null)),
        exact: false,
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
    if args.bridge {
        // `--bridge` routes through `agent_task_lifecycle::status()`, which is a
        // reconciling read that WRITES: it refreshes accepted runner handoffs,
        // expires unbound controller handoffs, and persists the result. So this
        // answer is not merely a different view from `activity show <id>` — it
        // can also change what a later `activity show` returns. Say so (#W3-15).
        let bridge_status = agent_task_service::run_status(&target.run_id, args.since_cursor)?;
        let mut value = serde_json::to_value(bridge_status).unwrap_or(Value::Null);
        attach_reconciled(&mut value, true);
        if let Some(selection) = target.selection {
            value["candidate_selection"] = selection;
        }
        return Ok((value, 0));
    }

    let run_id = &target.run_id;
    // Terminal inspection is a durable-local read. Reconciliation has its own
    // explicit command so an unavailable runner cannot hold status hostage.
    let durable_read = match if target.exact {
        agent_task_lifecycle::exact_durable_local_read(run_id)
    } else {
        agent_task_lifecycle::durable_local_read(run_id)
    } {
        Ok(read) => read,
        Err(error) if is_missing_agent_task_run_metadata_error(&error) => {
            if let Some(remediation) = agent_task_service::offloaded_status_remediation(run_id)? {
                return Ok((remediation, 1));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let record = durable_read.record;
    let runner_probe = runner_probe_projection(&agent_task_lifecycle::runner_probe_plan(
        &record,
        agent_task_lifecycle::AgentTaskStatusOptions {
            runner_probe: agent_task_lifecycle::AgentTaskRunnerProbe::Never,
        },
    ));
    // A future durable budget is incompatible, not an absent optional preview.
    if let Err(error) = agent_task_lifecycle::load_plan(run_id) {
        if error
            .message
            .contains("unsupported agent-task execution budget version")
        {
            return Err(error);
        }
    }
    let mut value = serde_json::to_value(&record).unwrap_or(Value::Null);
    // The default/`--full` status path is a durable-local read: reconciliation
    // has its own explicit command so an unavailable runner cannot hold status
    // hostage. That makes this answer directly comparable to `activity show`,
    // and the flag says so instead of leaving the caller to guess (#W3-15).
    attach_reconciled(&mut value, false);
    attach_status_identity(&mut value, &args.run_id, &target);
    attach_cook_notification_delivery(&mut value, &record, &target);
    attach_durable_read_availability(&mut value, &durable_read.unavailable_sources);
    attach_cook_completion(&mut value, &record);
    let acceptance_is_actionable = record.state
        == agent_task_lifecycle::AgentTaskRunState::Succeeded
        && record
            .metadata
            .pointer("/latest_promotion/status")
            .and_then(Value::as_str)
            == Some("applied");
    if acceptance_is_actionable {
        if let Some(verdict) = value.pointer("/acceptance/verdict").and_then(Value::as_str) {
            match verdict {
                "pending" => {
                    value["terminal_status"] = Value::String("awaiting_acceptance".to_string());
                }
                "rejected" => {
                    value["terminal_status"] = Value::String("repair_required".to_string());
                    let attempts = value
                        .pointer("/acceptance/repair_attempts")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    value["acceptance_continuation"] = json!({
                        "status": if attempts == 1 { "repair_available" } else { "repair_exhausted" },
                        "max_attempts": 1,
                        "command": (attempts == 1).then(|| format!("homeboy agent-task retry {} --run", record.run_id)),
                    });
                }
                _ => {}
            }
        }
    }
    enrich_with_diagnostic_summary(&mut value, run_id)?;
    if let Some((progress, source_run_id)) = selected_cook_terminal_progress(&target, run_id) {
        project_owning_cook_terminal_status_from_progress(
            &mut value,
            Some(&progress),
            Some(&source_run_id),
        );
    } else {
        project_owning_cook_terminal_status(&mut value);
    }
    attach_transport_proxy_recovery_guidance(&mut value, run_id);
    if let Some(selection) = target.selection.as_ref() {
        value["candidate_selection"] = selection.clone();
    }
    if args.full {
        let aggregate = completed_run_aggregate(run_id).and_then(Result::ok);
        attach_full_status_candidate(&mut value, aggregate.as_ref(), run_id);
        attach_runner_probe(&mut value, &runner_probe);
        attach_agent_task_status_actionable(&mut value, run_id);
        preserve_controller_owner_placement(&mut value, run_id);
        let exit_code = subject_exit_code(&value, args.strict_subject_exit);
        return Ok((value, exit_code));
    }
    let summary = compact_status_summary(&value, run_id);
    let mut summary = summary;
    attach_reconciled(&mut summary, false);
    attach_status_identity(&mut summary, &args.run_id, &target);
    if let Some(selection) = target.selection {
        summary["candidate_selection"] = selection;
    }
    attach_runner_probe(&mut summary, &runner_probe);
    attach_agent_task_status_actionable(&mut summary, run_id);
    preserve_controller_owner_placement(&mut summary, run_id);
    let exit_code = subject_exit_code(&summary, args.strict_subject_exit);
    Ok((summary, exit_code))
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
            self.changes.push(json!({ "poll": poll, "status": snapshot }));
        } else {
            self.omitted += 1;
        }
    }
}

fn emit_status_change_event(snapshot: &Value, poll: u64, retained_limit_reached: bool) -> String {
    let run_id = snapshot
        .get("run_id")
        .and_then(Value::as_str)
        .or_else(|| snapshot.pointer("/identity/resolved_run_id").and_then(Value::as_str));
    serde_json::to_string(&json!({
        "schema": "homeboy/agent-task-status-watch-event/v1",
        "event": "status_changed",
        "run_id": run_id,
        "state": snapshot.get("state"),
        "poll": poll,
        "latest": snapshot,
        "retained_limit_reached": retained_limit_reached,
        "continuation_command": run_id.map(|run_id| format!(
            "homeboy agent-task status {} --watch", quote_arg(run_id)
        )),
    }))
    .expect("status watch JSONL event serializes")
}

/// Watch output follows lifecycle progress rather than volatile read metadata
/// such as timestamps. The complete status remains in each emitted/final snapshot.
fn status_change_projection(status: &Value) -> Value {
    json!({
        "state": status.get("state"),
        "terminal_status": status.get("terminal_status"),
        "child_run_state": status.get("child_run_state"),
        "totals": status.get("totals"),
        "tasks": status.get("tasks"),
        "progress": status.get("progress"),
        "liveness": status.get("liveness"),
        "normalized_events": status.get("normalized_events"),
    })
}

fn status_is_terminal(status: &Value) -> bool {
    !matches!(
        status.get("state").and_then(Value::as_str),
        Some("queued" | "running" | "in_flight")
    )
}

fn status_is_failure(status: &Value) -> bool {
    let Some(state) = status.get("state").and_then(Value::as_str) else {
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
    let (latest, status_exit) = result.item;
    let continuation = format!(
        "homeboy agent-task status {} --watch --interval {} --timeout {}",
        quote_arg(&args.run_id), args.interval, args.timeout
    );
    let output = json!({
        "schema": "homeboy/agent-task-status-watch/v1",
        "command": "agent-task.status.watch",
        "run_id": args.run_id,
        "terminal": !timed_out,
        "timed_out": timed_out,
        "poll_count": result.poll_count,
        "waited_secs": result.waited.as_secs(),
        "changes": progress.changes,
        "changes_omitted": progress.omitted,
        "latest": latest,
        "continuation_command": continuation,
    });
    let exit_code = if timed_out {
        TIMEOUT_EXIT_CODE
    } else if status_is_failure(&output["latest"]) {
        1
    } else {
        status_exit
    };
    (output, exit_code)
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
            "command": format!("homeboy agent-task cook-continue {}", attempt.run_id),
            "message": "The immutable Cook recipe is durable but its lifecycle record is absent. Continue the exact attempt to materialize the controller lifecycle before provider work."
        }
    })))
}

/// Record whether this read reconciled — i.e. whether producing this answer
/// also *wrote*, and therefore changed what a later read of the same run
/// returns.
///
/// `activity show <id>` and `agent-task status <id>` can legitimately disagree
/// about the same run at the same instant because one is a read model and the
/// other (on `--bridge`) is a reconciling read. That is by design and is not
/// changed here; it is only made visible, so a consumer can tell which kind of
/// answer it received without inferring it from the command name (#W3-15).
fn attach_reconciled(value: &mut Value, reconciled: bool) {
    if let Value::Object(fields) = value {
        fields.insert("reconciled".to_string(), Value::Bool(reconciled));
    }
}

fn attach_status_identity(value: &mut Value, requested_run_id: &str, target: &CookReaderTarget) {
    if let Value::Object(fields) = value {
        let mut identity = json!({
            "requested_run_id": requested_run_id,
            "resolved_run_id": target.run_id,
            "resolution": if target.exact { "exact_record" } else { "default" },
        });
        if let Some(cook_alias) = &target.cook_alias {
            identity["cook_alias"] = cook_alias.clone();
        }
        fields.insert("identity".to_string(), identity);
    }
}

/// The outcome is stored beside the Cook index rather than copied to every
/// attempt record. Project it onto the resolved read so compact and full status
/// answer whether terminal silence means delivered, unconfigured, or failed.
fn attach_cook_notification_delivery(
    value: &mut Value,
    record: &AgentTaskRunRecord,
    target: &CookReaderTarget,
) {
    let cook_id = target
        .cook_alias
        .as_ref()
        .and_then(|alias| alias.get("cook_id"))
        .and_then(Value::as_str)
        .or_else(|| record.metadata.get("cook_id").and_then(Value::as_str));
    let Some(cook_id) = cook_id else { return };
    let Ok(Some(mut outcome)) = agent_task_lifecycle::cook_terminal_notification_outcome(cook_id)
    else {
        return;
    };
    if outcome.get("status").and_then(Value::as_str) != Some("delivered") {
        outcome["retry_command"] = Value::String(format!(
            "homeboy agent-task cook --continue {}",
            quote_arg(cook_id)
        ));
    }
    if outcome.get("status").and_then(Value::as_str) == Some("not_configured") {
        outcome["configuration_command"] = Value::String(
            "homeboy config set /notifications/default_transport '<installed-transport-id>'"
                .to_string(),
        );
    }
    if let Value::Object(fields) = value {
        fields.insert("notification_delivery".to_string(), outcome);
    }
}

/// A Cook owns the provider attempt's publication lifecycle. Once it records a
/// terminal outcome, that outcome is the status headline; the child run state
/// remains available as `child_run_state` and in `execution_states.provider`.
fn project_owning_cook_terminal_status(value: &mut Value) {
    let progress = value.pointer("/metadata/cook_progress").cloned();
    project_owning_cook_terminal_status_from_progress(value, progress.as_ref(), None);
}

/// Main's substantive-candidate reader can resolve an earlier patch-producing
/// attempt while the latest attempt owns the Cook's terminal publication result.
/// Read that separate lifecycle record so candidate evidence and Cook truth stay
/// visible together.
fn selected_cook_terminal_progress(
    target: &CookReaderTarget,
    selected_run_id: &str,
) -> Option<(Value, String)> {
    let source_run_id = target
        .selection
        .as_ref()?
        .get("latest_attempt_run_id")?
        .as_str()?
        .to_string();
    if source_run_id == selected_run_id {
        return None;
    }
    let record = agent_task_service_direct::persisted_status(&source_run_id).ok()?;
    let progress = record.metadata.get("cook_progress")?.clone();
    (progress.get("phase").and_then(Value::as_str) == Some("terminal"))
        .then_some((progress, source_run_id))
}

fn project_owning_cook_terminal_status_from_progress(
    value: &mut Value,
    progress: Option<&Value>,
    source_run_id: Option<&str>,
) {
    let Some(progress) = progress
        .filter(|progress| progress.get("phase").and_then(Value::as_str) == Some("terminal"))
    else {
        return;
    };
    let Some(detail) = progress
        .get("detail")
        .and_then(Value::as_str)
        .filter(|status| !status.trim().is_empty())
    else {
        return;
    };
    let projection = cook_lifecycle_projection(
        value,
        detail,
        progress.get("terminal_success").and_then(Value::as_bool) == Some(true),
    );
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(run_state) = object.get("state").cloned() {
        object.insert("child_run_state".to_string(), run_state);
    }
    object.insert(
        "state".to_string(),
        Value::String(projection.status.to_string()),
    );
    project_cook_task_status(object, &projection.status);
    if let Some(execution_states) = object.get_mut("execution_states") {
        project_execution_states(execution_states, &projection);
    }
    let mut cook = json!({
        "state": projection.status,
        "phase": "terminal",
        "publication": projection.publication,
    });
    if let Some(source_run_id) = source_run_id {
        cook["source_run_id"] = json!(source_run_id);
    }
    object.insert("cook".to_string(), cook);
}

/// The Cook result is a lifecycle decision, not the provider's exit status.
/// Provider success and an applied patch remain useful evidence, but a required
/// gate or finalization outcome determines whether the Cook is review-ready.
#[derive(Clone)]
struct CookLifecycleProjection {
    status: String,
    publication: &'static str,
    candidate: &'static str,
    gate: &'static str,
    promotion: String,
    finalization: String,
}

fn cook_lifecycle_projection(
    record: &Value,
    detail: &str,
    reported_success: bool,
) -> CookLifecycleProjection {
    let promotion = promotion_state(record);
    let finalization = finalization_state(record);
    let gate = promotion_gate_state(record, &promotion);
    let status = if !reported_success {
        detail
    } else if gate == "failed" {
        "gate_failed"
    } else if finalization == "finalization_failed" {
        "finalization_failed"
    } else if finalization == "finalization_pending" {
        "finalization_pending"
    } else if promotion == "verification_pending" {
        "verification_pending"
    } else if promotion == "applied"
        && finalization == "not_attempted"
        && detail != "green_no_finalize"
    {
        "finalization_not_attempted"
    } else {
        detail
    }
    .to_string();
    let candidate = match (promotion.as_str(), gate, finalization.as_str()) {
        ("gate_failed", "accepted_inherited_failure", _) => "promoted_accepted_inherited_failure",
        ("gate_failed", _, _) | (_, "failed", _) => "promoted_gate_failed",
        ("verification_pending", _, _) => "promoted_verification_pending",
        ("applied", _, "finalization_failed") => "promoted_finalization_failed",
        ("applied", _, "finalization_pending") => "promoted_finalization_pending",
        ("applied", _, "not_attempted") => "promoted_finalization_not_attempted",
        ("applied", _, _) => "promoted",
        _ => "unknown",
    };
    let publication = if !reported_success {
        "blocked"
    } else if matches!(
        status.as_str(),
        "review_ready" | "draft_published" | "green_no_finalize"
    ) {
        "completed"
    } else if matches!(status.as_str(), "gate_failed" | "finalization_failed") {
        "blocked"
    } else {
        "pending"
    };
    CookLifecycleProjection {
        status,
        publication,
        candidate,
        gate,
        promotion,
        finalization,
    }
}

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

fn project_cook_task_status(record: &mut serde_json::Map<String, Value>, status: &str) {
    let Some(tasks) = record.get_mut("tasks").and_then(Value::as_array_mut) else {
        return;
    };
    for task in tasks {
        if task.get("state").and_then(Value::as_str) == Some("succeeded") {
            task["provider_state"] = Value::String("succeeded".to_string());
            task["state"] = Value::String(status.to_string());
        }
    }
}

fn project_execution_states(states: &mut Value, projection: &CookLifecycleProjection) {
    states["candidate"]["state"] = Value::String(projection.candidate.to_string());
    states["gate"]["state"] = Value::String(projection.gate.to_string());
    states["promotion"]["state"] = Value::String(projection.promotion.clone());
    states["finalization"]["state"] = Value::String(projection.finalization.clone());
}

fn cook_requires_action(value: &Value) -> bool {
    value.pointer("/cook/phase").and_then(Value::as_str) == Some("terminal")
        && value.pointer("/cook/publication").and_then(Value::as_str) != Some("completed")
}

fn subject_exit_code(value: &Value, strict_subject_exit: bool) -> i32 {
    if !strict_subject_exit {
        return 0;
    }
    if cook_requires_action(value) {
        1
    } else {
        0
    }
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

fn attach_full_status_candidate(
    value: &mut Value,
    aggregate: Option<&AgentTaskAggregate>,
    run_id: &str,
) {
    if let Some(aggregate) = aggregate {
        if let Value::Object(fields) = value {
            fields.insert(
                "aggregate".to_string(),
                serde_json::to_value(aggregate).unwrap_or(Value::Null),
            );
        }
    }
    let canonical = classify_candidates(value);
    let liveness = liveness_summary(value, run_id, canonical.state());
    if let Value::Object(fields) = value {
        fields.insert(
            "canonical_candidate".to_string(),
            canonical_candidate_projection(canonical),
        );
        fields.insert("liveness".to_string(), liveness);
    }
}

/// Project the read-side runner-probe decision into the status payload.
///
/// This is the operator's answer to "is this a complete status, or a local one
/// because the runner could not be consulted?" (#10418). It is always present
/// so the distinction never has to be inferred from a missing field.
pub(crate) fn runner_probe_projection(
    plan: &agent_task_lifecycle::AgentTaskRunnerProbePlan,
) -> Value {
    let mut projection = serde_json::to_value(plan).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut projection {
        map.insert(
            "note".to_string(),
            Value::String(runner_probe_note(plan.skipped_reason)),
        );
    }
    projection
}

fn runner_probe_note(skipped_reason: Option<&str>) -> String {
    let Some(reason) = skipped_reason else {
        return "runner reconciliation was performed for this read".to_string();
    };
    if reason == agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL {
        "run is controller-local; answered from durable controller state without contacting any runner".to_string()
    } else if reason == agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT {
        "--no-runner-probe was requested; this is a partial, controller-local answer and runner-side job state may be stale".to_string()
    } else if reason == agent_task_lifecycle::RUNNER_PROBE_SKIPPED_NOT_RUNNING {
        "run is not running; there is no live runner job to reconcile".to_string()
    } else {
        format!("runner reconciliation was skipped: {reason}")
    }
}

fn attach_runner_probe(value: &mut Value, runner_probe: &Value) {
    if let Value::Object(map) = value {
        map.insert("runner_probe".to_string(), runner_probe.clone());
    }
}

#[cfg(test)]
mod runner_probe_tests {
    use super::*;

    #[test]
    fn a_controller_local_answer_says_it_never_contacted_a_runner() {
        let projection = runner_probe_projection(&agent_task_lifecycle::AgentTaskRunnerProbePlan {
            performed: false,
            skipped_reason: Some(agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL),
            controller_local: true,
        });

        assert_eq!(projection["performed"], false);
        assert_eq!(projection["controller_local"], true);
        assert_eq!(projection["skipped_reason"], "controller_local_record");
        assert!(projection["note"]
            .as_str()
            .expect("note")
            .contains("without contacting any runner"));
    }

    #[test]
    fn an_opted_out_answer_is_labelled_partial() {
        let projection = runner_probe_projection(&agent_task_lifecycle::AgentTaskRunnerProbePlan {
            performed: false,
            skipped_reason: Some(agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT),
            controller_local: false,
        });

        assert_eq!(projection["skipped_reason"], "caller_opted_out");
        assert!(projection["note"]
            .as_str()
            .expect("note")
            .contains("partial"));
    }

    #[test]
    fn a_completed_probe_reports_no_skip_reason() {
        let projection = runner_probe_projection(&agent_task_lifecycle::AgentTaskRunnerProbePlan {
            performed: true,
            skipped_reason: None,
            controller_local: false,
        });

        assert_eq!(projection["performed"], true);
        assert!(projection.get("skipped_reason").is_none());
        assert!(projection["note"]
            .as_str()
            .expect("note")
            .contains("was performed"));
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

/// Project only explicitly heavy evidence fields. This preserves every other
/// payload type and collection, including actions, identities, gates, and refs.
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
                        if text.len() > COMPACT_TEXT_LIMIT {
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

fn is_missing_agent_task_run_metadata_error(error: &homeboy::core::Error) -> bool {
    error.code == homeboy::core::ErrorCode::InternalJsonError
        && error.message.contains("is missing agent_task_run metadata")
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
pub(super) fn reconcile_run(run_id: &str, dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_service_direct::reconcile_run(run_id, dry_run)?;
    let exit = if report.failed > 0 { 1 } else { 0 };
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), exit))
}

pub(super) fn reconcile_records(dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_lifecycle::reconcile_record_health(dry_run)?;
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

fn attach_agent_task_status_actionable(value: &mut Value, run_id: &str) {
    let mut metadata = CommandActionableMetadata {
        refs: CommandResultRefs {
            agent_tasks: vec![agent_task_ref(run_id)],
            ..Default::default()
        },
        next_actions: vec![
            CommandNextAction::new(
                "show status",
                format!("homeboy agent-task status {run_id} --full"),
            )
            .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new("show logs", format!("homeboy agent-task logs {run_id}"))
                .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new(
                "list artifacts",
                format!("homeboy agent-task artifacts {run_id}"),
            )
            .with_kind(CommandNextActionKind::Artifacts),
        ],
        ..Default::default()
    };

    if cook_requires_action(value) {
        metadata.next_actions.push(
            CommandNextAction::new(
                "diagnose blocked Cook",
                format!("homeboy agent-task diagnose {run_id} --full"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    } else if classify_candidates(value).state().is_available() {
        let review_command = format!("homeboy agent-task review {run_id}");
        metadata.next_actions.push(
            CommandNextAction::new("review run", review_command)
                .with_kind(CommandNextActionKind::Show),
        );
    }

    if let Some(command) = transport_proxy_recovery_command(value) {
        metadata.next_actions.push(
            CommandNextAction::new("recover runner transport", command)
                .with_kind(CommandNextActionKind::Repair),
        );
    }

    if let Some(command) = value
        .get("notification_delivery")
        .and_then(|delivery| delivery.get("retry_command"))
        .and_then(Value::as_str)
    {
        metadata.next_actions.push(
            CommandNextAction::new("retry terminal notification", command)
                .with_kind(CommandNextActionKind::Repair),
        );
    }

    if let Some(command) = value
        .get("notification_delivery")
        .and_then(|delivery| delivery.get("configuration_command"))
        .and_then(Value::as_str)
    {
        metadata.next_actions.push(
            CommandNextAction::new("configure terminal notifications", command)
                .with_kind(CommandNextActionKind::Repair),
        );
    }

    if value
        .pointer("/metadata/stale_running")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        metadata.next_actions.push(
            CommandNextAction::new(
                "reconcile stale run",
                format!(
                    "homeboy agent-task reconcile {} --dry-run",
                    quote_arg(run_id)
                ),
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }

    attach_actionable_metadata(value, metadata);
}

fn transport_proxy_recovery_command(value: &Value) -> Option<String> {
    value
        .get("transport_recovery")
        .and_then(|recovery| recovery.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn attach_transport_proxy_recovery_guidance(value: &mut Value, run_id: &str) {
    let Some(kind) = value.pointer("/metadata/kind").and_then(Value::as_str) else {
        return;
    };
    if !kind.ends_with("_controller_proxy") {
        return;
    }
    let runner_id = value.pointer("/metadata/runner_id").and_then(Value::as_str);
    let job_id = value
        .pointer("/metadata/runner_job_id")
        .or_else(|| value.pointer("/metadata/runner_execution_record/job_id"))
        .and_then(Value::as_str);
    let guidance = transport_proxy_recovery_guidance(run_id, runner_id, job_id);
    if let Value::Object(map) = value {
        map.insert("transport_recovery".to_string(), guidance);
    }
}

fn transport_proxy_recovery_guidance(
    run_id: &str,
    runner_id: Option<&str>,
    job_id: Option<&str>,
) -> Value {
    let Some(runner_id) = runner_id.filter(|runner_id| !runner_id.trim().is_empty()) else {
        return json!({
            "condition": "controller_proxy_without_runner",
            "runner_id": Value::Null,
            "runner_job_id": job_id,
            "command": format!("homeboy agent-task run {}", quote_arg(run_id)),
        });
    };
    let local_runner = runner::load(runner_id).is_ok_and(|runner| runner.kind == RunnerKind::Local);
    let runner_command_id = quote_arg(runner_id);
    let (condition, command) = if local_runner {
        (
            "local_runner_resume_required",
            format!("homeboy agent-task run {}", quote_arg(run_id)),
        )
    } else if job_id.is_some() {
        (
            "runner_status_refresh_required",
            format!("homeboy runner status {runner_command_id}"),
        )
    } else {
        (
            "controller_proxy_without_runner_job",
            format!("homeboy runner status {runner_command_id}"),
        )
    };
    json!({ "condition": condition, "runner_id": runner_id, "runner_job_id": job_id, "command": command })
}

#[cfg(test)]
mod transport_proxy_tests {
    use super::*;

    #[test]
    fn transport_proxy_guidance_requires_an_explicit_authoritative_status_refresh() {
        let recorded_job = transport_proxy_recovery_guidance(
            "agent-task-proxy-42",
            Some("runner-transport-42"),
            Some("job-42"),
        );
        assert_eq!(recorded_job["condition"], "runner_status_refresh_required");
        assert_eq!(
            recorded_job["command"],
            "homeboy runner status runner-transport-42"
        );
    }

    #[test]
    fn transport_proxy_guidance_reports_missing_runner_and_quotes_commands() {
        let missing_runner = transport_proxy_recovery_guidance("run with spaces", None, None);
        assert_eq!(
            missing_runner["condition"],
            "controller_proxy_without_runner"
        );
        assert_eq!(
            missing_runner["command"],
            "homeboy agent-task run 'run with spaces'"
        );

        let unknown_runner = transport_proxy_recovery_guidance(
            "agent-task-proxy-42",
            Some("runner with spaces"),
            None,
        );
        assert_eq!(
            unknown_runner["condition"],
            "controller_proxy_without_runner_job"
        );
        assert_eq!(
            unknown_runner["command"],
            "homeboy runner status 'runner with spaces'"
        );
    }

    #[test]
    fn transport_proxy_guidance_resumes_local_runner_without_a_status_probe() {
        let guidance = transport_proxy_recovery_guidance("agent-task-local", Some("local"), None);

        assert_eq!(guidance["condition"], "local_runner_resume_required");
        assert_eq!(
            guidance["command"],
            "homeboy agent-task run agent-task-local"
        );
    }
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
        status_command: format!("homeboy agent-task status {run_id} --full"),
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

/// Follow-up lifecycle commands must retain the controller-local ownership of
/// the record even when a default Lab runner becomes available between reads.
fn preserve_controller_owner_placement(value: &mut Value, run_id: &str) {
    match value {
        Value::String(command) => {
            let prefix = "homeboy agent-task ";
            let lifecycle_command = command
                .strip_prefix(prefix)
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
                    )
                });
            if lifecycle_command && command.contains(run_id) {
                *command = command.replacen(prefix, "homeboy --placement local agent-task ", 1);
            }
        }
        Value::Array(values) => {
            for value in values {
                preserve_controller_owner_placement(value, run_id);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                preserve_controller_owner_placement(value, run_id);
            }
        }
        _ => {}
    }
}

pub(super) fn logs(args: LogsArgs) -> CmdResult<Value> {
    let log = if args.raw {
        agent_task_service_direct::logs_with_raw(&args.run_id)?
    } else {
        agent_task_service_direct::logs(&args.run_id)?
    };
    let mut value = serde_json::to_value(log).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    Ok((value, 0))
}

pub(super) fn artifacts(args: LifecycleReadArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let mut value = serde_json::to_value(artifacts).unwrap_or(Value::Null);
    if !args.full {
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
    // Keep diagnosis within the same durable-local inspection contract as
    // status and logs; reconciliation is explicitly requested separately.
    let record = agent_task_service_direct::persisted_status(run_id)?;
    let aggregate = completed_run_aggregate(run_id).transpose()?;
    let mut hydrated_evidence = Vec::new();
    let mut total_hydrated_evidence = 0;
    let mut nested_reasons = persisted_cook_failure_diagnostic(&record)
        .into_iter()
        .collect::<Vec<_>>();
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
        .map(|item| collected_diagnostic_value_with_details(item, args.full));
    let diagnostic_chain = ranked_reasons
        .into_iter()
        .take(FAILURE_REASON_LIMIT)
        .map(|item| collected_diagnostic_value_with_details(item, args.full))
        .collect::<Vec<_>>();

    let missing_artifacts = aggregate
        .as_ref()
        .map(missing_artifact_summaries)
        .unwrap_or_default();
    let causal_chain = aggregate
        .as_ref()
        .map(causal_chain_from_aggregate)
        .unwrap_or_default();
    let retry = retry_replay_action(&record);
    let next_commands =
        diagnose_next_commands(&record, retry.action.as_ref(), retry.continuation.as_ref());

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
        "continuation_admission": record.metadata.get("cook_continuation_admission"),
        "retry_replay": retry.projection(),
        "next_commands": next_commands,
    });
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
    );
    preserve_controller_owner_placement(&mut value, run_id);
    Ok((value, 0))
}

/// Add the Cook-level completion fact to record readers. Aggregate success is
/// nested provider evidence; only a durable PR receipt completes a requested
/// Cook publication.
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
        value["cook_completion"] = serde_json::to_value(completion).expect("completion serializes");
    }
}

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
}

/// Collect the distinct failure classifications a run actually recorded, first
/// implicated task wins. Successful and no-op outcomes are never a failure
/// signal even if a stale classification survived on them.
fn diagnosed_failures(aggregate: &AgentTaskAggregate) -> Vec<DiagnosedFailure> {
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
) {
    let run_id = record.run_id.as_str();
    let candidate_payload = candidate_result_payload(
        &serde_json::to_value(record).unwrap_or(Value::Null),
        aggregate,
    );
    let candidate_recoverable = classify_candidates(&candidate_payload)
        .state()
        .is_available();
    let failures = aggregate.map(diagnosed_failures).unwrap_or_default();
    let (next_actions, basis) = if let Some(continuation) = continuation_action {
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
    if actions.is_empty() {
        return (
            generic_diagnose_next_actions(run_id, retry_action),
            DIAGNOSE_ACTION_BASIS_FALLBACK,
        );
    }
    (actions, DIAGNOSE_ACTION_BASIS_DIAGNOSIS)
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
        // Policy refused this request. Retrying an identical request is denied
        // identically, so no retry action is emitted.
        AgentTaskFailureClassification::PolicyDenied => vec![
            failure_evidence,
            CommandNextAction::new(
                "show the full run record including the policy that denied it",
                format!("homeboy agent-task status {run} --full"),
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
        AgentTaskFailureClassification::InvalidInput => vec![
            failure_evidence,
            CommandNextAction::new(
                format!("replay the provider boundary for {}", failure.task_id),
                format!("homeboy agent-task replay-provider-boundary {run} --task {task}"),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
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
            format!("homeboy runner doctor {runner} --repair"),
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
            format!("homeboy agent-task status {run} --full"),
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
        status_command: format!("homeboy agent-task status {run} --full"),
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
            exact: false,
            bridge: false,
            since_cursor: None,
            full: false,
            strict_subject_exit: false,
            no_runner_probe: false,
            watch: true,
            interval: "250ms".to_string(),
            timeout: "2m".to_string(),
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
        assert_eq!(first["schema"], "homeboy/agent-task-status-watch-event/v1");
        assert_eq!(first["event"], "status_changed");
        assert_eq!(first["poll"], 1);
        assert_eq!(first["run_id"], "run-1");
        assert_eq!(first["state"], "running");
        assert_eq!(first["latest"], running);
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
            progress.observe(&json!({ "run_id": "run-1", "state": "running", "progress": poll }), poll, |line| {
                emitted.push(line)
            });
        }
        let terminal_poll = STATUS_WATCH_CHANGE_LIMIT as u64 + 3;
        progress.observe(
            &json!({ "run_id": "run-1", "state": "succeeded", "progress": terminal_poll }),
            terminal_poll,
            |line| emitted.push(line),
        );

        assert_eq!(progress.changes.len(), STATUS_WATCH_CHANGE_LIMIT);
        assert_eq!(progress.omitted, 3);
        assert_eq!(emitted.len(), STATUS_WATCH_CHANGE_LIMIT + 3);
        let first_overflow: Value = serde_json::from_str(&emitted[STATUS_WATCH_CHANGE_LIMIT])
            .expect("first overflow event");
        assert_eq!(first_overflow["poll"], STATUS_WATCH_CHANGE_LIMIT as u64 + 1);
        assert_eq!(first_overflow["latest"]["progress"], STATUS_WATCH_CHANGE_LIMIT as u64 + 1);
        let overflow: Value = serde_json::from_str(emitted.last().unwrap()).expect("overflow event");
        assert_eq!(overflow["retained_limit_reached"], true);
        assert_eq!(overflow["state"], "succeeded");
        assert_eq!(overflow["poll"], terminal_poll);
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
        assert_eq!(emitted.len(), 2, "progress was emitted before timeout returned");
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
    fn bridge_snapshot_is_retained_without_projection_loss() {
        let bridge = json!({
            "schema": "homeboy/agent-task-run-status/v1",
            "state": "succeeded",
            "reconciled": true,
            "normalized_events": [{ "type": "agent_task.state_changed" }]
        });
        let (output, exit) = watch_status_output(
            &args(),
            WatchResult {
                item: (bridge.clone(), 0),
                conclusion: WatchConclusion::Terminal,
                poll_count: 1,
                waited: Duration::ZERO,
            },
            StatusWatchProgress {
                changes: vec![json!({ "poll": 1, "status": bridge })],
                ..Default::default()
            },
        );

        assert_eq!(exit, 0);
        assert_eq!(output["latest"]["reconciled"], true);
        assert!(output["latest"]["normalized_events"].is_array());
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
                "homeboy runner doctor homeboy-lab --repair",
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy --placement local agent-task retry run-1 --run",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner doctor homeboy-lab --repair",
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
                "homeboy agent-task status run-1 --full",
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
    fn invalid_input_replays_the_rejected_boundary_instead_of_resending_it() {
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
        );

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task status run-1 --full",
                "homeboy agent-task artifacts run-1",
                "homeboy agent-task review run-1",
            ]
        );
    }

    #[test]
    fn a_run_with_no_diagnosis_at_all_falls_back_to_the_generic_set() {
        let (actions, basis) = diagnose_next_actions("run-1", &[], &[], None, None);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(actions.len(), 3);
    }

    #[test]
    fn missing_artifacts_name_the_task_and_the_artifacts_that_were_not_produced() {
        let missing = vec![json!({
            "task_id": "task-b",
            "missing": ["concept_packet", "design_packet"],
        })];

        let (actions, basis) = diagnose_next_actions("run-1", &[], &missing, None, None);

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
            }],
            &[],
            None,
            None,
        );

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence 'run with spaces' --task 'task with spaces' --failure-only",
                "homeboy agent-task replay-provider-boundary 'run with spaces' --task 'task with spaces'",
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
    let recovered = homeboy::agents::agent_task_lifecycle::recover_controller_runtime(
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
    let record = homeboy::agents::agent_task_lifecycle::validate_controller_runtime(&args.run_id)?;
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

pub(super) fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let record = agent_task_service::cancel(&args.run_id, args.reason.as_deref())?;
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    surface_cancellation_recovery(&mut value);
    Ok((value, 0))
}

pub(super) fn quarantine(args: QuarantineArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::quarantine_queued_run_exact(&args.run_id, &args.reason)?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

pub(super) fn rearm(args: RearmArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::rearm_quarantined_run(&args.run_id)?;
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

fn enrich_with_diagnostic_summary(value: &mut Value, run_id: &str) -> homeboy::core::Result<()> {
    let Some(aggregate) = completed_run_aggregate(run_id).transpose()? else {
        return Ok(());
    };
    let (summary, truncations) = diagnostic_summary(&aggregate);
    if let Some(summary) = summary {
        value["diagnostic_summary"] = summary;
    }
    value["diagnostic_collection"] = diagnostic_collection_projection(&truncations);
    let failure_reasons = failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        value["failure_reasons"] = Value::Array(failure_reasons);
    }
    value["execution_states"] = execution_states_from_aggregate(&aggregate, value);
    value["aggregate"] = serde_json::to_value(&aggregate).unwrap_or(Value::Null);
    Ok(())
}

/// Rank aggregate and executor evidence together before selecting the compact
/// status root cause. Neither source is a fallback: either can contain the
/// actionable diagnostic that explains a generic provider observation.
fn diagnostic_summary(aggregate: &AgentTaskAggregate) -> (Option<Value>, Vec<Value>) {
    let mut diagnostics = aggregate_failure_diagnostics(aggregate);
    let truncations = collect_aggregate_evidence_diagnostics(aggregate, &mut diagnostics);
    let summary = ranked_diagnostics(diagnostics)
        .into_iter()
        .map(collected_diagnostic_value)
        .next();
    (summary, truncations)
}

fn collect_aggregate_evidence_diagnostics(
    aggregate: &AgentTaskAggregate,
    diagnostics: &mut Vec<CollectedDiagnostic>,
) -> Vec<Value> {
    let mut truncations = Vec::new();
    let mut budget = DiagnosticCollectionBudget::default();
    for outcome in &aggregate.outcomes {
        for evidence in &outcome.evidence_refs {
            if let Some(truncation) = collect_hydrated_evidence_diagnostics(
                &outcome.task_id,
                evidence,
                diagnostics,
                &mut budget,
            ) {
                truncations.push(truncation);
            }
        }
    }
    truncations
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
    let priority = if is_policy_denial(&class, &text) {
        0
    } else if is_required_output_diagnostic(&class) {
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
/// wrapper's outcome-normalization consequence.
fn collect_process_stream_diagnostics(
    task_id: &str,
    streams: &Value,
    out: &mut Vec<CollectedDiagnostic>,
) {
    for stream in streams.as_array().into_iter().flatten() {
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
fn compact_status_summary(record: &Value, run_id: &str) -> Value {
    let aggregate = completed_run_aggregate(run_id).and_then(Result::ok);
    compact_status_summary_with_aggregate(record, run_id, aggregate.as_ref())
}

fn compact_status_summary_with_aggregate(
    record: &Value,
    run_id: &str,
    aggregate: Option<&AgentTaskAggregate>,
) -> Value {
    let plan = agent_task_lifecycle::load_plan(run_id).ok();
    let task_table = task_source_table(record, plan.as_ref());
    let tasks_omitted = record
        .get("tasks")
        .and_then(Value::as_array)
        .map_or(0, |tasks| tasks.len().saturating_sub(COMPACT_TASK_LIMIT));
    let ref_inventory = ref_inventory(record, aggregate);
    let (refs, refs_omitted) = compact_refs(&ref_inventory);
    let risk_flags = risk_flags(record);
    let work_summary = work_summary(record, aggregate, &ref_inventory);
    let canonical_candidate = classify_candidates(&candidate_result_payload(record, aggregate));

    let mut summary = json!({
        "schema": "homeboy/agent-task-status-summary/v1",
        "run_id": record.get("run_id").cloned().unwrap_or_else(|| json!(run_id)),
        "state": record.get("state").cloned().unwrap_or(Value::Null),
        "child_run_state": record.get("child_run_state").cloned().unwrap_or(Value::Null),
        "cook": record.get("cook").cloned().unwrap_or(Value::Null),
        "timestamps": compact_fields(record, &["created_at", "updated_at", "started_at", "completed_at"]),
        "work_summary": work_summary,
        "canonical_candidate": canonical_candidate_projection(canonical_candidate),
        "artifact_refs": refs.clone(),
        "artifact_refs_omitted": refs_omitted,
        "totals": record.get("totals").cloned().unwrap_or(Value::Null),
        "tasks": task_table,
        "tasks_omitted": tasks_omitted,
        "refs": refs,
        "refs_omitted": refs_omitted,
        "risk_flags": risk_flags,
        "execution_location": execution_location(record),
        "queue_visibility": queue_visibility(record),
        "execution_budget": plan.as_ref().map(|plan| &plan.options.execution_budget),
        "liveness": liveness_summary(record, run_id, canonical_candidate.state()),
        "full_command": format!("homeboy agent-task status {run_id} --full"),
    });

    if let Some(diagnostic) = record.get("diagnostic_summary") {
        if !diagnostic.is_null() {
            summary["diagnostic_summary"] = diagnostic.clone();
        }
    }
    if let Some(recovery) = record.get("transport_recovery") {
        summary["transport_recovery"] = recovery.clone();
    }
    if let Some(failure_reasons) = record.get("failure_reasons") {
        if failure_reasons
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
        {
            summary["failure_reasons"] = failure_reasons.clone();
        }
    }
    if let Some(execution_states) = record.get("execution_states") {
        summary["execution_states"] = execution_states.clone();
    }
    if let Some(aggregate_path) = record.get("aggregate_path") {
        if !aggregate_path.is_null() {
            summary["aggregate_path"] = aggregate_path.clone();
        }
    }
    if let Some(plan) = plan {
        summary["execution_budget"] =
            serde_json::to_value(&plan.options.execution_budget).unwrap_or(Value::Null);
    }
    if let Some(latest_promotion) = record
        .get("metadata")
        .and_then(|metadata| metadata.get("latest_promotion"))
    {
        if !latest_promotion.is_null() {
            summary["latest_promotion"] = compact_fields(
                latest_promotion,
                &[
                    "schema",
                    "status",
                    "run_id",
                    "task_id",
                    "artifact_id",
                    "patch_artifact_id",
                    "patch_artifact",
                    "patch",
                    "updated_at",
                    "created_at",
                    "command",
                ],
            );
        }
    }
    if let Some(cook_finalization) = record
        .get("metadata")
        .and_then(|metadata| metadata.get("cook_finalization"))
    {
        if !cook_finalization.is_null() {
            summary["cook_finalization"] = compact_fields(
                cook_finalization,
                &[
                    "schema",
                    "status",
                    "pr_number",
                    "pr_url",
                    "pull_request_url",
                    "updated_at",
                    "created_at",
                ],
            );
        }
    }
    if let Some(delivery) = record.get("notification_delivery") {
        summary["notification_delivery"] = compact_fields(
            delivery,
            &[
                "schema",
                "cook_id",
                "event_id",
                "event_kind",
                "transport",
                "route_classification",
                "status",
                "error_class",
                "transport_result",
                "retry_command",
                "configuration_command",
            ],
        );
    }
    summary
}

/// Project completion evidence for machine consumers. The durable aggregate and
/// `agent-task status <run-id> --full` remain the explicit lossless paths.
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
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id} --full"));
        summary["evidence_command"] = json!(format!("homeboy agent-task evidence {run_id}"));
    }
    summary
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
                "legal_actions",
                "next_actions",
            ],
        );
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
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id} --full"));
        summary["evidence_command"] = json!(format!("homeboy agent-task evidence {run_id}"));
    }
    for field in ["provider", "remaining_phases", "continuation_command"] {
        if let Some(value) = value.get(field) {
            summary[field] = bounded_value(value);
        }
    }
    summary
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
            claims.iter().find(|claim| {
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
    for field in fields {
        if let Some(value) = value.get(*field) {
            object.insert((*field).to_string(), bounded_value(value));
        }
    }
    Value::Object(object)
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
        Value::String(text) if text.chars().count() > COMPACT_TEXT_LIMIT => Value::String(format!(
            "{}...",
            text.chars().take(COMPACT_TEXT_LIMIT).collect::<String>()
        )),
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
fn record_state_is_terminal(record: &Value) -> bool {
    let lifecycle_state_is_terminal = |state: Option<&Value>| {
        state
            .and_then(|state| {
                serde_json::from_value::<agent_task_lifecycle::AgentTaskRunState>(state.clone())
                    .ok()
            })
            .is_some_and(agent_task_lifecycle::AgentTaskRunState::is_terminal)
    };

    lifecycle_state_is_terminal(record.get("state"))
        // A terminal Cook projects its controller status over the child run
        // state. Older controllers used a status such as `pre_execution_failure`,
        // which is not an AgentTaskRunState; the retained child state remains
        // authoritative for terminal liveness.
        || (record.pointer("/cook/phase").and_then(Value::as_str) == Some("terminal")
            && lifecycle_state_is_terminal(record.get("child_run_state")))
}

fn liveness_summary(record: &Value, run_id: &str, candidate_state: CandidateState) -> Value {
    let metadata = record.get("metadata").unwrap_or(&Value::Null);
    let provider_handle_count = record
        .get("provider_handles")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let stale = metadata
        .get("stale_running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let runner_job_id = metadata
        .get("runner_job_id")
        .and_then(Value::as_str)
        .filter(|job_id| !job_id.trim().is_empty());
    let terminal = record_state_is_terminal(record);
    let waiting_for_capacity = metadata
        .get("runner_queue")
        .and_then(|queue| queue.get("state"))
        .and_then(Value::as_str)
        == Some("waiting_for_capacity");
    let candidate_recoverable = candidate_state.is_recoverable();

    // A local cook runs the provider in an in-process thread, so it has neither
    // provider handles nor a runner job id. `reserve_provider_execution` still
    // durably records the running attempt (backend, model, started_at) before
    // the scheduler blocks on the backend, so surface that as authoritative
    // in-flight provider evidence instead of reporting the boundary absent
    // while the provider is actively working (#8396).
    let active_provider_executions = running_provider_executions(metadata);
    let has_active_provider_execution = !active_provider_executions.is_empty();
    let provider_boundary_recorded =
        provider_handle_count > 0 || runner_job_id.is_some() || has_active_provider_execution;
    let local_provider_ownership = metadata.get("local_provider_ownership");
    let local_supervisor = metadata.get("local_cook_supervisor").cloned().or_else(|| {
        metadata
            .get("detached_cook_handoff")
            .filter(|handoff| handoff.get("supervisor_job_id").is_some())
            .map(|handoff| {
                json!({
                    "job_id": handoff.get("supervisor_job_id"),
                    "reattach_command": handoff.get("reattach_command"),
                })
            })
    });

    json!({
        "status": if terminal { "terminal" } else if stale { "stale" } else if waiting_for_capacity { "waiting_for_capacity" } else { "active" },
        "heartbeat_last_seen_at": record.pointer("/lifecycle/heartbeat/last_seen_at"),
        "runner_job_status": metadata.get("runner_job_status"),
        "runner_job_last_seen_at": metadata.get("runner_job_last_seen_at"),
        "provider_boundary": {
            "status": if provider_boundary_recorded { "recorded" } else { "absent" },
            "provider_handle_count": provider_handle_count,
            "runner_job_id": runner_job_id,
            "active_execution_count": active_provider_executions.len(),
            "active_executions": active_provider_executions,
            "local_owner": local_provider_ownership,
        },
        "provider_activity": provider_activity_summary(metadata),
        "supervision": supervision_summary(metadata),
        "local_cook_supervisor": local_supervisor,
        "stale_reason": metadata.get("stale_running_reason"),
        "runner_queue": metadata.get("runner_queue"),
        "next_action": if terminal && candidate_recoverable {
            format!("homeboy agent-task review {run_id}")
        } else if stale {
            format!("homeboy agent-task reconcile {} --dry-run", quote_arg(run_id))
        } else if waiting_for_capacity {
            "await runner completion or reconnect; the runner will claim this FIFO queue entry under its capacity lease".to_string()
        } else {
            "homeboy agent-task status <run-id> --full".to_string()
        },
    })
}

/// Project the durably recorded provider-activity sample into the status
/// summary.
///
/// This is the answer to "what is the agent actually doing?" — the question
/// `agent-task status` could not answer, which is why diagnosing a stalled cook
/// meant `ps aux | grep` plus `git status` on a worktree path the operator had
/// to already know (#11482). `files_changed` leads because "zero files written
/// after N minutes" is the single most actionable fact about a running cook.
///
/// Absent when nothing was sampled: a fabricated zero would read as a
/// measurement and send an operator to kill a healthy run.
fn provider_activity_summary(metadata: &Value) -> Value {
    let Some(activity) = metadata
        .pointer("/cook_progress/activity")
        .filter(|activity| !activity.is_null())
    else {
        return Value::Null;
    };
    let mut summary = activity.clone();
    let Some(object) = summary.as_object_mut() else {
        return Value::Null;
    };
    if let Some(observed_at) = metadata
        .pointer("/cook_progress/activity_observed_at")
        .filter(|observed_at| !observed_at.is_null())
    {
        object.insert("observed_at".to_string(), observed_at.clone());
    }
    if let Ok(activity) =
        serde_json::from_value::<agent_task_service::CookProviderActivity>(activity.clone())
    {
        if let Some(line) = activity.summary_line() {
            object.insert("summary".to_string(), json!(line));
        }
    }
    summary
}

/// Project the durable resource timeline and supervision decisions into the
/// status summary.
///
/// The decisions are reported in full and the timeline is not: a run reaches a
/// handful of decisions and hundreds of samples, and a status summary that
/// inlined the whole timeline would bury the one fact a reader came for. The
/// latest sample plus a count is enough to say "this is holding nine gigabytes
/// and has been sampled forty times"; the full timeline stays in the run record
/// for anyone who wants the curve.
///
/// Absent when nothing was recorded, so a run from before supervision existed
/// does not acquire a misleading empty report.
fn supervision_summary(metadata: &Value) -> Value {
    let timeline = metadata
        .get("cook_resource_timeline")
        .and_then(Value::as_array);
    let events = metadata
        .get("cook_supervision_events")
        .and_then(Value::as_array);
    if timeline.is_none() && events.is_none() {
        return Value::Null;
    }
    json!({
        "resource_samples": timeline.map_or(0, |timeline| timeline.len()),
        "latest_resource_sample": timeline.and_then(|timeline| timeline.last()).cloned(),
        "events": events.cloned().unwrap_or_default(),
        "stopped_by_policy": events.is_some_and(|events| {
            events
                .iter()
                .any(|event| event.get("kind").and_then(Value::as_str) == Some("stop_executed"))
        }),
    })
}

/// Project the durable `provider_executions` metadata into a compact list of the
/// attempts that are still `running`. These are written by
/// `reserve_provider_execution` before the scheduler blocks on the backend and
/// cleared to a terminal state by `record_provider_execution_terminal`, so a
/// running entry is authoritative evidence that a provider is executing right
/// now — the only in-flight signal a local (in-process) cook has (#8396).
fn running_provider_executions(metadata: &Value) -> Vec<Value> {
    metadata
        .get("provider_executions")
        .and_then(Value::as_array)
        .map(|executions| {
            executions
                .iter()
                .filter(|execution| {
                    execution.get("state").and_then(Value::as_str) == Some("running")
                })
                .map(|execution| {
                    json!({
                        "task_id": execution.get("task_id"),
                        "attempt": execution.get("attempt"),
                        "backend": execution.get("backend"),
                        "model": execution.get("model"),
                        "started_at": execution.get("started_at"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
        .take(COMPACT_TASK_LIMIT)
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

#[derive(Clone)]
struct CompactRef {
    task_id: Value,
    kind: Value,
    uri: String,
    is_evidence: bool,
}

fn ref_inventory(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Vec<CompactRef> {
    let mut refs: Vec<CompactRef> = record
        .get("artifact_refs")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let uri = item.get("uri").and_then(Value::as_str)?.trim();
                    (!uri.is_empty()).then(|| CompactRef {
                        task_id: item.get("task_id").cloned().unwrap_or(Value::Null),
                        kind: item.get("kind").cloned().unwrap_or(Value::Null),
                        uri: uri.to_string(),
                        is_evidence: false,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(aggregate) = aggregate {
        for outcome in &aggregate.outcomes {
            for artifact in &outcome.artifacts {
                let uri = artifact
                    .url
                    .as_deref()
                    .or(artifact.path.as_deref())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("artifact:{}:{}", outcome.task_id, artifact.id));
                refs.push(CompactRef {
                    task_id: json!(outcome.task_id),
                    kind: json!(artifact.kind),
                    uri,
                    is_evidence: false,
                });
            }
            for evidence in &outcome.evidence_refs {
                let uri = evidence.uri.trim();
                if uri.is_empty() {
                    continue;
                }
                refs.push(CompactRef {
                    task_id: json!(outcome.task_id),
                    kind: json!(evidence.kind),
                    uri: uri.to_string(),
                    is_evidence: true,
                });
            }
        }
    }

    refs
}

/// Deduped, empty-uri-filtered artifact/evidence refs, capped to keep the
/// recovery summary scannable. The full list remains available via `--full`.
fn compact_refs(refs: &[CompactRef]) -> (Value, usize) {
    let mut seen = std::collections::HashSet::new();
    let mut rendered: Vec<Value> = Vec::new();
    let mut total_valid = 0usize;

    for item in refs {
        if item.uri.trim().is_empty() {
            continue;
        }
        if !seen.insert(item.uri.clone()) {
            continue;
        }
        total_valid += 1;
        if rendered.len() < COMPACT_REF_LIMIT {
            rendered.push(json!({
                "task_id": item.task_id.clone(),
                "kind": item.kind.clone(),
                "uri": item.uri.clone(),
            }));
        }
    }

    let omitted = total_valid.saturating_sub(rendered.len());
    (Value::Array(rendered), omitted)
}

fn work_summary(
    record: &Value,
    aggregate: Option<&AgentTaskAggregate>,
    refs: &[CompactRef],
) -> Value {
    let latest_promotion = record
        .get("metadata")
        .and_then(|metadata| metadata.get("latest_promotion"));
    let promotion_status = latest_promotion
        .and_then(|promotion| promotion.get("status"))
        .and_then(Value::as_str);
    let artifact_ref_count = deduped_ref_count(refs.iter().filter(|item| !item.is_evidence));
    let evidence_ref_count = deduped_ref_count(refs.iter().filter(|item| item.is_evidence));
    let provider_status = provider_execution_status(record, aggregate);
    let committed_changes = provider_committed_changes(record)
        || aggregate.is_some_and(aggregate_has_committed_changes)
        || latest_promotion.is_some_and(promotion_reports_committed_changes);
    let canonical = classify_candidates(&candidate_result_payload(record, aggregate));
    let classification = work_classification(
        record,
        promotion_status,
        artifact_ref_count,
        evidence_ref_count,
        committed_changes,
        canonical.state(),
    );

    json!({
        "classification": classification,
        "candidate_state": canonical.state().as_str(),
        "candidate_counts": {
            "patch_available": canonical.available,
            "empty": canonical.empty,
            "missing": canonical.missing,
            "unreadable": canonical.unreadable,
            "conflicting": canonical.conflicting,
            "retained_only": canonical.retained_only,
            "unknown": canonical.unknown,
        },
        "candidate_scan": {
            "attempts_omitted": canonical.attempts_omitted,
            "outcomes_omitted": canonical.outcomes_omitted,
            "artifacts_omitted": canonical.artifacts_omitted,
            "degraded": canonical.is_degraded(),
        },
        "provider_execution_status": provider_status,
        "promotion_status": promotion_status,
        "artifact_ref_count": artifact_ref_count,
        "evidence_ref_count": evidence_ref_count,
        "committed_changes_detected": committed_changes,
        "artifact_command": record.get("run_id").and_then(Value::as_str).map(|run_id| format!("homeboy agent-task artifacts {run_id}")),
    })
}

/// Combine the current record with the immutable aggregate before projecting a
/// candidate result. Promotion, finalization, and adoption facts live on the
/// record while patch facts live in the aggregate; neither may erase the other.
fn candidate_result_payload(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Value {
    let mut payload = record.clone();
    if let Some(aggregate) = aggregate {
        payload["aggregate"] = serde_json::to_value(aggregate).unwrap_or(Value::Null);
    }
    payload
}

fn deduped_ref_count<'a>(refs: impl Iterator<Item = &'a CompactRef>) -> usize {
    refs.filter(|item| !item.uri.trim().is_empty())
        .map(|item| item.uri.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn provider_execution_status(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Value {
    if let Some(aggregate) = aggregate {
        let mut statuses: Vec<String> = aggregate
            .outcomes
            .iter()
            .map(|outcome| format!("{:?}", outcome.status).to_ascii_lowercase())
            .collect();
        statuses.sort();
        statuses.dedup();
        if statuses.len() == 1 {
            return json!(statuses[0]);
        }
        if !statuses.is_empty() {
            return json!(statuses);
        }
    }
    record.get("state").cloned().unwrap_or(Value::Null)
}

fn work_classification(
    record: &Value,
    promotion_status: Option<&str>,
    artifact_ref_count: usize,
    evidence_ref_count: usize,
    committed_changes: bool,
    candidate_state: CandidateState,
) -> &'static str {
    if committed_changes && promotion_status.is_some_and(is_no_change_promotion_status) {
        return "committed_changes_pending_promotion";
    }
    match candidate_state {
        CandidateState::Finalized => return "pull_request_finalized",
        CandidateState::Promoted => return "promoted_changes",
        CandidateState::PatchAvailable => return "provider_completed_patch_available",
        _ => {}
    }
    if promotion_status.is_some_and(is_no_change_promotion_status) {
        if artifact_ref_count == 0 && evidence_ref_count == 0 {
            return "no_changes";
        }
        return "provider_completed_artifacts_pending_review";
    }
    match promotion_status {
        Some("applied") => return "promoted_changes",
        Some("gate_failed") => return "promoted_changes_gate_failed",
        Some("dry_run") => return "promotion_dry_run",
        _ => {}
    }
    if artifact_ref_count > 0 || evidence_ref_count > 0 {
        return "provider_completed_artifacts_available";
    }
    if record.get("state").and_then(Value::as_str) == Some("succeeded") {
        return "no_changes";
    }
    "unknown"
}

fn is_no_change_promotion_status(status: &str) -> bool {
    matches!(status, "no_changes" | "no_patch_produced")
}

fn provider_committed_changes(record: &Value) -> bool {
    value_reports_committed_changes(record)
}

fn aggregate_has_committed_changes(aggregate: &AgentTaskAggregate) -> bool {
    aggregate.outcomes.iter().any(|outcome| {
        value_reports_committed_changes(&outcome.outputs)
            || value_reports_committed_changes(&outcome.metadata)
            || outcome.artifacts.iter().any(|artifact| {
                value_reports_committed_changes(&artifact.metadata)
                    || artifact
                        .metadata
                        .get("changed_files")
                        .and_then(Value::as_array)
                        .is_some_and(|files| !files.is_empty())
            })
    })
}

fn promotion_reports_committed_changes(promotion: &Value) -> bool {
    value_reports_committed_changes(promotion)
        || promotion
            .get("changed_files")
            .and_then(Value::as_array)
            .is_some_and(|files| !files.is_empty())
}

fn value_reports_committed_changes(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("committed_changes").and_then(Value::as_bool) == Some(true)
                || map
                    .get("commits")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
                || map
                    .get("commit_shas")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
                || map
                    .get("provider_commits")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
                || map.values().any(value_reports_committed_changes)
        }
        Value::Array(items) => items.iter().any(value_reports_committed_changes),
        _ => false,
    }
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

fn collected_diagnostic_value(item: CollectedDiagnostic) -> Value {
    collected_diagnostic_value_with_details(item, false)
}

fn collected_diagnostic_value_with_details(
    item: CollectedDiagnostic,
    include_details: bool,
) -> Value {
    let owner = diagnostic_owner(&item.class, &item.source);
    let mut value = json!({
        "task_id": item.task_id,
        "class": item.class,
        "message": bounded_diagnostic_value(&Value::String(item.message)),
        "source": item.source,
        "owner": owner,
    });
    if include_details && !item.data.is_null() {
        value["details"] = bounded_diagnostic_value(&item.data);
    } else if let Some(details) = policy_denial_details(&item.data) {
        value["details"] = details;
    }
    if let Some(field) = item.data.get("field").filter(|field| !field.is_null()) {
        value["field"] = bounded_diagnostic_value(field);
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
    let details = fields.into_iter().filter_map(|field| {
        data.get(field)
            .filter(|value| !value.is_null())
            .map(|value| (field.to_string(), bounded_diagnostic_value(value)))
    });
    let details = serde_json::Map::from_iter(details);
    (!details.is_empty()).then(|| Value::Object(details))
}

fn bounded_diagnostic_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > COMPACT_TEXT_LIMIT => {
            let prefix: String = text.chars().take(COMPACT_TEXT_LIMIT).collect();
            Value::String(format!("{prefix}..."))
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .take(COMPACT_REF_LIMIT)
                .map(bounded_diagnostic_value)
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .take(COMPACT_REF_LIMIT)
                .map(|(key, value)| (key.clone(), bounded_diagnostic_value(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn diagnostic_owner(class: &str, source: &str) -> &'static str {
    let class = class.to_ascii_lowercase();
    if source == "hydrated_process_stream" {
        "provider_runtime"
    } else if class.contains("malformed") || class.contains("normalization") {
        "executor_wrapper"
    } else if class.contains("provider") || class.contains("runtime") {
        "provider_runtime"
    } else {
        "agent_task"
    }
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
    action: Option<CommandNextAction>,
    continuation: Option<CommandNextAction>,
}

impl RetryReplayAction {
    fn unavailable(owner: Value, reason: impl Into<String>) -> Self {
        Self {
            owner,
            readiness: "unavailable",
            reason: Some(reason.into()),
            action: None,
            continuation: None,
        }
    }

    fn projection(&self) -> Value {
        json!({
            "owner": self.owner,
            "readiness": self.readiness,
            "reason": self.reason,
            "action": self.action.as_ref().and_then(|action| action.action.clone()),
        })
    }
}

/// Render retry only from the durable owner and replay contract. The command and
/// typed action share this one admission decision, so a human hint cannot claim
/// a retry exists when route materialization will reject it.
fn retry_replay_action(record: &AgentTaskRunRecord) -> RetryReplayAction {
    let runner_id = record.runner_id().filter(|id| !id.trim().is_empty());
    let owner = match runner_id {
        Some(runner_id) => json!({ "placement": "runner", "runner_id": runner_id }),
        None => json!({ "placement": "local" }),
    };
    let plan = match agent_task_lifecycle::load_plan(&record.run_id) {
        Ok(plan) => plan,
        Err(error) => {
            return RetryReplayAction::unavailable(
                owner,
                format!("persisted replay plan is unavailable: {}", error.message),
            );
        }
    };
    match cook_continuation_action(record) {
        Ok(true) => {
            return RetryReplayAction {
                owner,
                readiness: "unavailable",
                reason: Some(
                    "the authenticated review-form continuation must resume through Cook"
                        .to_string(),
                ),
                action: None,
                continuation: Some(cook_continuation_command(&record.run_id)),
            };
        }
        Ok(false) => {}
        Err(error) => {
            return RetryReplayAction::unavailable(
                owner,
                format!(
                    "cannot validate whether this run requires Cook continuation: {}",
                    error.message
                ),
            );
        }
    }
    if !plan_has_retry_materialization_identity(&plan) {
        return RetryReplayAction::unavailable(
            owner,
            "persisted replay plan has no materialization identity",
        );
    }
    if let Some(replay) = plan.metadata.get("generic_lab_command_replay") {
        let root = replay
            .pointer("/materialization/canonical_root")
            .and_then(Value::as_str);
        let expected_identity = replay
            .pointer("/materialization/content_identity")
            .and_then(Value::as_str);
        let (Some(root), Some(expected_identity)) = (root, expected_identity) else {
            return RetryReplayAction::unavailable(
                owner,
                "generic Lab replay is missing its materialization identity",
            );
        };
        match homeboy::runner::controller_workspace_materialization_identity(std::path::Path::new(
            root,
        )) {
            Ok(actual_identity) if actual_identity == expected_identity => {}
            Ok(_) => {
                return RetryReplayAction::unavailable(
                    owner,
                    "source workspace no longer matches the persisted Lab replay identity",
                );
            }
            Err(error) => {
                return RetryReplayAction::unavailable(
                    owner,
                    format!(
                        "cannot validate persisted Lab replay identity: {}",
                        error.message
                    ),
                );
            }
        }
    }

    RetryReplayAction {
        owner: owner.clone(),
        readiness: "ready",
        reason: None,
        action: Some(owner_bound_retry_action(&record.run_id, runner_id, owner)),
        continuation: None,
    }
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
        format!("homeboy agent-task cook-continue {}", quote_arg(run_id)),
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
) -> Vec<String> {
    let owner = record
        .runner_id()
        .map(|runner| format!("--runner {runner}"))
        .unwrap_or_else(|| "--placement local".to_string());
    let run_id = &record.run_id;
    let mut commands = vec![
        format!("homeboy {owner} agent-task status {run_id} --full"),
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

        assert_eq!(
            value["details"]["canonical_path"]
                .as_str()
                .expect("bounded path")
                .chars()
                .count(),
            COMPACT_TEXT_LIMIT + 3
        );
    }

    #[test]
    fn successful_status_reads_only_use_subject_exit_in_compatibility_mode() {
        let actionable = json!({ "cook": { "phase": "terminal", "publication": "blocked" } });

        assert_eq!(subject_exit_code(&actionable, false), 0);
        assert_eq!(subject_exit_code(&actionable, true), 1);
    }

    #[test]
    fn discovery_actionable_metadata_is_bounded_and_continues_active_pages() {
        let mut report = json!({
            "limit": 20,
            "next_cursor": 20,
            "liveness_summary": { "stale": 1, "suspect": 0, "unreconciled": 0 },
            "runs": (0..20).map(|index| json!({
                "run_id": format!("run-{index}"),
                "commands": {
                    "status": format!("homeboy agent-task status run-{index}"),
                    "logs": format!("homeboy agent-task logs run-{index}"),
                    "artifacts": format!("homeboy agent-task artifacts run-{index}"),
                    "reconcile": format!("homeboy agent-task reconcile run-{index} --dry-run"),
                },
                "liveness": "stale",
            })).collect::<Vec<_>>(),
        });

        attach_agent_task_discovery_actionable(&mut report, Some("homeboy agent-task active"));

        let actions = report[ACTIONABLE_METADATA_KEY]["next_actions"]
            .as_array()
            .expect("next actions");
        assert_eq!(actions.len(), DISCOVERY_NEXT_ACTION_LIMIT);
        assert_eq!(
            actions[0]["command"],
            "homeboy agent-task active --reconcile --dry-run"
        );
        assert_eq!(
            actions[1]["command"],
            "homeboy agent-task active --limit 20 --cursor 20"
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

        let commands = diagnose_next_commands(&record, None, None);

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
            action: None,
            continuation: Some(
                CommandNextAction::new(
                    "resume the authenticated Cook continuation",
                    "homeboy agent-task cook-continue review-form-timeout",
                )
                .with_kind(CommandNextActionKind::Repair),
            ),
        };

        let commands =
            diagnose_next_commands(&record, retry.action.as_ref(), retry.continuation.as_ref());

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

        assert_eq!(summary["latest_promotion"]["status"], "applied");
        assert!(summary["latest_promotion"]
            .get("operator_notification")
            .is_none());
        assert_eq!(
            summary["queue_visibility"]["commands"][0],
            "homeboy agent-task list"
        );
        assert_eq!(summary["liveness"]["status"], "terminal");
        assert_eq!(
            summary["liveness"]["next_action"],
            "homeboy agent-task status <run-id> --full"
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

        let stale = compact_status_summary(
            &json!({
                "run_id": "agent-task-ghost",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "lifecycle": { "heartbeat": { "last_seen_at": "2026-07-12T23:28:28Z" } },
                "metadata": {
                    "stale_running": true,
                    "stale_running_reason": "runner_job_unverified_after_daemon_restart"
                }
            }),
            "agent-task-ghost",
        );
        assert_eq!(stale["liveness"]["status"], "stale");
        assert_eq!(stale["liveness"]["provider_boundary"]["status"], "absent");
        assert_eq!(
            stale["liveness"]["next_action"],
            "homeboy agent-task reconcile agent-task-ghost --dry-run"
        );

        let accepted_handoff = compact_status_summary(
            &json!({
                "run_id": "agent-task-run-bound",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "metadata": {
                    "runner_id": "homeboy-lab",
                    "runner_job_id": "accepted-daemon-job"
                }
            }),
            "agent-task-run-bound",
        );
        assert_eq!(
            accepted_handoff["liveness"]["provider_boundary"]["status"],
            "recorded"
        );
        assert_eq!(
            accepted_handoff["liveness"]["provider_boundary"]["runner_job_id"],
            "accepted-daemon-job"
        );

        let supervised = compact_status_summary(
            &json!({
                "run_id": "cook-attempt-1",
                "state": "running",
                "tasks": [],
                "metadata": {
                    "local_cook_supervisor": {
                        "job_id": "controller-job-1",
                        "reattach_command": "homeboy agent-task status cook-1 --full"
                    }
                }
            }),
            "cook-attempt-1",
        );
        assert_eq!(
            supervised["liveness"]["local_cook_supervisor"]["job_id"],
            "controller-job-1"
        );
        assert_eq!(
            supervised["liveness"]["local_cook_supervisor"]["reattach_command"],
            "homeboy agent-task status cook-1 --full"
        );
    }

    #[test]
    fn compact_status_surfaces_actionable_notification_delivery_without_destination() {
        let summary = compact_status_summary(
            &json!({
                "run_id": "cook-attempt-1",
                "state": "failed",
                "tasks": [],
                "notification_delivery": {
                    "schema": "homeboy/cook-notification-delivery/v1",
                    "cook_id": "cook-1",
                    "event_id": "terminal",
                    "event_kind": "needs_attention",
                    "transport": "generic.transport",
                    "route_classification": "explicit",
                    "status": "failed",
                    "error_class": "transport_spawn_failed",
                    "retry_command": "homeboy agent-task cook --continue cook-1",
                    "raw_destination": "must-not-appear"
                }
            }),
            "cook-attempt-1",
        );

        assert_eq!(summary["notification_delivery"]["status"], "failed");
        assert_eq!(
            summary["notification_delivery"]["retry_command"],
            "homeboy agent-task cook --continue cook-1"
        );
        assert!(summary["notification_delivery"]
            .get("raw_destination")
            .is_none());
    }

    #[test]
    fn compact_status_answers_what_the_provider_is_doing_right_now() {
        // #11482: this is the question `agent-task status` could not answer, so
        // diagnosing a stalled cook meant `ps aux | grep` and `git status` on a
        // path the operator had to already know.
        let summary = compact_status_summary(
            &json!({
                "run_id": "agent-task-working",
                "state": "running",
                "tasks": [],
                "metadata": {
                    "cook_progress": {
                        "phase": "heartbeat",
                        "attempt": 1,
                        "detail": "provider execution is still running",
                        "activity": {
                            "worktree_root": "/tmp/wt/11482",
                            "files_changed": 0,
                            "command": "cargo test -q -p homeboy-agents",
                            "command_elapsed_seconds": 372,
                            "elapsed_seconds": 400
                        },
                        "activity_observed_at": "2026-08-04T17:16:58Z"
                    }
                }
            }),
            "agent-task-working",
        );

        let activity = &summary["liveness"]["provider_activity"];
        assert_eq!(activity["files_changed"], 0);
        assert_eq!(activity["command"], "cargo test -q -p homeboy-agents");
        assert_eq!(activity["observed_at"], "2026-08-04T17:16:58Z");
        let rendered = activity["summary"].as_str().expect("rendered summary");
        assert!(rendered.contains("no files written yet"));
        assert!(rendered.contains("6m12s in `cargo test -q -p homeboy-agents`"));
    }

    #[test]
    fn compact_status_reports_no_provider_activity_rather_than_a_fabricated_zero() {
        // An unsampled cook must not read as "the agent has written nothing" —
        // that is the signal an operator kills a run on.
        let summary = compact_status_summary(
            &json!({
                "run_id": "agent-task-unsampled",
                "state": "running",
                "tasks": [],
                "metadata": { "cook_progress": { "phase": "provider_start", "attempt": 1 } }
            }),
            "agent-task-unsampled",
        );

        assert!(summary["liveness"]["provider_activity"].is_null());
        // A run recorded before supervision existed must not acquire an empty
        // supervision report that reads as "supervised, nothing to say".
        assert!(summary["liveness"]["supervision"].is_null());
    }

    #[test]
    fn compact_status_reports_why_a_run_was_stopped_by_policy() {
        // #7015: the resource cost of a session used to be discoverable only by
        // having watched it. The decision that ended a run has to survive in the
        // status envelope, and it must not be buried under the sample stream.
        let summary = compact_status_summary(
            &json!({
                "run_id": "agent-task-supervised",
                "state": "failed",
                "tasks": [],
                "metadata": {
                    "cook_resource_timeline": [
                        { "at": "2026-08-06T00:00:00Z", "attempt": 1,
                          "sample": { "rss_mib": 4096, "child_processes": 12 } },
                        { "at": "2026-08-06T00:00:15Z", "attempt": 1,
                          "sample": { "rss_mib": 10_500, "child_processes": 41 } }
                    ],
                    "cook_supervision_events": [
                        { "kind": "budget_breached", "attempt": 1, "decision": {
                            "metric": "rss_mib", "action": "stop",
                            "limit": 10_240, "observed": 10_500,
                            "reason": "15Gi box", "remediation": "narrow the task"
                        }},
                        { "kind": "stop_executed", "attempt": 1,
                          "outcome": { "status": "terminated", "signal": "SIGTERM" } }
                    ]
                }
            }),
            "agent-task-supervised",
        );

        let supervision = &summary["liveness"]["supervision"];
        assert_eq!(supervision["resource_samples"], 2);
        assert_eq!(
            supervision["latest_resource_sample"]["sample"]["rss_mib"],
            10_500
        );
        assert_eq!(supervision["events"][0]["decision"]["action"], "stop");
        assert_eq!(supervision["stopped_by_policy"], true);
    }

    #[test]
    fn compact_status_envelope_preserves_candidate_lifecycle_parity() {
        let promoted_record = json!({
            "run_id": "agent-task-promoted",
            "state": "succeeded",
            "tasks": [],
            "metadata": { "latest_promotion": {
                "status": "applied", "patch_artifact": { "id": "patch" }
            }}
        });
        let promoted = compact_status_summary(&promoted_record, "agent-task-promoted");
        assert_eq!(promoted["latest_promotion"]["status"], "applied");
        assert_eq!(
            classify_candidates(&promoted).state(),
            CandidateState::Promoted
        );

        let finalized_record = json!({
            "run_id": "agent-task-finalized",
            "state": "succeeded",
            "tasks": [],
            "metadata": { "cook_finalization": {
                "status": "review_ready", "pr_url": "https://example.test/pull/1"
            }}
        });
        let finalized = compact_status_summary(&finalized_record, "agent-task-finalized");
        assert_eq!(finalized["cook_finalization"]["status"], "review_ready");
        assert_eq!(
            classify_candidates(&finalized).state(),
            CandidateState::Finalized
        );
    }

    #[test]
    fn default_status_projects_durable_patch_candidate_for_rendering_and_actions() {
        let record = json!({
            "run_id": "agent-task-durable-patch",
            "state": "succeeded",
            "tasks": [],
        });
        let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "plan-durable-patch",
            "status": "succeeded",
            "totals": {
                "queued": 0, "running": 0, "blocked": 0, "skipped": 0,
                "succeeded": 1, "failed": 0, "cancelled": 0, "timed_out": 0
            },
            "outcomes": [{
                "schema": "homeboy/agent-task-outcome/v1",
                "task_id": "cook",
                "status": "succeeded",
                "summary": "patch ready",
                "failure_classification": null,
                "artifacts": [{
                    "schema": "homeboy/agent-task-artifact/v1",
                    "id": "durable-patch",
                    "kind": "patch",
                    "size_bytes": 32_318,
                    "metadata": { "changed_file_count": 7 }
                }],
                "evidence_refs": [],
                "metadata": {},
                "outputs": {}
            }]
        }))
        .expect("durable aggregate");

        let mut compact = compact_status_summary_with_aggregate(
            &record,
            "agent-task-durable-patch",
            Some(&aggregate),
        );
        assert_eq!(
            compact["canonical_candidate"]["schema"],
            "homeboy/agent-task-candidate/v1"
        );
        assert_eq!(compact["canonical_candidate"]["state"], "patch_available");
        assert_eq!(compact["canonical_candidate"]["diff_bytes"], 32_318);
        assert_eq!(compact["canonical_candidate"]["scan"]["degraded"], false);
        assert!(compact.get("aggregate").is_none());

        attach_agent_task_status_actionable(&mut compact, "agent-task-durable-patch");
        let rendered = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
            &compact,
        )
        .expect("status summary");

        assert!(rendered.contains("Candidate state: patch_available"));
        assert!(rendered.contains("Patch candidates: 1 non-empty / 0 empty"));
        assert!(!rendered.contains("Patch candidates: 1 non-empty / 0 empty /"));
        assert!(rendered.contains("Diff bytes: 32318"));
        assert!(rendered.contains("Changed files: unknown"));
        assert!(rendered.contains("Next: homeboy agent-task review agent-task-durable-patch"));
        assert!(compact[ACTIONABLE_METADATA_KEY]["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .any(
                |action| action["command"] == "homeboy agent-task review agent-task-durable-patch"
            ));

        let mut full = record.clone();
        attach_full_status_candidate(&mut full, Some(&aggregate), "agent-task-durable-patch");
        attach_agent_task_status_actionable(&mut full, "agent-task-durable-patch");
        let full_rendered = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
            &full,
        )
        .expect("full status summary");

        assert_eq!(
            full["aggregate"]["outcomes"][0]["artifacts"][0]["size_bytes"],
            32_318
        );
        assert_eq!(full["canonical_candidate"], compact["canonical_candidate"]);
        assert_eq!(
            compact["liveness"]["next_action"],
            "homeboy agent-task review agent-task-durable-patch"
        );
        assert_eq!(full["liveness"], compact["liveness"]);
        assert!(full_rendered.contains("Subject state: succeeded"));
        assert!(full_rendered.contains("Patch candidates: 1 non-empty / 0 empty"));
        assert!(full_rendered.contains("Changed files: 7"));
        assert!(!full_rendered.contains("no_patch_produced"));
        assert!(full_rendered.contains("Next: homeboy agent-task review agent-task-durable-patch"));
        assert!(full[ACTIONABLE_METADATA_KEY]["next_actions"]
            .as_array()
            .expect("full next actions")
            .iter()
            .any(
                |action| action["command"] == "homeboy agent-task review agent-task-durable-patch"
            ));
    }

    #[test]
    fn terminal_cook_gate_failure_overrides_successful_provider_status() {
        let mut status = blocked_cook_status_fixture("gate_failed", "gate_failed", "not_attempted");

        project_owning_cook_terminal_status(&mut status);
        let mut status = compact_status_summary(&status, "cook-attempt-1");
        attach_agent_task_status_actionable(&mut status, "cook-attempt-1");
        let rendered = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
            &status,
        )
        .expect("status summary");

        assert_eq!(status["state"], "gate_failed");
        assert_eq!(status["child_run_state"], "succeeded");
        assert_eq!(
            status["execution_states"]["provider"][0]["state"],
            "succeeded"
        );
        assert_eq!(status["execution_states"]["gate"]["state"], "failed");
        assert_eq!(status["cook"]["publication"], "blocked");
        assert_eq!(subject_exit_code(&status, true), 1);
        assert!(rendered.contains("Subject state: gate_failed"));
        assert!(rendered.contains("Candidate state: promoted_gate_failed"));
        assert!(rendered.contains("Cook: gate_failed (publication blocked)"));
        assert!(rendered.contains("Next: homeboy agent-task diagnose cook-attempt-1 --full"));
        assert!(!rendered.contains("Next: homeboy agent-task review"));
        assert!(
            status[ACTIONABLE_METADATA_KEY]["next_actions"]
                .as_array()
                .expect("next actions")
                .iter()
                .any(|action| action["command"]
                    == "homeboy agent-task diagnose cook-attempt-1 --full")
        );
        assert!(!status[ACTIONABLE_METADATA_KEY]["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .any(|action| action["command"] == "homeboy agent-task review cook-attempt-1"));
    }

    #[test]
    fn terminal_cook_finalization_failure_overrides_successful_provider_status() {
        let mut status =
            blocked_cook_status_fixture("finalization_failed", "applied", "finalization_failed");

        project_owning_cook_terminal_status(&mut status);
        attach_agent_task_status_actionable(&mut status, "cook-attempt-1");

        assert_eq!(status["state"], "finalization_failed");
        assert_eq!(status["child_run_state"], "succeeded");
        assert_eq!(
            status["execution_states"]["provider"][0]["state"],
            "succeeded"
        );
        assert_eq!(status["execution_states"]["promotion"]["state"], "applied");
        assert_eq!(
            status["execution_states"]["finalization"]["state"],
            "finalization_failed"
        );
        assert_eq!(subject_exit_code(&status, true), 1);
        assert!(
            status[ACTIONABLE_METADATA_KEY]["next_actions"]
                .as_array()
                .expect("next actions")
                .iter()
                .any(|action| action["command"]
                    == "homeboy agent-task diagnose cook-attempt-1 --full")
        );
    }

    #[test]
    fn terminal_cook_budget_exhaustion_overrides_successful_provider_status() {
        let mut status = blocked_cook_status_fixture(
            "execution_budget_exhausted",
            "gate_failed",
            "not_attempted",
        );

        project_owning_cook_terminal_status(&mut status);
        let mut status = compact_status_summary(&status, "cook-attempt-1");
        attach_agent_task_status_actionable(&mut status, "cook-attempt-1");

        assert_eq!(status["state"], "execution_budget_exhausted");
        assert_eq!(status["child_run_state"], "succeeded");
        assert_eq!(
            status["execution_states"]["provider"][0]["state"],
            "succeeded"
        );
        assert_eq!(status["execution_states"]["gate"]["state"], "failed");
        assert_eq!(subject_exit_code(&status, true), 1);
        assert!(
            status[ACTIONABLE_METADATA_KEY]["next_actions"]
                .as_array()
                .expect("next actions")
                .iter()
                .any(|action| action["command"]
                    == "homeboy agent-task diagnose cook-attempt-1 --full")
        );
    }

    #[test]
    fn successful_terminal_cook_result_projects_its_declared_cook_status() {
        let mut status = json!({
            "run_id": "cook-attempt-1",
            "state": "succeeded",
            "metadata": {
                "cook_progress": {
                    "phase": "terminal",
                    "detail": "future_success_status",
                    "terminal_success": true,
                    "exit_code": 0
                }
            }
        });

        project_owning_cook_terminal_status(&mut status);

        assert_eq!(status["state"], "future_success_status");
        assert_eq!(status["child_run_state"], "succeeded");
        assert_eq!(status["cook"]["publication"], "pending");
        assert_eq!(subject_exit_code(&status, true), 1);
    }

    #[test]
    fn green_and_review_ready_terminal_cooks_keep_successful_status_actions() {
        for terminal_status in ["green_no_finalize", "review_ready"] {
            let mut status = json!({
                "run_id": "cook-attempt-1",
                "state": "succeeded",
                "tasks": [],
                "metadata": {
                    "cook_progress": {
                        "phase": "terminal",
                        "detail": terminal_status,
                        "terminal_success": true,
                        "exit_code": 0
                    },
                    "latest_promotion": {
                        "status": "applied",
                        "patch_artifact": { "id": "patch" }
                    },
                    "cook_finalization": { "status": "review_ready" }
                }
            });

            project_owning_cook_terminal_status(&mut status);
            let mut status = compact_status_summary(&status, "cook-attempt-1");
            attach_agent_task_status_actionable(&mut status, "cook-attempt-1");
            let rendered = crate::commands::agent_task_summary::render_agent_task_summary(
                crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
                &status,
            )
            .expect("status summary");

            assert_eq!(status["state"], terminal_status, "{terminal_status}");
            assert_eq!(status["child_run_state"], "succeeded", "{terminal_status}");
            assert_eq!(
                status["cook"]["publication"], "completed",
                "{terminal_status}"
            );
            assert_eq!(subject_exit_code(&status, true), 0, "{terminal_status}");
            assert!(rendered.contains("Next: homeboy agent-task review cook-attempt-1"));
            assert!(status[ACTIONABLE_METADATA_KEY]["next_actions"]
                .as_array()
                .expect("next actions")
                .iter()
                .any(|action| action["command"] == "homeboy agent-task review cook-attempt-1"));
        }
    }

    fn blocked_cook_status_fixture(
        cook_status: &str,
        promotion_status: &str,
        finalization_status: &str,
    ) -> Value {
        json!({
            "run_id": "cook-attempt-1",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "metadata": {
                "cook_progress": {
                    "phase": "terminal",
                    "detail": cook_status,
                    "terminal_success": false,
                    "exit_code": 1
                },
                "latest_promotion": {
                    "status": promotion_status,
                    "patch_artifact": { "id": "patch" }
                },
                "cook_finalization": { "status": finalization_status }
            },
            "execution_states": {
                "provider": [{ "task_id": "cook", "state": "succeeded" }],
                "gate": { "state": if promotion_status == "gate_failed" { "failed" } else { "passed" } },
                "promotion": { "state": promotion_status },
                "finalization": { "state": finalization_status }
            }
        })
    }

    #[test]
    fn cook_lifecycle_matrix_keeps_provider_patch_gate_and_finalization_truth_separate() {
        let cases = [
            (
                "green",
                "review_ready",
                "applied",
                "review_ready",
                "review_ready",
                "promoted",
                "passed",
                "completed",
                "completed",
            ),
            (
                "gate failed",
                "review_ready",
                "gate_failed",
                "not_attempted",
                "gate_failed",
                "promoted_gate_failed",
                "failed",
                "not_attempted",
                "blocked",
            ),
            (
                "accepted baseline red",
                "review_ready",
                "gate_failed",
                "review_ready",
                "review_ready",
                "promoted_accepted_inherited_failure",
                "accepted_inherited_failure",
                "completed",
                "completed",
            ),
            (
                "finalization failed",
                "review_ready",
                "applied",
                "finalization_failed",
                "finalization_failed",
                "promoted_finalization_failed",
                "passed",
                "finalization_failed",
                "blocked",
            ),
            (
                "finalization pending",
                "review_ready",
                "applied",
                "pending",
                "finalization_pending",
                "promoted_finalization_pending",
                "passed",
                "finalization_pending",
                "pending",
            ),
        ];

        for (
            name,
            detail,
            promotion,
            finalization,
            expected_status,
            candidate,
            gate,
            expected_finalization,
            publication,
        ) in cases
        {
            let mut status = blocked_cook_status_fixture(detail, promotion, finalization);
            if name == "accepted baseline red" {
                status["metadata"]["latest_promotion"] = accepted_baseline_red_promotion();
            }
            status["metadata"]["cook_progress"]["terminal_success"] = Value::Bool(true);
            project_owning_cook_terminal_status(&mut status);

            assert_eq!(status["state"], expected_status, "{name}");
            assert_eq!(status["child_run_state"], "succeeded", "{name}");
            assert_eq!(status["tasks"][0]["state"], expected_status, "{name}");
            assert_eq!(status["tasks"][0]["provider_state"], "succeeded", "{name}");
            assert_eq!(
                status["execution_states"]["provider"][0]["state"], "succeeded",
                "{name}"
            );
            assert_eq!(
                status["execution_states"]["candidate"]["state"], candidate,
                "{name}"
            );
            assert_eq!(status["execution_states"]["gate"]["state"], gate, "{name}");
            assert_eq!(
                status["execution_states"]["promotion"]["state"], promotion,
                "{name}"
            );
            assert_eq!(
                status["execution_states"]["finalization"]["state"], expected_finalization,
                "{name}"
            );
            assert_eq!(status["cook"]["publication"], publication, "{name}");
            assert_eq!(
                subject_exit_code(&status, true),
                if publication == "completed" { 0 } else { 1 },
                "{name}"
            );

            let mut compact = compact_status_summary(&status, "cook-attempt-1");
            attach_agent_task_status_actionable(&mut compact, "cook-attempt-1");
            let rendered = crate::commands::agent_task_summary::render_agent_task_summary(
                crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
                &compact,
            )
            .expect("status summary");
            let review_command = "homeboy agent-task review cook-attempt-1";
            assert_eq!(
                rendered.contains(&format!("Next: {review_command}")),
                publication == "completed",
                "{name}"
            );
            assert_eq!(
                compact[ACTIONABLE_METADATA_KEY]["next_actions"]
                    .as_array()
                    .expect("next actions")
                    .iter()
                    .any(|action| action["command"] == review_command),
                publication == "completed",
                "{name}"
            );
        }
    }

    fn accepted_baseline_red_promotion() -> Value {
        json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "gate_failed",
            "source": { "kind": "aggregate", "task_id": "cook" },
            "to_worktree": "fixture",
            "target": { "worktree": "fixture" },
            "patch_artifact": { "id": "patch", "kind": "patch", "path": "patch.diff" },
            "deterministic_gates": [{
                "schema": "homeboy/agent-task-gate-report/v1",
                "id": "required-gate",
                "status": "accepted_inherited_failure",
                "command": ["cargo", "test"],
                "exit_code": 1,
                "baseline_comparison": {
                    "base_ref": "main",
                    "exit_code": 1,
                    "failure_fingerprint": "same-failure",
                    "matches_candidate_failure": true,
                    "result": "baseline_red"
                }
            }],
            "operator_notification": { "status": "completed", "message": "accepted baseline red" }
        })
    }

    #[test]
    fn local_cook_surfaces_in_flight_provider_execution() {
        // A local cook has no provider handles and no runner job id, but
        // `reserve_provider_execution` durably records the running attempt
        // before the backend blocks. The liveness projection must report the
        // provider boundary as recorded (not absent) and expose the running
        // execution's backend, model, and start time so operators can tell an
        // active provider from a hung preflight (#8396).
        let local_running = compact_status_summary(
            &json!({
                "run_id": "agent-task-local-1",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "lifecycle": { "heartbeat": { "last_seen_at": "2026-07-16T00:00:00Z" } },
                "metadata": {
                    "provider_executions_consumed": 1,
                    "provider_executions": [{
                        "key": "cook:1",
                        "task_id": "cook",
                        "attempt": 1,
                        "backend": "opencode",
                        "model": "openai/gpt-5.6-sol",
                        "state": "running",
                        "started_at": "2026-07-16T00:00:00Z"
                    }]
                }
            }),
            "agent-task-local-1",
        );
        let boundary = &local_running["liveness"]["provider_boundary"];
        assert_eq!(boundary["status"], "recorded");
        assert_eq!(boundary["provider_handle_count"], 0);
        assert!(boundary["runner_job_id"].is_null());
        assert_eq!(boundary["active_execution_count"], 1);
        assert_eq!(boundary["active_executions"][0]["backend"], "opencode");
        assert_eq!(
            boundary["active_executions"][0]["model"],
            "openai/gpt-5.6-sol"
        );
        assert_eq!(boundary["active_executions"][0]["task_id"], "cook");
        assert_eq!(
            boundary["active_executions"][0]["started_at"],
            "2026-07-16T00:00:00Z"
        );
        assert_eq!(local_running["execution_location"], "local");
    }

    #[test]
    fn terminal_provider_execution_is_not_surfaced_as_active() {
        // Once the provider execution reaches a terminal state, it must no
        // longer count as in-flight. A local run with only completed executions
        // and no other liveness evidence reports the boundary as absent (#8396).
        let completed = compact_status_summary(
            &json!({
                "run_id": "agent-task-local-2",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "metadata": {
                    "provider_executions": [{
                        "key": "cook:1",
                        "task_id": "cook",
                        "attempt": 1,
                        "backend": "opencode",
                        "state": "succeeded",
                        "started_at": "2026-07-16T00:00:00Z",
                        "finished_at": "2026-07-16T00:03:00Z"
                    }]
                }
            }),
            "agent-task-local-2",
        );
        let boundary = &completed["liveness"]["provider_boundary"];
        assert_eq!(boundary["status"], "absent");
        assert_eq!(boundary["active_execution_count"], 0);
        assert!(boundary["active_executions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn every_terminal_run_state_reports_terminal_liveness() {
        // `liveness` used to compare `state` against an inline
        // `"succeeded" | "failed" | "cancelled"` list, so a run that finished in
        // `partial_failure`, `partial_recoverable`, or `candidate_recoverable`
        // reported `status: "active"` forever and an orchestrator polling
        // liveness never saw it finish. Terminality now has exactly one
        // definition — `AgentTaskRunState::is_terminal` — and this pins every
        // member of that set through the same untyped read path.
        for state in [
            "succeeded",
            "candidate_recoverable",
            "partial_recoverable",
            "partial_failure",
            "failed",
            "cancelled",
        ] {
            let summary = liveness_summary(
                &json!({ "run_id": "agent-task-terminal", "state": state, "tasks": [] }),
                "agent-task-terminal",
                CandidateState::Unknown,
            );
            assert_eq!(summary["status"], "terminal", "{state}");
        }

        for state in ["queued", "running"] {
            let summary = liveness_summary(
                &json!({ "run_id": "agent-task-active", "state": state, "tasks": [] }),
                "agent-task-active",
                CandidateState::Unknown,
            );
            assert_eq!(summary["status"], "active", "{state}");
        }

        // A state this build cannot parse keeps the prior non-terminal reading
        // rather than being asserted as finished.
        let unknown = liveness_summary(
            &json!({ "run_id": "agent-task-unknown", "state": "not_a_state", "tasks": [] }),
            "agent-task-unknown",
            CandidateState::Unknown,
        );
        assert_eq!(unknown["status"], "active");
        assert!(!record_state_is_terminal(&json!({ "tasks": [] })));
    }

    #[test]
    fn terminal_cook_pre_execution_failure_keeps_terminal_liveness_after_projection() {
        // v0.335.0 persisted this controller failure as a failed child run.
        // Status replaces that child state with the Cook's terminal detail, an
        // arbitrary controller label that cannot parse as AgentTaskRunState.
        let mut record = json!({
            "run_id": "agent-task-pre-execution-compatibility",
            "state": "failed",
            "tasks": [{ "task_id": "cook", "state": "failed" }],
            "provider_handles": [],
            "metadata": {
                "pre_execution_failure": {
                    "phase": "controller_admission",
                    "provider_executions_consumed": 0
                },
                "cook_progress": {
                    "phase": "terminal",
                    "detail": "pre_execution_failure",
                    "terminal_success": false,
                    "exit_code": 1
                }
            }
        });

        project_owning_cook_terminal_status(&mut record);
        let summary = compact_status_summary(&record, "agent-task-pre-execution-compatibility");

        assert_eq!(summary["state"], "pre_execution_failure");
        assert_eq!(summary["child_run_state"], "failed");
        assert_eq!(summary["liveness"]["status"], "terminal");
        assert_eq!(summary["liveness"]["provider_boundary"]["status"], "absent");
    }

    #[test]
    fn compact_status_retains_recovered_patch_refs_for_summary_rendering() {
        let record = json!({
            "run_id": "recovered-lab-run",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [{
                "task_id": "cook",
                "kind": "patch",
                "uri": "homeboy://agent-task/run/recovered-lab-run/artifacts#task=cook&artifact=patch",
                "size_bytes": 7926
            }]
        });

        let summary = compact_status_summary(&record, "recovered-lab-run");

        assert_eq!(summary["artifact_refs"][0]["kind"], "patch");
        assert!(summary["artifact_refs"][0].get("size_bytes").is_none());
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
        assert_eq!(
            compact["full_command"],
            "homeboy agent-task status run-14 --full"
        );
        assert!(
            compact.get("failure_context").is_none(),
            "a report without failure_context must not grow one"
        );
        assert_eq!(compact_cook_report(report.clone(), true), report);
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
                    { "action": "status", "command": "homeboy agent-task status run-2 --full" },
                    { "action": "diagnose", "command": "homeboy agent-task diagnose run-2" }
                ],
                "next_actions": [
                    { "action": "status", "command": "homeboy agent-task status run-2 --full" }
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
            "homeboy agent-task status run-2 --full"
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

        // The compact text budget still applies to the fields that are kept.
        assert_eq!(
            context["recovery_reason"]
                .as_str()
                .expect("recovery_reason")
                .chars()
                .count(),
            COMPACT_TEXT_LIMIT + 3,
            "long prose must still be truncated to the compact budget"
        );

        assert_eq!(compact_cook_report(report.clone(), true), report);
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

    #[test]
    fn compact_work_summary_separates_no_patch_promotion_from_provider_artifacts() {
        let record = json!({
            "run_id": "agent-task-run-artifacts",
            "state": "succeeded",
            "tasks": [],
            "artifact_refs": [{
                "task_id": "task-a",
                "kind": "provider-transcript",
                "uri": "file:///tmp/provider-transcript.json"
            }],
            "metadata": {
                "latest_promotion": {
                    "status": "no_changes",
                    "operator_notification": { "status": "completed" }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-artifacts");

        assert_eq!(
            summary["work_summary"]["provider_execution_status"],
            "succeeded"
        );
        assert_eq!(summary["work_summary"]["promotion_status"], "no_changes");
        assert_eq!(
            summary["work_summary"]["classification"],
            "provider_completed_artifacts_pending_review"
        );
        assert_eq!(summary["work_summary"]["artifact_ref_count"], 1);
        assert_eq!(
            summary["refs"][0]["uri"],
            "file:///tmp/provider-transcript.json"
        );
    }

    #[test]
    fn compact_work_summary_classifies_provider_commits_pending_promotion() {
        let record = json!({
            "run_id": "agent-task-run-commits",
            "state": "succeeded",
            "tasks": [],
            "metadata": {
                "latest_promotion": {
                    "status": "no_patch_produced",
                    "changed_files": ["src/lib.rs"],
                    "operator_notification": { "status": "completed" }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-commits");

        assert_eq!(
            summary["work_summary"]["classification"],
            "committed_changes_pending_promotion"
        );
        assert_eq!(summary["work_summary"]["committed_changes_detected"], true);
    }

    #[test]
    fn compact_work_summary_preserves_true_no_changes() {
        let record = json!({
            "run_id": "agent-task-run-empty",
            "state": "succeeded",
            "tasks": [],
            "metadata": {
                "latest_promotion": {
                    "status": "no_changes",
                    "operator_notification": { "status": "completed" }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-empty");

        assert_eq!(summary["work_summary"]["classification"], "no_changes");
        assert_eq!(summary["work_summary"]["artifact_ref_count"], 0);
        assert_eq!(summary["work_summary"]["evidence_ref_count"], 0);
    }

    #[test]
    fn work_summary_distinguishes_available_promoted_and_finalized_candidates() {
        let available = work_summary(&json!({ "state": "succeeded" }), None, &[]);
        assert_eq!(available["classification"], "no_changes");

        let promoted = work_summary(
            &json!({
                "state": "succeeded",
                "metadata": { "latest_promotion": {
                    "status": "applied", "patch_artifact": { "id": "patch" }
                }}
            }),
            None,
            &[],
        );
        assert_eq!(promoted["candidate_state"], "promoted");
        assert_eq!(promoted["classification"], "promoted_changes");

        let finalized = work_summary(
            &json!({
                "state": "succeeded",
                "metadata": { "cook_finalization": {
                    "status": "review_ready", "pr_url": "https://example.test/pull/1"
                }}
            }),
            None,
            &[],
        );
        assert_eq!(finalized["candidate_state"], "finalized");
        assert_eq!(finalized["classification"], "pull_request_finalized");
    }

    #[test]
    fn aggregate_artifacts_are_counted_when_record_refs_are_empty() {
        let record = json!({
            "run_id": "agent-task-run-aggregate-refs",
            "state": "succeeded",
            "tasks": []
        });
        let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "plan-a",
            "status": "succeeded",
            "totals": {
                "queued": 0,
                "running": 0,
                "blocked": 0,
                "skipped": 0,
                "succeeded": 1,
                "failed": 0,
                "cancelled": 0,
                "timed_out": 0
            },
            "outcomes": [{
                "schema": "homeboy/agent-task-outcome/v1",
                "task_id": "task-a",
                "status": "succeeded",
                "summary": "ok",
                "failure_classification": null,
                "artifacts": [{
                    "id": "patch.diff",
                    "kind": "patch",
                    "path": "/tmp/patch.diff",
                    "metadata": null
                }],
                "typed_artifacts": [],
                "evidence_refs": [{
                    "kind": "executor-result",
                    "uri": "file:///tmp/executor-result.json"
                }],
                "diagnostics": [],
                "outputs": null,
                "workflow": null,
                "follow_up": null,
                "metadata": null
            }]
        }))
        .expect("aggregate");
        let refs = ref_inventory(&record, Some(&aggregate));
        let summary = work_summary(&record, Some(&aggregate), &refs);
        let (compact_refs, _) = compact_refs(&refs);

        assert_eq!(summary["artifact_ref_count"], 1);
        assert_eq!(summary["evidence_ref_count"], 1);
        assert_eq!(
            summary["classification"],
            "provider_completed_artifacts_available"
        );
        assert_eq!(compact_refs.as_array().expect("refs").len(), 2);
    }
}
