//! Read-side handlers: status, logs, artifacts, list/active/latest, and cancel.
//!
//! `status` returns a compact, recovery-first summary by default (#4396):
//! run id, state, totals, a per-task source table (#4392), deduped patch/changed
//! references, and a prominent risk-flag section (#4398). The full verbose
//! payload is available behind `--full`.

// Keep the read/transition handlers here; isolate rendering and I/O adapters.
mod adapters;
#[cfg(test)]
mod cook_summary_contract_tests;
mod diagnose;
mod diagnostics;
mod projections;
pub(super) use adapters::cancel;
use adapters::*;
pub(crate) use adapters::{
    artifacts, evidence, wait_for_recovery_status, wait_for_terminal_status,
};
pub(super) use diagnose::diagnose;
use diagnose::*;
pub(crate) use diagnostics::failure_reasons_from_aggregate;
use diagnostics::*;
use projections::*;
pub(crate) use projections::{
    bounded_cancel_report, bounded_full_operation_report, compact_aggregate_summary,
    compact_cook_report, compact_cook_report_with_summary, project_operator_output,
};

use crate::commands::operator_projection::serialized_len;
use crate::commands::runs::cook::{EvidenceArgs, LifecycleReadArgs};
use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_lifecycle::{
    resolve_cook_reader_target_in_store, AgentTaskCookReaderTarget,
};
use homeboy::agents::agent_task_provider::structured_error::normalized_structured_error;
use homeboy::agents::agent_task_service as agent_task_service_direct;
use homeboy::agents::agent_task_service::CookRecoveryStep;
use homeboy::agents::agent_task_service::{CookDelivery, CookDeliveryBlockerCode};
use homeboy::agents::agent_tasks::lifecycle::{self as agent_task_lifecycle, AgentTaskRunRecord};
use homeboy::agents::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::agents::agent_tasks::{
    AgentTaskEvidenceRef, AgentTaskFailureClassification, AgentTaskOutcomeStatus,
};
use homeboy::core::engine::shell::quote_arg;
use homeboy::core::error::{ActionSafety, ExecutableAction};
use homeboy::core::output::{budget_json_values, OutputBudget};
use homeboy_core::cook_status::CookStatus;
use homeboy_lab_contract::lab::transport_failure::LabTransportAttemptReceipt;

use super::super::CmdResult;
use super::args::{CancelArgs, DiagnoseArgs, ReconcileArgs, StatusArgs};
use super::candidate::classify_candidates;
use crate::commands::utils::response::{
    CommandActionableMetadata, CommandAgentTaskRef, CommandArtifactRef, CommandNextAction,
    CommandNextActionKind, CommandResultRefs, CommandRunRef, ACTIONABLE_METADATA_KEY,
};
use crate::commands::utils::watch::{
    parse_duration, watch_loop_with_interval, WatchConclusion, WatchConfig, WatchPoller,
    WatchResult, TIMEOUT_EXIT_CODE,
};

/// Cap the number of detail refs rendered in the compact summary so a noisy
/// aggregate cannot flood recovery output. Overflow is reported as an
/// `omitted` count rather than dropped silently.
const COMPACT_REF_LIMIT: usize = 12;

fn attach_gate_cleanup_warnings(value: &mut Value, record: &AgentTaskRunRecord) {
    let Ok(Some(promotion)) =
        agent_task_service_direct::recorded_promotion_report(&record.metadata)
    else {
        return;
    };
    let warnings: Vec<_> = promotion
        .get("deterministic_gates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|gate| {
            gate.get("cleanup_warning")
                .filter(|warning| !warning.is_null())
                .map(|warning| json!({"gate_id": gate.get("id"), "cleanup_warning": warning}))
        })
        .take(COMPACT_REF_LIMIT)
        .collect();
    if !warnings.is_empty() {
        value["gate_cleanup_warnings"] = json!(warnings);
    }
}
const COMPACT_TASK_LIMIT: usize = 12;
const COMPACT_TEXT_LIMIT: usize = 512;
/// A runnable command is the point of status; one that embeds durable input
/// paths (a Cook re-launch) is kept whole up to this size.
const COMPACT_COMMAND_LIMIT: usize = 2048;
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
const CANCEL_PROJECTION_BYTE_LIMIT: usize = 4 * 1024;

pub(super) fn resolve_cook_reader_target(
    run_or_cook_id: &str,
    exact: bool,
) -> homeboy::core::Result<AgentTaskCookReaderTarget> {
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    if exact {
        return Ok(AgentTaskCookReaderTarget {
            run_id: run_or_cook_id.to_string(),
            selection: None,
        });
    }
    resolve_cook_reader_target_in_store(&lifecycle_store, run_or_cook_id)
}

pub(super) fn status(args: StatusArgs) -> CmdResult<Value> {
    if super::cook_batch::is_workflow_instance(&args.run_id) {
        return super::cook_batch::batch_rollup_from_daemon(&args.run_id, "status");
    }
    if args.watch {
        return watch_status(args);
    }
    status_once(args)
}

fn status_once(args: StatusArgs) -> CmdResult<Value> {
    let (mut value, code) = status_without_feedback(args.clone())?;
    // CI receipts and recipe-only Cooks have no record to project (#18817).
    super::cook_summary::ensure(&mut value);
    let cook_id = bounded_exact_record(&args.run_id)
        .ok()
        .and_then(|record| record.metadata["cook_id"].as_str().map(str::to_string))
        .unwrap_or_else(|| args.run_id.clone());
    let store = homeboy::agents::agent_task_feedback::CookFeedbackStore::from_current_data_root()?;
    value["cook_feedback"] = store.status_value(&cook_id)?;
    value["placement"] = crate::commands::run_placement::placement(&value);
    // A queued local gate names its host-wide queue standing (#18555), with
    // the same fields the Lab step queue publishes.
    let local = homeboy::core::local_gate_admission::snapshot().unwrap_or(Value::Null);
    let waiting = local["waiting"].as_array().and_then(|queue| {
        queue.iter().find(|entry| {
            entry["cook_id"].as_str().is_some_and(|id| {
                id == cook_id || id == args.run_id || value["run"].as_str() == Some(id)
            })
        })
    });
    if let Some(waiting) = waiting {
        let position = waiting["queue_position"].as_u64().unwrap_or(1);
        value["local_admission"] = json!({
            "queue_position": position,
            "waiters_ahead": position.saturating_sub(1),
            "priority": waiting["priority"],
            "waited_seconds": waiting["waited_seconds"],
            "eta_seconds": waiting["eta_seconds"],
            "eta_kind": local["eta_kind"],
            "holders": local["holders"],
            "pressure": local["pressure"],
        });
    }
    Ok((value, code))
}

fn status_without_feedback(args: StatusArgs) -> CmdResult<Value> {
    if !args.exact {
        if let Some(ci) = agent_task_service_direct::cook_ci_status(&args.run_id)? {
            return Ok((ci, 0));
        }
    }
    if let Some(recipe_only) = recipe_only_status(&args.run_id, args.exact)? {
        return Ok((recipe_only, 0));
    }
    // Status follows continuation authority, not promotion's candidate selection.
    // Exact attempts and non-Cook runs retain their existing read behavior.
    let target = if !args.exact && agent_task_service_direct::recipe_exists(&args.run_id)? {
        AgentTaskCookReaderTarget {
            run_id: agent_task_service_direct::resolve_cook_continuation_run_id(&args.run_id)?,
            selection: None,
        }
    } else {
        resolve_cook_reader_target(&args.run_id, args.exact)?
    };
    let run = if args.exact {
        homeboy::agents::orchestration::run_exact_from_current_environment(&target.run_id)?
    } else {
        agent_task_service::control_plane_run(&target.run_id)?
    };
    let exit_code = if args.strict_subject_exit && control_plane_run_requires_action(&run) {
        1
    } else {
        0
    };
    // The same verdict `runs show`, `runs evidence <id> --dossier` and `diagnose` report
    // (#15885): status names why a Cook has not completed, not just its state.
    let verdict = crate::commands::cook_verdict::cook_verdict(&run);
    let recoverable = recover_has_step(&run);
    let mut value = serde_json::to_value(run).unwrap_or(Value::Null);
    if let Some(reason) = verdict.reason {
        value["reason"] = json!(reason);
    }
    if !args.exact
        && target.run_id != args.run_id
        && matches!(value["state"].as_str(), Some("running" | "queued"))
    {
        value["recovery"] = json!({
            "command": format!("homeboy agent-task status {} --watch", quote_arg(&target.run_id)),
        });
    }
    // Status is a pure projection (#13697, #15718): every read below is
    // bounded and never initializes, migrates, or reconciles durable state.
    value["failure"] = Value::Null;
    let mut summary = None;
    if let Ok(record) = bounded_exact_record(&target.run_id) {
        let outcome = cook_outcome_record(&args, &record);
        let outcome = outcome.as_ref().unwrap_or(&record);
        summary = Some(super::cook_summary::of_record(outcome));
        value["terminal_outcome"] = json!(outcome.terminal_outcome());
        value["recoverable"] = json!(recoverable);
        if record
            .metadata
            .get("unmaterialized_cook_admission")
            .is_some()
            && matches!(value["state"].as_str(), Some("running" | "queued"))
        {
            if let Ok(status) = homeboy::core::daemon::read_status() {
                let admission = status.admission_report();
                if admission["waiting_for"] == "daemon_replacement" {
                    value["waiting_for"] = admission["waiting_for"].clone();
                    value["daemon_admission"] = admission;
                }
            }
        }
        // A legacy retry reservation has neither an executable plan nor an
        // admission driver. Never present it as healthy capacity waiting.
        if record.state() == homeboy_lifecycle_contract::RunExecutionState::Queued
            && agent_task_lifecycle::is_unmaterialized_cook_admission(&record)
            && record.metadata["retry_of"].is_string()
        {
            let error = agent_task_lifecycle::unmaterialized_cook_retry_error(&record);
            value["reason"] = json!(error.message);
            value["waiting_for"] = json!("operator");
        }
        if let Some(promotion) = record
            .metadata
            .pointer("/cook_delivery/artifacts/promotion")
        {
            value["candidate_ref"] = promotion
                .get("candidate_ref")
                .cloned()
                .unwrap_or(Value::Null);
            value["gated_sha"] = promotion
                .pointer("/candidate/candidate_sha")
                .cloned()
                .unwrap_or(Value::Null);
            value["publication_sha"] = promotion
                .get("publication_sha")
                .cloned()
                .unwrap_or(Value::Null);
            value["tree"] = promotion
                .pointer("/candidate/tree_sha")
                .cloned()
                .unwrap_or(Value::Null);
            if value["publication_sha"].is_null() {
                if let (Some(path), Some(reference)) = (
                    promotion.pointer("/target/path").and_then(Value::as_str),
                    promotion.get("candidate_ref").and_then(Value::as_str),
                ) {
                    value["publication_sha"] = json!(homeboy::core::git::rev_parse(
                        std::path::Path::new(path),
                        reference
                    ));
                }
            }
            value["identity_note"] =
                json!("The publication commit carries the gated tree unchanged.");
            value["candidate_sha"] = promotion
                .pointer("/candidate/candidate_sha")
                .cloned()
                .unwrap_or(Value::Null);
        }
        // The typed immutable candidate commit harvest recorded (#16303).
        if let Some(candidate) = &record.candidate {
            value["candidate_commit"] = json!(candidate);
            value["gated_sha"] = json!(candidate.candidate_sha);
            value["tree"] = json!(candidate.tree_sha);
        }
        // Which repository holds the candidate, proven from any cwd (#18856).
        super::candidate_location::attach_candidate_location(&mut value, outcome);
        // Provider response identity, distinct from the requested routing tier.
        value["served_models"] = json!(record.served_models);
        attach_gate_cleanup_warnings(&mut value, outcome);
        attach_lab_snapshot_failure_projection(&mut value, &record);
        let admission = agent_task_service_direct::queued_admission(&record);
        attach_lab_admission_wait(&mut value, admission.as_ref());
        if let Some(wait) = admission
            .as_ref()
            .filter(|wait| !wait.live_driver && wait.liveness.is_reconcilable())
        {
            let probe = agent_task_lifecycle::runner_diagnostic_probe(&record);
            if let Some(diagnostic) =
                queued_runner_ownership_diagnostic(&record, wait.liveness, &probe)
            {
                value["reason"] = json!(diagnostic.message);
                value["runner_ownership_diagnostic"] = collected_diagnostic_value(diagnostic);
            }
        }
        if let Some(diagnostic) = lab_staging_root_cause(&record) {
            value["blocker_summary"] = json!(diagnostic.message);
            value["next_action"] = json!({
                "description": "Repair or recreate the malformed private Lab staging recipe, then retry the run.",
                "command": format!("homeboy agent-task retry {}", quote_arg(record.metadata["cook_id"].as_str().unwrap_or(&record.run_id))),
            });
            value["lab_staging_diagnostic"] = collected_diagnostic_value(diagnostic);
        }
        attach_gate_failure(&mut value, &record);
        attach_recovery_step(&mut value, &record);
        attach_local_dispatch_wait(&mut value, &record);
    }
    attach_promotion_replay(&mut value, &target.run_id, &args.run_id);
    if value["action_eligibility"]["actions"]
        .as_array()
        .is_some_and(|actions| {
            actions.iter().any(|action| {
                action["action"] == "placement_update" && action["availability"] == "available"
            })
        })
    {
        value["action_guidance"] = json!([{
            "action": "placement_update",
            "command": format!(
                "homeboy --placement local agent-task placement-update {} --confirm",
                quote_arg(&target.run_id)
            ),
            "reason": "explicit local placement is legal before execution ownership begins",
        }]);
    }
    super::cook_summary::attach(&mut value, summary);
    if args.full {
        deduplicate_control_plane_evidence(&mut value);
    } else {
        value = compact_control_plane_status(value);
    }
    Ok((value, exit_code))
}

/// A settled Cook's outcome is its decisive attempt's: the attempt candidate
/// selection and `diagnose` read. A later attempt that produced no candidate
/// (a no-change retry, a retry that failed to start) does not replace the gate
/// verdict on the candidate it followed. Running Cooks and exact reads keep
/// the continuation attempt's own record.
fn cook_outcome_record(
    args: &StatusArgs,
    continuation: &AgentTaskRunRecord,
) -> Option<AgentTaskRunRecord> {
    if args.exact || !homeboy::agents::agent_task_lifecycle::CookRunState::of(continuation).terminal
    {
        return None;
    }
    let selected = resolve_cook_reader_target(&args.run_id, false).ok()?;
    if selected.run_id == continuation.run_id {
        return None;
    }
    bounded_exact_record(&selected.run_id)
        .ok()
        .filter(|record| homeboy::agents::agent_task_lifecycle::CookRunState::of(record).terminal)
}

/// The latest admitted recovery step (for example `recover --regate`) and its
/// recorded outcome, so an admitted step is never invisible (#18657).
fn attach_recovery_step(value: &mut Value, record: &AgentTaskRunRecord) {
    let Ok(store) = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment() else {
        return;
    };
    if let Some(step) = agent_task_service_direct::recovery_step_projection(&store, record) {
        if step["state"] == "failed" {
            value["blocker"] = step["failure"].clone();
            value["failure"] = step["failure"].clone();
        }
        value["recovery_step"] = step;
    }
}

fn bounded_exact_record(run_id: &str) -> homeboy::core::Result<AgentTaskRunRecord> {
    homeboy::agents::agent_task_lifecycle::exact_record_bounded_in_store(
        &agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?,
        run_id,
    )
}

fn attach_promotion_replay(value: &mut Value, run_id: &str, requested_id: &str) {
    if !value["failure"].is_null()
        || crate::commands::agent_task_summary::live_gate_verifying(value)
    {
        return;
    }
    let Ok(record) = bounded_exact_record(run_id) else {
        return;
    };
    let recoverable = homeboy::agents::agent_task_lifecycle::CookRunState::of(&record).recoverable
        || CookDelivery::from_run_metadata(&record.metadata).is_some_and(|delivery| {
            delivery.blocker_code() == Some(CookDeliveryBlockerCode::GateFailed)
        });
    if !recoverable
        || record
            .metadata
            .get("cook_id")
            .and_then(Value::as_str)
            .is_none()
    {
        return;
    }
    let guidance = agent_task_service_direct::promotion_replay_guidance(requested_id);
    value["promotion_replay"] = guidance;
}

#[test]
fn compact_recovery_preserves_selected_planner_eligibility() {
    let value = json!({
        "run": "run", "state": "candidate_recoverable",
        "recovery": { "step": "regate", "command": "homeboy agent-task recover run" },
        "action_eligibility": { "actions": [
            { "action": "promote", "availability": "indeterminate" }
        ] }
    });
    let compact = compact_control_plane_status(value);
    assert_eq!(
        compact["next_action"]["command"],
        "homeboy agent-task recover run"
    );
    assert_eq!(
        compact["action_eligibility"]["actions"],
        json!([
            { "action": "promote", "availability": "indeterminate" }
        ])
    );
    assert_eq!(compact["action_eligibility"]["available_action_count"], 0);
}

/// `recoverable` means `agent-task recover` has a step: the projected Cook
/// recovery plan, or `Recover` eligibility for runs that are not Cooks.
fn recover_has_step(run: &homeboy_control_plane_contract::ControlPlaneRun) -> bool {
    use homeboy_control_plane_contract::{ControlPlaneAction, ControlPlaneActionAvailability};

    if let Some(recovery) = &run.recovery {
        return recovery.step != "none";
    }
    run.action_eligibility.as_ref().is_some_and(|report| {
        report.actions.iter().any(|action| {
            action.action == ControlPlaneAction::Recover
                && action.availability == ControlPlaneActionAvailability::Available
        })
    })
}

fn control_plane_run_requires_action(
    run: &homeboy_control_plane_contract::ControlPlaneRun,
) -> bool {
    use homeboy_control_plane_contract::{ControlPlaneAction, ControlPlaneActionAvailability};

    // A Cook requires action exactly when `agent-task recover` has a step.
    recover_has_step(run)
        || (run.recovery.is_none()
            && run.action_eligibility.as_ref().is_some_and(|report| {
                report.actions.iter().any(|action| {
                    matches!(
                        action.action,
                        ControlPlaneAction::PlacementUpdate | ControlPlaneAction::Retry
                    ) && action.availability == ControlPlaneActionAvailability::Available
                })
            }))
}

fn compact_cook_candidate_projection(run_id: &str) -> Value {
    match agent_task_lifecycle::durable_local_read(run_id) {
        Ok(snapshot) => {
            let Some(aggregate) = snapshot.aggregate else {
                return json!({
                    "status": "unavailable",
                    "run_id": snapshot.record.run_id,
                    "reason": bounded_value(&Value::String(snapshot.unavailable_sources.first().map(|source| source.detail.as_str()).unwrap_or("the authoritative observation has no aggregate").to_string())),
                    "canonical_candidate": homeboy::agents::orchestration::canonical_candidate_from_durable_record(&snapshot.record, None).ok(),
                });
            };
            let projection =
                homeboy::agents::orchestration::canonical_candidate_from_durable_record(
                    &snapshot.record,
                    Some(&aggregate),
                )
                .unwrap_or(Value::Null);
            let candidate = classify_candidates(&json!({ "canonical_candidate": projection }));
            let artifact_count = aggregate
                .outcomes
                .iter()
                .map(|outcome| outcome.artifacts.len())
                .sum::<usize>();
            let outcome_count = aggregate.outcomes.len();
            let provider_execution_count = candidate
                .provider_executions
                .map(|count| count.max(outcome_count))
                .unwrap_or(outcome_count);
            json!({
                "status": "available",
                "run_id": snapshot.record.run_id,
                "state": snapshot.record.state(),
                "task_count": snapshot.record.tasks.len(),
                "aggregate_path": snapshot.record.aggregate_path,
                "outcome_count": outcome_count,
                "provider_execution_count": provider_execution_count,
                "artifact_count": artifact_count,
                "canonical_candidate": projection,
            })
        }
        Err(error) => json!({
            "status": "unavailable",
            "run_id": run_id,
            "reason": bounded_value(&Value::String(error.message)),
        }),
    }
}

/// Status is a read-only diagnostic. A recipe-only Cook is recoverable through
/// its exact attempt id, but only `agent-task recover` may materialize that attempt.
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
    // `recover` continues the exact attempt for this state (#15925).
    let recover_command = agent_task_service_direct::cook_recovery_command(
        &attempt.run_id,
        &["recover", &attempt.run_id],
    );
    // A recipe no controller has driven for longer than the heartbeat window
    // is an abandoned admission, not a Cook waiting to be resumed: its rows
    // are reconciliation work. Resuming it stays an explicit choice.
    let guidance = if agent_task_service_direct::recipe_only_cook_is_abandoned(&recipe.cook_id)? {
        json!({
            "action": "reconcile",
            "command": "homeboy agent-task reconcile --all",
            "apply_command": "homeboy agent-task reconcile --all --apply",
            "recover_command": recover_command,
            "message": "The Cook recipe is durable but its attempt never materialized a lifecycle record and no controller has driven it within the heartbeat window. Reconcile settles its rows that are still `running`; run the recover command only to resume this exact attempt deliberately."
        })
    } else {
        json!({
            "action": "materialize_recipe_attempt",
            "command": recover_command,
            "message": "The immutable Cook recipe is durable but its lifecycle record is absent. Continue the exact attempt to materialize the controller lifecycle before provider work."
        })
    };
    Ok(Some(json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": recipe.cook_id,
        "run_id": attempt.run_id,
        "latest_run_id": attempt.run_id,
        "status": "recipe_only_recovery_required",
        "lifecycle_state": "recipe_persisted_without_lifecycle_record",
        "provider_budget_consumed": false,
        "provider_executions_consumed": 0,
        "guidance": guidance,
    })))
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

const STDERR_INLINE_LIMIT: usize = 4 * 1024;
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

pub(crate) fn list_runs(
    filter: agent_task_service::AgentTaskDiscoveryFilter,
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let report = agent_task_service_direct::discover_runs_with_options(filter, options)?;
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    attach_agent_task_discovery_actionable(&mut value, None, false);
    Ok((value, 0))
}

pub(crate) fn list_runs_page(
    filter: agent_task_service::AgentTaskDiscoveryFilter,
    args: crate::commands::runs::RunsListArgs,
) -> CmdResult<Value> {
    let command = list_continuation_prefix(&args);
    let branch_scoped = args.branch.is_some();
    let limit = args.limit as usize;
    let report = agent_task_service_direct::discover_runs_page(
        filter,
        agent_task_service_direct::AgentTaskDiscoveryPageOptions {
            limit,
            cursor: args.cursor,
            repo: args.repo,
            workspace: args.workspace,
            task_url: args.task_url,
            submitted_after: args.since,
            state: args.status,
            placement: args.run_placement,
            parent_id: args.parent_id,
            branch: args.branch,
        },
    )?;
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    attach_agent_task_discovery_actionable(&mut value, Some(&command), branch_scoped);
    Ok((value, 0))
}

/// `runs list --kind agent-task --active`: list queued + running runs, but SEPARATE them into
/// active / stale / suspect / unreconciled buckets so a stale or orphaned
/// `running` record (especially a Lab/offloaded run whose runner process died)
/// is never silently treated as genuinely-active (#5682).
///
/// The base discovery report (with per-run liveness, source, last-update age,
/// and a per-run safe reconcile command) is preserved under `report`, and a
/// `buckets` view groups run ids by classification for an at-a-glance triage.
pub(crate) fn list_active(
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let branch_scoped = options.branch.is_some();
    let report = agent_task_service_direct::discover_runs_with_options(
        agent_task_service::AgentTaskDiscoveryFilter::Active,
        options,
    )?;
    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);

    let buckets = active_liveness_buckets(&report.runs);
    if let Value::Object(map) = &mut value {
        map.insert("buckets".to_string(), buckets);
        map.insert(
            "reconcile_hint".to_string(),
            json!("run the per-run `commands.reconcile` preview, then repeat it with `--apply` after reviewing authoritative provider state"),
        );
    }
    attach_agent_task_discovery_actionable(
        &mut value,
        Some("homeboy runs list --kind agent-task --active"),
        branch_scoped,
    );
    Ok((value, 0))
}

pub(crate) fn list_active_page(args: crate::commands::runs::RunsListArgs) -> CmdResult<Value> {
    let command = active_continuation_prefix(&args);
    let branch_scoped = args.branch.is_some();
    let limit = args.limit as usize;
    let report = agent_task_service_direct::discover_runs_page(
        agent_task_service::AgentTaskDiscoveryFilter::Active,
        agent_task_service_direct::AgentTaskDiscoveryPageOptions {
            limit,
            cursor: args.cursor,
            branch: args.branch,
            ..Default::default()
        },
    )?;
    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("buckets".to_string(), active_liveness_buckets(&report.runs));
        map.insert(
            "reconcile_hint".to_string(),
            json!("run the per-run `commands.reconcile` preview, then repeat it with `--apply` after reviewing authoritative provider state"),
        );
    }
    attach_agent_task_discovery_actionable(&mut value, Some(&command), branch_scoped);
    Ok((value, 0))
}

fn list_continuation_prefix(args: &crate::commands::runs::RunsListArgs) -> String {
    let mut command = "homeboy runs list --kind agent-task".to_string();
    append_discovery_scope(
        &mut command,
        [
            ("repo", args.repo.as_deref()),
            ("workspace", args.workspace.as_deref()),
            ("task-url", args.task_url.as_deref()),
            ("since", args.since.as_deref()),
            ("status", args.status.as_deref()),
            ("run-placement", args.run_placement.as_deref()),
            ("parent-id", args.parent_id.as_deref()),
            ("branch", args.branch.as_deref()),
        ],
    );
    command
}

fn active_continuation_prefix(args: &crate::commands::runs::RunsListArgs) -> String {
    let mut command = "homeboy runs list --kind agent-task --active".to_string();
    append_discovery_scope(&mut command, [("branch", args.branch.as_deref())]);
    command
}

fn append_discovery_scope<const N: usize>(command: &mut String, fields: [(&str, Option<&str>); N]) {
    for (flag, value) in fields {
        if let Some(value) = value {
            command.push_str(&format!(" --{flag} {}", quote_arg(value)));
        }
    }
}

/// `agent-task reconcile --all`: preview stale/suspect/unreconciled records
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
    if args.all {
        return reconcile_active(!args.apply);
    }
    let run_id = args.run_id.expect("clap requires run_id or --all");
    if args.apply {
        let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
        let resolved_run_ids =
            agent_task_lifecycle::reconcile_scope_run_ids_in_store(&lifecycle_store, &run_id)?;
        let scope_key = args
            .idempotency_key
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut acknowledgements = Vec::with_capacity(resolved_run_ids.len());
        let scoped = resolved_run_ids.len() > 1;
        for resolved_run_id in resolved_run_ids {
            // A group is only a selection mechanism. Every durable record gets
            // its own action identity, receipt, and event stream.
            let idempotency_key = if scoped {
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("{scope_key}:reconcile:{resolved_run_id}").as_bytes(),
                )
                .to_string()
            } else {
                scope_key.clone()
            };
            acknowledgements.push(
                homeboy::agents::orchestration::execute_action_from_current_environment(
                    &resolved_run_id,
                    &homeboy_control_plane_contract::ControlPlaneActionRequest {
                        schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA
                            .to_string(),
                        action: homeboy_control_plane_contract::ControlPlaneAction::Reconcile,
                        effect_id: homeboy_control_plane_contract::action_effect_id(
                            "cli",
                            &resolved_run_id,
                            "reconcile",
                            &idempotency_key,
                        ),
                        idempotency_key,
                        actor: "homeboy-cli".to_string(),
                        expected_updated_at: None,
                        parameters:
                            homeboy_control_plane_contract::ControlPlaneActionPayload::empty(),
                        confirmed: true,
                    },
                )?,
            );
        }
        let exit = i32::from(acknowledgements.iter().any(|acknowledgement| {
            acknowledgement.outcome
                == homeboy_control_plane_contract::ControlPlaneActionOutcome::Failed
        }));
        if acknowledgements.len() == 1 {
            return Ok((
                serde_json::to_value(acknowledgements.remove(0)).unwrap_or(Value::Null),
                exit,
            ));
        }
        return Ok((
            json!({
                "schema": "homeboy/control-plane-reconcile-scope-result/v1",
                "requested_run_id": run_id,
                "acknowledgements": acknowledgements,
            }),
            exit,
        ));
    }

    let report = agent_task_service_direct::reconcile_run(&run_id, true)?;
    let exit = i32::from(report.failed > 0);
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    if let Value::Object(object) = &mut value {
        object.insert("owner".to_string(), json!("durable_agent_tasks"));
        object.insert(
            "scope".to_string(),
            json!(format!("durable run or Cook group `{run_id}`")),
        );
        object.insert(
            "postcondition".to_string(),
            json!("reports the selected durable records as stored, without persisted mutation; --apply re-reads authoritative provider state before acting"),
        );
    }
    Ok((value, exit))
}

/// Group active-run ids by liveness classification for a scannable triage view.
///
/// The bucket names, the "no classification means active" default, and the
/// reconcilability of each bucket all come from [`AgentTaskLiveness`] itself
/// rather than from a hand-rolled match here. The previous version restated the
/// four-way mapping in the CLI, which is precisely the duplication #W3-4 is
/// about: an orchestrator reading `liveness: "suspect"` had to reimplement the
/// same table to know whether it was allowed to reconcile.
fn active_liveness_buckets(runs: &[agent_task_service_direct::AgentTaskDiscoveryRun]) -> Value {
    use agent_task_service_direct::AgentTaskLiveness;

    let mut buckets = serde_json::Map::new();
    for liveness in AgentTaskLiveness::ALL {
        buckets.insert(liveness.as_str().to_string(), Value::Array(Vec::new()));
    }

    for run in runs {
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

/// Snapshot construction happens before a provider or runner owns work. The
/// recovery marker is compatibility-tolerant because persisted failures from
/// before lifecycle projection carried the producer's stale command string.
/// The recovery marker of a Lab snapshot-construction failure, when the
/// record carries one.
fn lab_snapshot_recovery(record: &AgentTaskRunRecord) -> Option<&Value> {
    let recovery = record
        .metadata
        .pointer("/pre_execution_failure/details/recovery")?;
    let snapshot_construction = record
        .metadata
        .pointer("/pre_execution_failure/details/classification")?
        .as_str()
        == Some("snapshot_construction");
    let lifecycle_marker = matches!(
        (
            recovery.get("owner").and_then(Value::as_str),
            recovery.get("action").and_then(Value::as_str),
        ),
        (
            Some("durable_lifecycle"),
            Some("project_lifecycle_recovery")
        ) | (
            Some("homeboy_snapshot_staging"),
            Some("rebuild_snapshot_staging_and_replay_cook")
        )
    );
    (snapshot_construction && lifecycle_marker).then_some(recovery)
}

fn lab_snapshot_recovery_projection(
    record: &AgentTaskRunRecord,
    retry: &RetryReplayAction,
) -> Option<Value> {
    let recovery = lab_snapshot_recovery(record)?;
    let remediation = recovery
        .get("remediation")
        .and_then(Value::as_str)
        .unwrap_or("Correct the controller-side snapshot inputs before retrying or starting replacement lifecycle work.");
    let reason = retry
        .reason
        .as_deref()
        .or_else(|| recovery.get("reason").and_then(Value::as_str));
    let owner = if let Some(cook_id) = record.metadata["cook_id"].as_str() {
        json!({
            "lifecycle": "cook",
            "cook_id": cook_id,
            "attempt_run_id": record.run_id,
        })
    } else {
        json!({
            "lifecycle": "agent_task",
            "run_id": record.run_id,
        })
    };
    Some(json!({
        "owner": owner,
        "readiness": retry.readiness,
        "reason": reason,
        "admission": retry.admission,
        "action": retry.action.as_ref().and_then(|action| action.action.clone()),
        "remediation": remediation,
    }))
}

fn attach_lab_snapshot_failure_projection(value: &mut Value, record: &AgentTaskRunRecord) {
    // Every watch poll reads status. Deriving the retry action loads the
    // replay plan, recipe and retry admission, so derive it only for records
    // that project a snapshot failure.
    if lab_snapshot_recovery(record).is_none() {
        return;
    }
    let retry = retry_replay_action(record);
    if let Some(projection) = lab_snapshot_recovery_projection(record, &retry) {
        value["lab_snapshot_failure"] = projection;
    }
}

const DISCOVERY_NEXT_ACTION_LIMIT: usize = 8;

fn attach_agent_task_discovery_actionable(
    value: &mut Value,
    active_command: Option<&str>,
    branch_scoped: bool,
) {
    if branch_scoped {
        // The service summary is reusable across discovery callers and carries
        // fleet reconciliation guidance. A branch-filtered CLI response must
        // only offer its per-run reconciliation commands.
        if let Some(summary) = value
            .get_mut("liveness_summary")
            .and_then(Value::as_object_mut)
        {
            summary.remove("reconcile_command");
        }
    }
    let runs = value
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut metadata = CommandActionableMetadata::default();

    if !branch_scoped
        && value
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
                "homeboy agent-task reconcile --all",
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }
    if let (Some(command), Some(cursor), Some(limit)) = (
        active_command,
        value.get("next_cursor").and_then(Value::as_str),
        value.get("limit").and_then(Value::as_u64),
    ) {
        metadata.next_actions.push(
            CommandNextAction::new(
                "show next page",
                format!("{command} --limit {limit} --cursor {}", quote_arg(cursor)),
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
        logs_command: format!("homeboy runs logs {run_id}"),
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
                            | "recover"
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
            for (key, value) in values.iter_mut() {
                // Retained gate evidence is data, not a suggested lifecycle command.
                if key != "failure" {
                    preserve_controller_owner_placement_with_prefix(value, run_id, recovery_prefix);
                }
            }
        }
        _ => {}
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

#[cfg(test)]
use crate::commands::utils::watch::watch_loop;

#[cfg(test)]
#[test]
fn status_wait_interval_tracks_projection_changes() {
    let mut policy = StatusWaitInterval::default();
    for (projection, seconds) in [
        ("queued", 1),
        ("queued", 2),
        ("queued", 4),
        ("queued", 8),
        ("queued", 15),
        ("queued", 15),
        ("running", 1),
        ("running", 2),
        ("new progress", 1),
    ] {
        assert_eq!(
            policy.observe(json!(projection)),
            Duration::from_secs(seconds)
        );
    }
}

#[cfg(test)]
#[path = "status/compact_status_tests.rs"]
mod compact_status_tests;

#[cfg(test)]
#[path = "status/bounded_failure_result_tests.rs"]
mod bounded_failure_result_tests;

#[cfg(test)]
#[path = "status/artifact_content_tests.rs"]
mod artifact_content_tests;

#[cfg(test)]
#[path = "status/watch_tests.rs"]
mod watch_tests;

#[cfg(test)]
#[path = "status/diagnose_actionable_tests.rs"]
mod diagnose_actionable_tests;

#[cfg(test)]
#[path = "status/cancel_exit_code_tests.rs"]
mod cancel_exit_code_tests;

#[cfg(test)]
#[path = "status/tests.rs"]
mod tests;

#[cfg(test)]
mod gate_failure_projection_tests {
    use super::*;

    #[test]
    fn status_and_diagnose_failure_table() {
        for (status, private, termination, expected_action) in [
            (
                "failed",
                false,
                "completed",
                "homeboy agent-task recover run",
            ),
            (
                "failed",
                true,
                "completed",
                "homeboy agent-task recover run",
            ),
            (
                "failed",
                false,
                "no_progress",
                "homeboy agent-task recover run --regate",
            ),
            ("succeeded", false, "completed", ""),
        ] {
            let stdout = (1..=50)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            let report = json!({"deterministic_gates": [{
                "id": "verify", "status": status,
                "visibility": if private { "private" } else { "public" },
                "reveal_policy": "summary_only", "command": ["cargo", "test"],
                "exit_code": 1, "termination": termination, "stdout": stdout,
                "lab_receipt": {"artifact_ref": "homeboy://run/run/artifact/output"}
            }]});
            let failure = agent_task_service_direct::gate_failure_projection(&report);
            let mut status_value = json!({"run": "run"});
            let mut diagnose_value =
                json!({"summary": "deterministic gates rejected the candidate"});
            project_gate_failure(&mut status_value, failure.clone(), "run");
            project_gate_failure(&mut diagnose_value, failure, "run");
            let status_value = compact_control_plane_status(status_value);
            assert_eq!(status_value["failure"], diagnose_value["failure"]);
            if status == "succeeded" {
                assert!(status_value["failure"].is_null());
                continue;
            }
            let failure = &status_value["failure"];
            assert_eq!(failure["gate"], "verify#1");
            assert_eq!(failure["exit_code"], 1);
            assert_eq!(failure["termination"], termination);
            assert_eq!(failure["artifact"], "homeboy://run/run/artifact/output");
            if private {
                assert!(failure["command"].is_null());
                assert!(failure["output_tail"].is_null());
            } else {
                assert_eq!(failure["command"], "cargo test");
                let tail = failure["output_tail"].as_str().unwrap();
                assert_eq!(tail.lines().count(), 40);
                assert!(tail.starts_with("line 11\n"));
            }
            for value in [&status_value, &diagnose_value] {
                assert_eq!(value["next_action"]["command"], expected_action);
                if termination == "no_progress" {
                    let suggestion = value["next_action"]["description"].as_str().unwrap();
                    assert!(suggestion.contains("--gate-timeout-seconds"));
                    assert!(suggestion.contains("--gate-no-progress-timeout-seconds"));
                }
            }
            let summary = diagnose_value["summary"].as_str().unwrap();
            assert!(summary.contains("verify#1"));
            assert!(summary.contains("exit 1"));
            assert!(summary.contains("homeboy://run/run/artifact/output"));
        }
    }
}
