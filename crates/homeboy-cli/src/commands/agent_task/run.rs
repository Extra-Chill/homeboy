//! Durable run lifecycle handlers: cook, run-plan, run, run-next, submit,
//! resume, and retry.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use homeboy::agents::agent_tasks::dispatch_service;
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::provider;
use homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::agents::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::core::command_invocation::CommandInvocation;
use homeboy::core::defaults;
use homeboy::core::worktree_providers::{
    provision_apply_enabled_worktree_provider_from_config, WorktreeProviderCreateIntent,
};

use super::super::agent_task_dispatch::DispatchArgs;
use super::super::CmdResult;
use super::args::{
    AgentTaskCookArgs, CookContinueArgs, LifecycleReadArgs, PromotionProviderArgs, RetryArgs,
    RunArgs, RunPlanArgs, SubmitArgs,
};

const MAX_PROMOTION_PROVIDER_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

/// Operator-facing durable identity block for a Cook that has just become
/// addressable.
///
/// A Cook's run id is the only handle an interrupted caller has. Everything
/// after durable submission — gate toolchain preflight, transport preparation,
/// Lab materialization, provider execution — can outlive a client timeout, so
/// the handle and its follow-up commands are assembled here and printed the
/// first time the controller reports identity (#10419, #9163).
pub(crate) fn durable_cook_identity_lines(cook_id: Option<&str>, run_id: &str) -> Vec<String> {
    let cook_suffix = cook_id
        .filter(|cook_id| *cook_id != run_id)
        .map(|cook_id| format!(" (cook `{cook_id}`)"))
        .unwrap_or_default();
    vec![
        format!("cook: durable run id `{run_id}`{cook_suffix} — persisted before materialization."),
        format!("cook: status -> homeboy --placement local agent-task status {run_id}"),
        format!("cook: logs   -> homeboy --placement local agent-task logs {run_id}"),
        format!("cook: cancel -> homeboy --placement local agent-task cancel {run_id}"),
    ]
}

/// Print the durable identity block unconditionally.
///
/// Deliberately not TTY-gated. The clients that most need this — agent/Discord
/// bridges, CI, and anything invoking Homeboy as a tool call — are exactly the
/// non-TTY callers that a TTY-gated status line silently skips, which is how an
/// interrupted Lab Cook ended up with no reported task identity at all (#10419).
pub(crate) fn announce_durable_cook_identity(cook_id: Option<&str>, run_id: &str) {
    for line in durable_cook_identity_lines(cook_id, run_id) {
        eprintln!("{line}");
    }
}

/// Operator-facing statement of the provider rotation a Cook will actually
/// perform.
///
/// A configured `agent_task.rotation` is only reachable if the execution budget
/// funds it, and nothing surfaced that mismatch — which is how a Cook could
/// carry a multi-provider rotation policy and still die terminally on the first
/// provider's recoverable failure while every other provider sat available
/// (#11082). State the effective behavior on every submission, including when
/// that behavior is "no rotation at all".
pub(crate) fn cook_rotation_disclosure(plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    let executions = budget.max_provider_executions;
    let entries = plan
        .options
        .rotation
        .as_ref()
        .map_or(0, |rotation| rotation.entries.len());
    let entries = u32::try_from(entries).unwrap_or(u32::MAX);
    // Only rotations that are both configured AND affordable will ever fire.
    let funded = entries
        .min(budget.max_provider_rotations)
        .min(executions.saturating_sub(1));
    if funded == 0 {
        let unreachable = if entries > 0 {
            format!(
                "; {entries} configured rotation provider(s) are unreachable at this budget \
                 (--max-provider-executions {executions}, --max-provider-rotations {})",
                budget.max_provider_rotations
            )
        } else {
            String::new()
        };
        format!("cook: rotation: disabled ({executions} provider execution(s)){unreachable}")
    } else {
        format!(
            "cook: rotation: {funded} fallback provider(s), up to {executions} provider execution(s)"
        )
    }
}

fn cook_resolved_policy_disclosure(max_attempts: u32, plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    format!(
        "cook: retry policy: 1 initial execution, {} same-provider remediation retry(ies), {} rotation(s), {} provider execution(s) maximum",
        budget.max_same_provider_retries,
        budget.max_provider_rotations,
        budget.max_provider_executions.max(max_attempts),
    )
}

/// Serialize a completed run aggregate and, when the run did not fully succeed,
/// surface a prominent top-level `failure_reasons` summary so the operator sees
/// the root cause (recipe validation, PHP fatal, provider registration, missing
/// path) without hand-digging the nested outcome JSON (#3806). The full nested
/// payload is preserved unchanged; this only ADDS the surfaced summary.
fn aggregate_value_with_failure_reasons(aggregate: &AgentTaskAggregate) -> Value {
    let aggregate = homeboy::agents::agent_task_artifacts::reviewer_facing_aggregate(aggregate);
    let mut value = serde_json::to_value(&aggregate).unwrap_or(Value::Null);
    let failure_reasons = super::status::failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        if let Value::Object(map) = &mut value {
            map.insert("failure_reasons".to_string(), Value::Array(failure_reasons));
        }
    }
    value
}

pub(crate) fn run_cook(args: AgentTaskCookArgs) -> CmdResult<Value> {
    let args = resolve_cook_destination(args)?;
    validate_cook_request(&args)?;
    run_cook_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

/// Resume a Cook from its immutable recipe rather than asking the operator to
/// replay prompt, provider, gate, workspace, or disclosure arguments.
pub(crate) fn continue_cook(args: CookContinueArgs) -> CmdResult<Value> {
    continue_cook_with(
        args,
        ExtensionProviderAgentTaskExecutor::discover(),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )
}

pub(crate) fn continue_cook_with<E, F>(
    args: CookContinueArgs,
    executor: E,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    let recipe =
        agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        })?;
    let run_id = agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id)?;
    let record = agent_task_service::reconcile_recipe_attempt_for_continuation(&recipe, &run_id)?;
    if !record.state.is_terminal() {
        return Ok((cook_continuation_status(&recipe.cook_id, &record), 0));
    }

    if matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
    ) {
        // Ordinary continuation consumes only the pending entry scheduled by
        // authoritative reconciliation. Failed and completed claims require an
        // explicit rearm and can never be silently replayed.
        let claim = if args.rearm {
            agent_task_service::claim_continuation_for_recovery(&recipe.cook_id, &run_id)?
        } else {
            agent_task_service::claim_continuation_for(&recipe.cook_id, &run_id)?
        };
        let Some(claim) = claim else {
            return Ok((
                cook_terminal_continuation_status(
                    &recipe.cook_id,
                    &run_id,
                    &format!("{:?}", record.state),
                    agent_task_service::continuation_state(&recipe.cook_id, &run_id)?,
                ),
                0,
            ));
        };
        let mut result = None;
        let historical_terminal =
            recipe.runtime_generation != homeboy::core::build_identity::current().display;
        let dispatcher = reconstruct_dispatcher;
        let executor = executor.clone();
        let execute = |options| {
            agent_task_service::authorize_cook_continue_route(&options)?;
            let cook = if historical_terminal {
                agent_task_service::run_terminal_cook_continuation(options, executor.clone())?
            } else {
                agent_task_service::run_cook(options, executor.clone())?
            };
            let exit_code = cook.exit_code;
            result = Some(cook.value);
            Ok(exit_code)
        };
        let exit_code = if historical_terminal {
            agent_task_service::consume_claimed_terminal_with_dispatcher(
                claim, dispatcher, execute,
            )?
        } else {
            agent_task_service::consume_claimed_with_dispatcher(claim, dispatcher, execute)?
        };
        let value = cook_report_with_continuation(
            serde_json::to_value(result.ok_or_else(|| {
                homeboy::core::Error::internal_unexpected(
                    "claimed Cook continuation returned no result",
                )
            })?)
            .unwrap_or(Value::Null),
        );
        return Ok((
            super::status::compact_cook_report(value, args.full),
            exit_code,
        ));
    }

    let dispatcher = reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cook_or_attempt_id",
                "selected attempt is absent from its durable Cook recipe",
                Some(run_id.clone()),
                None,
            )
        })?;
    let terminal_review_form_continuation =
        agent_task_service::terminal_review_form_continuation_is_eligible(&attempt.plan, &record)?;
    let mut options = if terminal_review_form_continuation {
        agent_task_service::reconstruct_adoption_options_with_dispatcher(&recipe, dispatcher)?
    } else {
        agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)?
    };
    options.initial_run_id = attempt.run_id.clone();
    options.initial_plan = attempt.plan.clone();
    agent_task_service::authorize_cook_continue_route(&options)?;
    let result = if terminal_review_form_continuation {
        agent_task_service::run_terminal_cook_continuation(options, executor)?
    } else {
        agent_task_service::run_cook(options, executor)?
    };
    let value =
        cook_report_with_continuation(serde_json::to_value(result.value).unwrap_or(Value::Null));
    Ok((
        super::status::compact_cook_report(value, args.full),
        result.exit_code,
    ))
}

/// Probe the continuation admission boundary without claiming a continuation or
/// calling any execution, transport, or finalization API.
pub(crate) fn preflight_continue_cook(args: CookContinueArgs) -> CmdResult<Value> {
    let mut phases = Vec::new();
    let mut selected_run_id = None;
    let mut candidate_fingerprint = Value::Null;
    let recipe =
        match agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        }) {
            Ok(recipe) => {
                phases.push(
                    serde_json::json!({ "phase": "recipe", "status": "passed", "reason": "ok" }),
                );
                recipe
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(None, Value::Null, phases, "recipe", &error),
                    1,
                ))
            }
        };
    let run_id =
        match agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id) {
            Ok(run_id) => {
                selected_run_id = Some(run_id.clone());
                phases.push(
                    serde_json::json!({ "phase": "selection", "status": "passed", "reason": "ok" }),
                );
                run_id
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        Value::Null,
                        phases,
                        "selection",
                        &error,
                    ),
                    1,
                ))
            }
        };
    let record = match agent_task_service::reconcile_recipe_attempt_for_continuation(
        &recipe, &run_id,
    ) {
        Ok(record) => {
            candidate_fingerprint = record
                .metadata
                .pointer("/latest_promotion/provenance/candidate")
                .cloned()
                .unwrap_or(Value::Null);
            phases.push(serde_json::json!({ "phase": "lifecycle", "status": "passed", "reason": "ok", "state": format!("{:?}", record.state) }));
            record
        }
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    candidate_fingerprint,
                    phases,
                    "lifecycle",
                    &error,
                ),
                1,
            ))
        }
    };
    if !record.state.is_terminal() {
        let error = homeboy::core::Error::validation_invalid_argument(
            "cook_or_attempt_id",
            "selected Cook attempt is not terminal and cannot be admitted to dispatch",
            Some(run_id),
            None,
        );
        return Ok((
            cook_continuation_preflight_report(
                selected_run_id,
                candidate_fingerprint,
                phases,
                "lifecycle",
                &error,
            ),
            1,
        ));
    }
    let dispatcher = match crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(
        &recipe.promotion_transport["attempt_dispatch"],
    ) {
        Ok(dispatcher) => {
            phases.push(
                serde_json::json!({ "phase": "transport", "status": "passed", "reason": "ok" }),
            );
            dispatcher
        }
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    candidate_fingerprint,
                    phases,
                    "transport",
                    &error,
                ),
                1,
            ))
        }
    };
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .expect("continuation selection is recipe-bound");
    let terminal_review =
        agent_task_service::terminal_review_form_continuation_is_eligible(&attempt.plan, &record)?;
    let mut options = match if terminal_review {
        agent_task_service::reconstruct_adoption_options_with_dispatcher(&recipe, dispatcher)
    } else {
        agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)
    } {
        Ok(options) => options,
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    candidate_fingerprint,
                    phases,
                    "recipe",
                    &error,
                ),
                1,
            ))
        }
    };
    options.initial_run_id = attempt.run_id.clone();
    options.initial_plan = attempt.plan.clone();
    if let Err(error) = agent_task_service::preflight_cook_continuation_admission(&options) {
        return Ok((
            cook_continuation_preflight_report(
                selected_run_id,
                candidate_fingerprint,
                phases,
                "provider_workspace_baseline",
                &error,
            ),
            1,
        ));
    }
    phases.push(serde_json::json!({ "phase": "provider_workspace_baseline", "status": "passed", "reason": "ok" }));
    phases.push(
        serde_json::json!({ "phase": "candidate_admission", "status": "passed", "reason": "ok" }),
    );
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-continue-preflight/v1",
            "admitted": true,
            "selected_attempt": { "run_id": selected_run_id },
            "candidate_fingerprint": candidate_fingerprint,
            "phases": phases,
            "side_effects": { "provider_dispatch": false, "git_mutation": false, "github_mutation": false, "finalization": false }
        }),
        0,
    ))
}

fn cook_continuation_preflight_report(
    selected_run_id: Option<String>,
    candidate_fingerprint: Value,
    mut phases: Vec<Value>,
    phase: &str,
    error: &homeboy::core::Error,
) -> Value {
    phases.push(serde_json::json!({ "phase": phase, "status": "blocked", "reason": format!("{:?}", error.code), "message": error.message }));
    serde_json::json!({
        "schema": "homeboy/agent-task-cook-continue-preflight/v1",
        "admitted": false,
        "selected_attempt": { "run_id": selected_run_id },
        "candidate_fingerprint": candidate_fingerprint,
        "phases": phases,
        "side_effects": { "provider_dispatch": false, "git_mutation": false, "github_mutation": false, "finalization": false }
    })
}

fn cook_terminal_continuation_status(
    cook_id: &str,
    run_id: &str,
    provider_state: &str,
    state: agent_task_service::CookContinuationState,
) -> Value {
    let (status, guidance) = match state {
        agent_task_service::CookContinuationState::Pending => (
            "continuation_pending",
            serde_json::json!({
                "action": "await_continuation_claim",
                "command": format!("homeboy agent-task cook-continue {run_id}"),
            }),
        ),
        agent_task_service::CookContinuationState::Claimed => (
            "continuation_in_progress",
            serde_json::json!({
                "action": "await_claimed_continuation",
                "command": format!("homeboy agent-task status {run_id} --full"),
            }),
        ),
        agent_task_service::CookContinuationState::Failed => (
            "continuation_recovery_required",
            serde_json::json!({
                "action": "rearm_failed_continuation",
                "command": format!("homeboy agent-task cook-continue {run_id} --rearm"),
            }),
        ),
        agent_task_service::CookContinuationState::Completed => (
            "continuation_completed",
            serde_json::json!({
                "action": "inspect_completed_cook",
                "command": format!("homeboy agent-task status {run_id} --full"),
            }),
        ),
        agent_task_service::CookContinuationState::Absent => (
            "continuation_not_scheduled",
            serde_json::json!({
                "action": "reconcile_terminal_attempt",
                "command": format!("homeboy agent-task reconcile {run_id} --dry-run"),
            }),
        ),
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": status,
        "provider": { "state": provider_state, "run_id": run_id },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "guidance": guidance,
    })
}

fn cook_continuation_status(
    cook_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Value {
    let run_id = &record.run_id;
    let runner_id = record.runner_id();
    let waiting_for_capacity = record
        .metadata
        .pointer("/runner_queue/state")
        .and_then(Value::as_str)
        == Some("waiting_for_capacity");
    let runner_disconnected = record
        .metadata
        .get("runner_liveness")
        .and_then(Value::as_str)
        == Some("disconnected");
    let (status, guidance) = if runner_disconnected {
        (
            "recovery_required",
            serde_json::json!({
                "action": "reconcile_runner_authority",
                "command": format!("homeboy agent-task reconcile {run_id} --dry-run"),
                "message": "The runner could not be reached during bounded reconciliation; inspect the scoped recovery before retrying provider work."
            }),
        )
    } else if waiting_for_capacity
        || record.state == agent_task_lifecycle::AgentTaskRunState::Queued
    {
        let command = runner_id
            .map(|runner_id| format!("homeboy runner status {runner_id}"))
            .unwrap_or_else(|| format!("homeboy agent-task run {run_id}"));
        (
            "accepted_unscheduled",
            serde_json::json!({
                "action": if runner_id.is_some() { "await_runner_capacity" } else { "schedule_queued_run" },
                "command": command,
                "message": if runner_id.is_some() {
                    "The runner accepted this Cook and owns its FIFO queue entry; it will schedule provider execution when a capacity lease is available."
                } else {
                    "This Cook is durably queued but has no runner or provider boundary; schedule its queued run before expecting provider work."
                }
            }),
        )
    } else {
        (
            "observation_in_progress",
            serde_json::json!({
                "action": "watch_provider",
                "command": format!("homeboy agent-task logs {run_id}"),
                "message": "The runner reports active provider work; follow the durable observation until its terminal result is projected."
            }),
        )
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": status,
        "provider": {
            "state": format!("{:?}", record.state),
            "run_id": run_id,
            "runner_id": runner_id,
            "runner_job_status": record.metadata.get("runner_job_status"),
        },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "guidance": guidance,
    })
}

fn cook_report_with_continuation(mut value: Value) -> Value {
    if value.get("status").and_then(Value::as_str) != Some("in_flight") {
        return value;
    }
    let run_id = value
        .get("latest_run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_state = value
        .get("attempts")
        .and_then(Value::as_array)
        .and_then(|attempts| attempts.last())
        .and_then(|attempt| attempt.get("run_state"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Value::Object(report) = &mut value {
        report.insert(
            "provider".to_string(),
            serde_json::json!({ "state": provider_state, "run_id": run_id }),
        );
        report.insert(
            "remaining_phases".to_string(),
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"]),
        );
        report.insert(
            "continuation_command".to_string(),
            serde_json::json!(format!("homeboy agent-task cook-continue {run_id}")),
        );
    }
    value
}

/// Converge a Cook promotion destination before compiling a task plan. This is
/// controller-owned so local and Lab dispatch use the same managed checkout.
pub(crate) fn provision_cook_destination(args: &AgentTaskCookArgs) -> homeboy::core::Result<Value> {
    let to_worktree = args.to_worktree.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--to-worktree is required before provisioning a Cook destination".to_string(),
        ])
    })?;
    let direct_path = Path::new(to_worktree);
    if direct_path.is_dir() {
        homeboy::core::worktree_providers::validate_task_worktree_root(direct_path, to_worktree)?;
        let path = std::fs::canonicalize(direct_path).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(direct_path.display().to_string()),
            )
        })?;
        return Ok(serde_json::json!({
            "action": "existing",
            "kind": "direct_task_worktree",
            "handle": to_worktree,
            "path": path,
        }));
    }
    if let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(to_worktree)? {
        if record.state() != &homeboy::core::worktree::TaskWorktreeState::Active {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace `{}` is no longer active",
                    record.handle()
                ),
                Some(to_worktree.to_string()),
                None,
            ));
        }
        let path = PathBuf::from(record.path());
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, to_worktree)?;
        return Ok(
            serde_json::json!({ "action": "existing", "kind": record.source_kind(), "handle": to_worktree, "path": path }),
        );
    }

    let config = defaults::load_config();
    match homeboy::core::worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
        to_worktree,
        &config,
        None,
    ) {
        Ok(resolution) => {
            homeboy::core::worktree_providers::validate_task_worktree_root(
                Path::new(&resolution.worktree.path),
                to_worktree,
            )?;
            return Ok(
                serde_json::json!({ "action": "existing", "kind": "provider", "provider": resolution.provider_id, "handle": resolution.worktree.handle, "path": resolution.worktree.path, "branch": resolution.worktree.branch }),
            );
        }
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("not_found") => {}
        Err(error) => return Err(error),
    }

    let repo = args.dispatch.repo.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--repo <repo> is required to create a missing --to-worktree destination".to_string(),
        ])
    })?;
    let head = args.head.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--head <branch> is required to create a missing --to-worktree destination".to_string(),
        ])
    })?;
    let task_url = args.dispatch.task_url.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--task-url <url> is required to create a missing --to-worktree destination"
                .to_string(),
        ])
    })?;
    provision_apply_enabled_worktree_provider_from_config(
        &WorktreeProviderCreateIntent {
            handle: to_worktree.to_string(),
            repo,
            base: args.base.clone(),
            head,
            task_url,
        },
        &config,
    )
    .map(|provision| {
        serde_json::json!({
            "action": provision.action,
            "provider": provision.resolution.provider_id,
            "idempotency_key": provision.idempotency_key,
            "handle": provision.resolution.worktree.handle,
            "path": provision.resolution.worktree.path,
            "branch": provision.resolution.worktree.branch,
        })
    })
}

pub(crate) fn resolve_cook_destination(
    mut args: AgentTaskCookArgs,
) -> homeboy::core::Result<AgentTaskCookArgs> {
    if args.to_worktree.is_some() {
        return Ok(args);
    }
    let repo = args.dispatch.repo.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--repo <repo> is required when --to-worktree is omitted".to_string(),
        ])
    })?;
    let task_url = args.dispatch.task_url.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--task-url <url> is required when --to-worktree is omitted".to_string(),
        ])
    })?;
    let config = defaults::load_config();
    // DMC's provider mapping currently exposes safety and handle metadata but
    // not task ownership. In that mode the canonical handle is still resolved
    // first; its ensure operation must reject a different task-owned worktree.
    args.to_worktree = Some(match homeboy::core::worktree_providers::find_apply_enabled_worktree_provider_by_task_url_from_config(task_url, &config) {
        Ok(Some(resolution)) => resolution.worktree.handle,
        Ok(None) => format!("{repo}@{}", slugify_cook_branch(&derived_cook_branch(task_url)?)),
        Err(mut error) => {
            if let Some(handles) = error.message.strip_prefix(&format!("multiple active apply-enabled worktrees are owned by `{task_url}`: ")) {
                error.details["recovery"] = serde_json::json!(handles.split(", ").map(|handle| format!("homeboy agent-task cook --to-worktree {handle}")).collect::<Vec<_>>());
            }
            return Err(error);
        }
    });
    if args.head.is_none() {
        args.head = Some(derived_cook_branch(task_url)?);
    }
    Ok(args)
}

fn derived_cook_branch(task_url: &str) -> homeboy::core::Result<String> {
    let issue = task_url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let Some((repository, number)) = issue.rsplit_once("/issues/") else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must be a GitHub issue URL when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    };
    let number = number.split('/').next().unwrap_or_default();
    if number.is_empty() || !number.chars().all(|character| character.is_ascii_digit()) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must end in a numeric GitHub issue number when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    }
    let mut segments = repository.trim_end_matches('/').rsplit('/');
    let repo = segments.next().unwrap_or_default();
    let owner = segments.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must include a GitHub owner and repo when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    }
    Ok(format!("fix/issue-{number}-{}", slugify_cook_branch(repo)))
}

fn slugify_cook_branch(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn record_cook_provision(plan: &mut AgentTaskPlan, provision: Value) {
    if let Some(task) = plan.tasks.first_mut() {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["worktree_provision"] = provision;
    }
}

pub(crate) fn validate_cook_request(args: &AgentTaskCookArgs) -> homeboy::core::Result<()> {
    validate_cook_request_with_provenance(args, None)
}

/// Reject a Cook whose selected backend cannot execute, before the destination
/// worktree is provisioned.
///
/// `agent-task providers` reporting `available` used to mean only "declared",
/// so a Cook could be dispatched to a backend with no credential, materialize a
/// workspace, and burn its whole execution budget discovering the gap inside
/// the provider — while other configured backends were sitting there usable
/// (#11479). The backend's declared credentials are knowable here, so the
/// remediation is reported instead of a spent budget.
///
/// A backend that cannot be resolved at all is left to the resolution
/// validators; this only speaks about credentials.
pub(crate) fn preflight_cook_provider_credentials(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<()> {
    let dispatch = dispatch_args_for_cook(args);
    let backend = match dispatch.backend.clone() {
        Some(backend) => Some(backend),
        None => provider::default_backend_for_component(dispatch.repo.as_deref())?,
    };
    let Some(backend) = backend else {
        // No backend is resolvable yet; dispatch resolution reports that.
        return Ok(());
    };
    provider::preflight_discovered_provider_credentials_for_backend(
        &backend,
        dispatch.selector.as_deref(),
    )
}

/// Validates Cook input authority before destination provisioning, provider
/// discovery, or any other external effect starts.
pub(crate) fn validate_cook_request_with_provenance(
    args: &AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<()> {
    if args.no_finalize {
        if let Some(provenance) = provenance {
            provenance
                .require_sources(
                    &["no_finalize"],
                    &[crate::cli_surface::ArgumentSource::CommandLine],
                )
                .map_err(|error| {
                    homeboy::core::Error::validation_invalid_argument(
                        "no_finalize",
                        "--no-finalize must be explicitly authorized on the command line",
                        Some(serde_json::to_string(&error).expect("source policy serializes")),
                        None,
                    )
                })?;
        }
    }
    if args.goal.is_some() && !args.dispatch.tasks.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task",
            "agent-task cook uses --goal as framing metadata and --prompt as its one source; --goal and --task conflict",
            None,
            Some(vec![
                "Use: homeboy agent-task cook --goal 'Describe the outcome' --prompt @task.txt --to-worktree sample-plugin@fix-issue --verify 'npm test'".to_string(),
            ]),
        ));
    }
    let dispatch = dispatch_args_for_cook(args);
    dispatch_service::validate_single_cook_prompt_source(
        dispatch.prompt.as_deref(),
        &dispatch.tasks,
        dispatch.core.tasks_json.as_deref(),
    )?;
    if !args.gates.has_deterministic_gate() && !args.no_finalize {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task cook requires at least one deterministic --verify or --private-verify gate before it can commit, push, and open a PR",
            None,
            Some(vec!["Provide a deterministic verification gate, e.g. --verify \"cargo test\".".to_string()]),
        ));
    }
    if args.dispatch.core.queue_only {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "queue-only",
            "agent-task cook cannot queue its controller-owned lifecycle",
            None,
            None,
        ));
    }
    // Resolve against the same filtered rotation policy that compilation uses,
    // while Cook is still in its no-side-effect validation phase.
    let request = dispatch_service::resolve_dispatch_request(dispatch.into())?;
    let configured_rotations = dispatch_service::controller_resolved_execution_policy(&request)
        .rotation
        .as_ref()
        .map(|rotation| {
            rotation
                .max_total_attempts()
                .min(
                    u32::try_from(rotation.entries.len())
                        .unwrap_or(u32::MAX)
                        .saturating_add(1),
                )
                .saturating_sub(1)
        })
        .unwrap_or(0);
    homeboy::agents::agent_task_service::resolve_cook_budget(
        args.max_attempts,
        configured_rotations,
        args.dispatch.core.attempts,
        args.dispatch.core.same_provider_retries,
        args.dispatch.core.provider_rotations,
    )?;
    Ok(())
}

pub(crate) fn promotion_provider(args: PromotionProviderArgs) -> CmdResult<Value> {
    let mut request = Vec::new();
    std::io::stdin()
        .take(MAX_PROMOTION_PROVIDER_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("read agent-task promotion provider request".to_string()),
            )
        })?;
    if request.len() as u64 > MAX_PROMOTION_PROVIDER_REQUEST_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!(
                "promotion provider request exceeds {MAX_PROMOTION_PROVIDER_REQUEST_BYTES} bytes"
            ),
            None,
            None,
        ));
    }
    let request = String::from_utf8(request).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!("promotion provider request is not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    homeboy::agents::agent_task_promotion::apply_materialized_workspace_patch(
        Path::new(&args.workspace),
        &request,
    )
    .and_then(|response| {
        serde_json::from_str(&response).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize agent-task promotion provider response".to_string()),
            )
        })
    })
    .map(|value| (value, 0))
}

pub(super) fn run_cook_with_executor<E>(args: AgentTaskCookArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_cook_with_executor_and_dispatcher(args, executor, None)
}

pub(crate) fn run_cook_with_executor_and_dispatcher<E>(
    args: AgentTaskCookArgs,
    executor: E,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_cook_with_executor_and_dispatcher_with_progress(
        args,
        executor,
        attempt_dispatcher,
        None,
        None,
    )
}

pub(crate) fn run_cook_with_executor_and_dispatcher_with_progress<E>(
    args: AgentTaskCookArgs,
    executor: E,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    progress: super::CookProgress<'_>,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let args = resolve_cook_destination(args)?;
    validate_cook_request_with_provenance(&args, provenance)?;
    // Before any external effect: a backend that cannot execute must say so now
    // rather than after a workspace exists and an execution has been spent
    // (#11479).
    preflight_cook_provider_credentials(&args)?;
    let no_progress = args.no_progress;
    // Deterministic gates exist to make *publication* safe: a green gate is the
    // proof a cook may commit, push, and open a PR. A `--no-finalize` cook does
    // none of those, so a gate is not meaningful there — read-only/exploratory
    // cooks legitimately have nothing to verify. The promotion lifecycle already
    // treats an empty gate set as a vacuously-green run
    // (`run_promotion_gates` -> `PromotionGateRun::without_gates`), so relaxing
    // the requirement here is safe end to end (#7608). Finalizing cooks still
    // require a gate, but now say so with a copy-pasteable example instead of a
    // bare rejection.
    let provision = provision_cook_destination(&args)?;

    let mut dispatch_args = dispatch_args_for_cook(&args);
    // Resolve @file / stdin / stored-ref prompts before anything consumes the
    // prompt, so the executor receives the exact bytes (#10100).
    resolve_dispatch_prompt(&mut dispatch_args)?;
    let requested_cook_id = dispatch_args.run_id.clone();
    if let Some(cook_id) = requested_cook_id.as_deref() {
        dispatch_args.run_id = Some(
            args.attempt_run_id
                .clone()
                .unwrap_or_else(|| agent_task_lifecycle::cook_attempt_run_id(cook_id, 1)),
        );
    }
    let run_id = dispatch_args
        .run_id
        .clone()
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    dispatch_args.run_id = Some(run_id.clone());
    let cook_id = requested_cook_id.clone().unwrap_or_else(|| run_id.clone());
    if !no_progress {
        if let Some(progress) = progress {
            progress("preparing", None, None, None)?;
        }
    }
    let (run_id, mut initial_plan) = if let Some(attempt_plan) = args.attempt_plan.as_deref() {
        let run_id = dispatch_args.run_id.clone().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task cook attempt plan requires an attempt run id".to_string(),
            )
        })?;
        (run_id, agent_task_service::read_plan(attempt_plan)?)
    } else {
        let plan = compile_cook_plan(&args, provision.clone())?;
        (run_id, plan)
    };
    if args.attempt_plan.is_some() {
        record_cook_provision(&mut initial_plan, provision);
        record_cook_goal(&mut initial_plan, args.goal.as_deref());
    }
    resolve_cook_execution_budget(&args, &mut initial_plan)?;
    if !no_progress {
        eprintln!(
            "{}",
            cook_resolved_policy_disclosure(args.max_attempts, &initial_plan)
        );
        eprintln!("{}", cook_rotation_disclosure(&initial_plan));
    }
    if let Some(provenance) = provenance {
        record_cook_argument_provenance(&mut initial_plan, provenance);
    }
    if args.require_acceptance {
        let authority = args.acceptance_authority.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "acceptance-authority",
                "--require-acceptance requires --acceptance-authority",
                None,
                None,
            )
        })?;
        let policy = args.acceptance_policy.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "acceptance-policy",
                "--require-acceptance requires --acceptance-policy",
                None,
                None,
            )
        })?;
        if authority.trim().is_empty() || policy.trim().is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "acceptance",
                "--require-acceptance requires non-empty --acceptance-authority and --acceptance-policy",
                None,
                None,
            ));
        }
        initial_plan.metadata["acceptance"] =
            serde_json::json!({ "authority": authority, "policy": policy });
    }
    // Capture the resolved task workspace before dispatch. The provider may
    // commit and leave a clean tree, so resolving this after it runs would
    // silently widen the promotion range.
    let source_worktree_path = initial_plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(std::path::PathBuf::from);
    let task_base_sha = source_worktree_path.as_deref().and_then(git_head_sha);
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| default_loop_title(&args));
    let commit_message = args
        .commit_message
        .clone()
        .unwrap_or_else(|| default_loop_commit_message(&args));
    let selected_model = initial_plan
        .tasks
        .first()
        .and_then(|task| task.executor.model())
        .map(str::to_string);
    let durable_observer = |event: &agent_task_service::CookProgressEvent<'_>| {
        if no_progress && event.phase != "durable_identity" {
            return Ok(());
        }
        // The observer renders the activity sample here rather than passing the
        // struct on, so foreground clients (TTY, machine log, `--output` file)
        // all describe a running provider with the same bounded sentence.
        let activity = event.activity_summary();
        progress
            .map(|progress| {
                progress(
                    event.phase,
                    Some(event.cook_id),
                    Some(event.run_id),
                    activity.as_deref(),
                )
            })
            .unwrap_or(Ok(()))
    };
    let result = agent_task_service::run_cook_with_durable_observer(
        agent_task_service::AgentTaskCookServiceOptions {
            cook_id,
            initial_run_id: run_id,
            initial_plan,
            to_worktree: args.to_worktree.expect("Cook destination is resolved"),
            source_worktree_path,
            provider_command: args.provider_command,
            provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
                argv: args.provider_argv,
                ..Default::default()
            }),
            gates: args.gates.into(),
            max_attempts: args.max_attempts,
            no_finalize: args.no_finalize,
            draft_pr: args.draft_pr,
            base: args.base,
            task_base_sha,
            head: args.head,
            title,
            commit_message,
            source_refs: args.dispatch.task_url.into_iter().collect(),
            protected_branches: args.protected_branches,
            ai_tool: super::fanout::resolve_ai_tool_disclosure(
                &args.ai_tool,
                args.dispatch.backend.as_deref(),
                args.dispatch.selector.as_deref(),
                args.dispatch.model.as_deref(),
            ),
            // Model identity comes only from explicit/config/rotation selection
            // (`--model`, provider profile). Disclosure text like
            // `OpenCode (GPT-5.5)` is presentation, not execution provenance, and
            // must not be reverse-parsed into a model — an omitted model stays
            // omitted so finalization records only real model identity (#9789).
            ai_model: selected_model,
            ai_used_for: args.ai_used_for,
            attempt_dispatcher,
            harvest_context:
                homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process(
                )?,
        },
        executor,
        &durable_observer,
    )?;
    Ok((
        super::status::compact_cook_report(
            cook_report_with_continuation(
                serde_json::to_value(result.value).unwrap_or(Value::Null),
            ),
            args.full,
        ),
        result.exit_code,
    ))
}

pub(crate) fn record_cook_argument_provenance(
    plan: &mut AgentTaskPlan,
    provenance: &crate::cli_surface::CommandArgumentProvenance,
) {
    provenance.project_into(&mut plan.metadata);
}

pub(super) fn dispatch_args_for_cook(args: &AgentTaskCookArgs) -> DispatchArgs {
    let mut dispatch_args = args.dispatch.clone();
    let has_explicit_work = dispatch_args.prompt.is_some()
        || !dispatch_args.tasks.is_empty()
        || dispatch_args.core.tasks_json.is_some();
    if !has_explicit_work {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args
}

fn resolve_cook_execution_budget(
    args: &AgentTaskCookArgs,
    plan: &mut AgentTaskPlan,
) -> homeboy::core::Result<()> {
    let core = &args.dispatch.core;
    let resolved = homeboy::agents::agent_task_service::resolve_cook_budget(
        args.max_attempts,
        plan.options.execution_budget.max_provider_rotations,
        core.attempts,
        core.same_provider_retries,
        core.provider_rotations,
    )?;
    plan.options.execution_budget =
        homeboy::agents::agent_task_scheduler::AgentTaskExecutionBudget::new(
            resolved.provider_executions,
            resolved.same_provider_remediations,
            resolved.provider_rotations,
        );
    plan.metadata["cook_retry_policy"] = serde_json::json!({
        "operator_intent": {
            "max_attempts": args.max_attempts,
            "max_provider_executions": core.attempts,
            "max_same_provider_retries": core.same_provider_retries,
            "max_provider_rotations": core.provider_rotations,
        },
        "resolved": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.provider_rotations,
        },
    });
    Ok(())
}

/// Resolve `--prompt` through the structured-input contract its help already
/// advertises: `@file`, `-` for stdin, and stored `@prompt:<id>` references.
///
/// Cook previously took `--prompt` as a literal string, so a large Markdown
/// prompt had to travel as one shell argument. Backticks, `$` expressions and
/// quotes were then interpreted by the caller's shell before Homeboy ever saw
/// them — in one case executing prompt text as commands (#10100).
///
/// Bytes and newlines are preserved exactly; no trimming.
pub(super) fn resolve_dispatch_prompt(
    dispatch_args: &mut DispatchArgs,
) -> homeboy::core::Result<()> {
    let Some(spec) = dispatch_args.prompt.as_deref() else {
        return Ok(());
    };
    let resolved = homeboy::agents::agent_task_prompts::read_prompt_input(spec)?;
    dispatch_args.prompt = Some(resolved);
    Ok(())
}

#[cfg(test)]
mod rotation_disclosure_tests {
    use super::*;
    use homeboy::agents::agent_task_scheduler::{
        AgentTaskExecutionBudget, AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
    };

    fn plan_with(executions: u32, rotations: u32, entries: usize) -> AgentTaskPlan {
        let mut plan = AgentTaskPlan::new("cook-rotation-disclosure".to_string(), Vec::new());
        plan.options.execution_budget = AgentTaskExecutionBudget::new(executions, 0, rotations);
        plan.options.rotation = (entries > 0).then(|| AgentTaskProviderRotationPolicy {
            entries: (0..entries)
                .map(|index| AgentTaskProviderRotationEntry {
                    model: Some(format!("fallback-model-{index}")),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
        plan
    }

    #[test]
    fn a_funded_rotation_states_the_providers_and_executions_it_will_use() {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(3, 2, 2)),
            "cook: rotation: 2 fallback provider(s), up to 3 provider execution(s)"
        );
    }

    #[test]
    fn no_rotation_at_all_states_that_plainly() {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(1, 0, 0)),
            "cook: rotation: disabled (1 provider execution(s))"
        );
    }

    /// The silence that let #11082 survive: a policy was configured, carried,
    /// and unreachable, and nothing said so.
    #[test]
    fn a_configured_but_unfunded_rotation_is_named_as_unreachable() {
        let disclosure = cook_rotation_disclosure(&plan_with(1, 0, 3));

        assert!(disclosure.contains("disabled"), "{disclosure}");
        assert!(
            disclosure.contains("3 configured rotation provider(s) are unreachable"),
            "{disclosure}"
        );
    }

    /// A rotation budget that outruns the execution budget can never fire.
    #[test]
    fn executions_bound_the_rotations_the_disclosure_promises() {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(2, 5, 5)),
            "cook: rotation: 1 fallback provider(s), up to 2 provider execution(s)"
        );
    }
}

#[cfg(test)]
mod prompt_input_tests {
    use super::*;

    fn dispatch_with_prompt(prompt: Option<&str>) -> DispatchArgs {
        DispatchArgs {
            prompt: prompt.map(str::to_string),
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            repo: None,
            task_url: None,
            backend: None,
            selector: None,
            model: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            concurrency: 1,
            run_id: None,
            core: crate::commands::agent_task_dispatch::DispatchCoreArgs {
                tasks_json: None,
                provider_config: None,
                client_context: None,
                attempts: Some(1),
                same_provider_retries: Some(0),
                provider_rotations: Some(0),
                queue_only: false,
                timeout_ms: None,
                resolved_provider_policy: None,
                deny_command: Vec::new(),
                allow_command: Vec::new(),
                command_policy_reason: None,
            },
        }
    }

    /// Shell-sensitive prompt content must reach the executor byte-for-byte,
    /// with no interpolation and no trimming (#10100).
    #[test]
    fn at_file_prompt_is_read_verbatim() {
        let file = tempfile::NamedTempFile::new().expect("prompt file");
        let body = "Run `cargo test` for $HOME\n\nUse \"quotes\" and 'apostrophes'.\n";
        std::fs::write(file.path(), body).expect("write prompt");

        let mut args = dispatch_with_prompt(Some(&format!("@{}", file.path().display())));
        resolve_dispatch_prompt(&mut args).expect("resolve @file prompt");

        assert_eq!(args.prompt.as_deref(), Some(body));
    }

    #[test]
    fn inline_prompt_is_unchanged() {
        let mut args = dispatch_with_prompt(Some("fix the flaky test"));
        resolve_dispatch_prompt(&mut args).expect("resolve inline prompt");

        assert_eq!(args.prompt.as_deref(), Some("fix the flaky test"));
    }

    #[test]
    fn missing_at_file_is_reported_not_passed_through() {
        let mut args = dispatch_with_prompt(Some("@/definitely/not/a/prompt/file/10100"));
        resolve_dispatch_prompt(&mut args).expect_err("a missing prompt file must fail");
    }

    #[test]
    fn empty_at_prefix_is_rejected() {
        let mut args = dispatch_with_prompt(Some("@"));
        let error = resolve_dispatch_prompt(&mut args).expect_err("bare @ is not a path");
        assert!(error.message.contains("missing file path"), "{error:?}");
    }

    #[test]
    fn absent_prompt_is_a_no_op() {
        let mut args = dispatch_with_prompt(None);
        resolve_dispatch_prompt(&mut args).expect("no prompt is fine");
        assert!(args.prompt.is_none());
    }
}

/// Compile the one durable provider-cell plan used by local Cook and Lab handoff.
pub(crate) fn compile_cook_plan(
    args: &AgentTaskCookArgs,
    provision: Value,
) -> homeboy::core::Result<AgentTaskPlan> {
    let workspace = provision
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "Cook destination provisioning did not return a task worktree path".to_string(),
            )
        })?
        .to_string();
    let mut dispatch = dispatch_args_for_cook(args);
    // Cook providers always receive the declared task checkout. `--cwd` is a
    // dispatch input, never authority to replace the writable Cook workspace.
    dispatch.cwd = None;
    dispatch.workspace = Some(workspace);
    let mut request = dispatch_service::resolve_dispatch_request(dispatch.into())?;
    let mut plan = match dispatch_service::build_controller_dispatch_plan(&mut request) {
        Ok(plan) => plan,
        // A managed promotion handle may be intentionally unavailable until
        // after provider execution. Keep the compiled task durable and let
        // promotion report its established controlled policy failure.
        Err(error)
            if request.workspace.as_deref() == args.to_worktree.as_deref()
                && error.message.contains(
                    "neither an existing directory nor a resolvable managed worktree handle",
                ) =>
        {
            request.workspace = None;
            dispatch_service::build_controller_dispatch_plan(&mut request)?
        }
        Err(error) => return Err(error),
    };
    plan.options.candidate_completion = args.candidate_completion;
    record_cook_provision(&mut plan, provision);
    for task in &mut plan.tasks {
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "workspace",
                "Cook requires a bound task worktree",
                None,
                None,
            )
        })?;
        task.metadata["cook_workspace_identity"] = workspace_identity_attestation(Path::new(root))?;
    }
    homeboy::agents::agent_task_provider::AgentTaskProviderCatalog::discover()
        .validate_explicit_models(&plan)?;
    record_cook_goal(&mut plan, args.goal.as_deref());
    Ok(plan)
}

#[cfg(unix)]
fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(canonical.display().to_string()))
    })?;
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(&canonical)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let git_file = canonical.join(".git");
    let git_metadata = std::fs::symlink_metadata(&git_file).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let git_content = std::fs::read_to_string(&git_file).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let gitdir_target = git_content
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .and_then(|target| std::fs::canonicalize(canonical.join(target)).ok());
    Ok(
        serde_json::json!({ "canonical_path": canonical, "device": metadata.dev(), "inode": metadata.ino(), "git_dir": git_dir, "git_file_is_file": git_metadata.file_type().is_file(), "git_file_content": git_content, "gitdir_target": gitdir_target }),
    )
}

#[cfg(not(unix))]
fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    Ok(serde_json::json!({ "canonical_path": canonical }))
}

fn record_cook_goal(plan: &mut AgentTaskPlan, goal: Option<&str>) {
    let Some(goal) = goal else {
        return;
    };
    if !plan.metadata.is_object() {
        plan.metadata = serde_json::json!({});
    }
    plan.metadata["cook_goal"] = serde_json::json!(goal);
    for task in &mut plan.tasks {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["cook_goal"] = serde_json::json!(goal);
    }
}

fn git_head_sha(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn default_loop_title(args: &AgentTaskCookArgs) -> String {
    let target = args
        .dispatch
        .repo
        .as_deref()
        .or(args.dispatch.task_url.as_deref())
        .unwrap_or("agent task");
    format!("Cook {target}")
}

fn default_loop_commit_message(args: &AgentTaskCookArgs) -> String {
    let target = args.dispatch.repo.as_deref().unwrap_or("agent task");
    format!("fix: cook {target}")
}

pub(super) fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let mut plan = agent_task_service::read_plan(&args.plan)?;
    if let Some(timeout_ms) = args.timeout_ms {
        plan.options.timeout_ms = Some(timeout_ms);
    }
    emit_runner_lifecycle_progress(&plan, args.record_run_id.as_deref());
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn emit_runner_lifecycle_progress(plan: &AgentTaskPlan, run_id: Option<&str>) {
    if std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV).is_none() {
        return;
    }
    for task in &plan.tasks {
        println!(
            "HOMEBOY_RUNNER_PROGRESS {}",
            serde_json::json!({
                "schema": "homeboy/runner-progress/v1",
                "phase": "provider_dispatch",
                "current_item": task.task_id,
                "metadata": {
                    "agent_task_run_id": run_id,
                    "task_id": task.task_id,
                    "provider": task.executor.backend,
                    "event": "provider_selected",
                },
            })
        );
    }
}

pub(super) fn run_loaded_plan<E>(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_loaded_plan(plan, record_run_id, executor)?;
    let value =
        if std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV).is_some() {
            aggregate_value_with_failure_reasons(&result.value)
        } else {
            super::status::compact_aggregate_summary(&result.value, record_run_id)
        };
    Ok((value, result.exit_code))
}

pub(super) fn run_submitted(args: RunArgs) -> CmdResult<Value> {
    run_submitted_with_executor(
        args.run_id,
        args.timeout_ms,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

pub(super) fn run_submitted_with_executor<E>(
    run_id: String,
    timeout_ms: Option<u64>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result =
        agent_task_service::run_submitted_with_timeout(run_id.clone(), timeout_ms, executor)?;
    Ok((
        super::status::compact_aggregate_summary(&result.value, Some(&run_id)),
        result.exit_code,
    ))
}

pub(super) fn run_next() -> CmdResult<Value> {
    run_next_with_executor(ExtensionProviderAgentTaskExecutor::discover())
}

pub(super) fn run_next_with_executor<E>(executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let result = agent_task_service::run_next_with_cook_dispatcher(
        executor,
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )?;
    let Some(aggregate) = result.value else {
        return Ok((serde_json::json!({ "claimed": false }), 0));
    };
    Ok((
        aggregate_value_with_failure_reasons(&aggregate),
        result.exit_code,
    ))
}

pub(super) fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let record = agent_task_service::submit_plan_spec(&args.plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

pub(super) fn resume(args: impl Into<LifecycleReadArgs>) -> CmdResult<Value> {
    let args = args.into();
    run_resume_with_executor_and_bridge(
        args.run_id,
        args.bridge,
        args.since_cursor,
        args.full,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

pub(super) fn run_resume_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    run_resume_with_executor_and_bridge(run_id, false, None, false, executor)
}

pub(super) fn run_resume_with_executor_and_bridge<E>(
    run_id: String,
    bridge: bool,
    since_cursor: Option<u64>,
    full: bool,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let needs_transport_recovery =
        bridge && agent_task_service::terminal_transport_recovery_required(&run_id);
    if needs_transport_recovery {
        // Recover the authenticated terminal runner snapshot before `resume`
        // can short-circuit on a previously persisted lossy aggregate.
        agent_task_service::recover_terminal_transport_proxy_evidence(&run_id)?;
    }
    let result = agent_task_service::resume(run_id.clone(), executor)?;
    if bridge {
        // Resume first imports authoritative terminal runner evidence when the
        // local aggregate is absent. Reproject only after that shared recovery
        // contract has persisted the aggregate and identity.
        agent_task_service::reconcile_terminal_artifact_projection(&run_id)?;
        let status = agent_task_service::run_status(&run_id, since_cursor)?;
        return Ok((
            serde_json::to_value(status).unwrap_or(Value::Null),
            result.exit_code,
        ));
    }
    Ok((
        if full {
            aggregate_value_with_failure_reasons(&result.value)
        } else {
            super::status::compact_aggregate_summary(&result.value, Some(&run_id))
        },
        result.exit_code,
    ))
}

pub(super) fn retry(args: RetryArgs) -> CmdResult<Value> {
    let result = agent_task_service::retry(
        &args.run_id,
        args.new_run_id.as_deref(),
        args.run,
        args.force,
    )?;
    if result.run {
        return run_submitted_with_executor(
            result.record.run_id,
            None,
            ExtensionProviderAgentTaskExecutor::discover(),
        );
    }
    Ok((
        serde_json::to_value(result.record).unwrap_or(Value::Null),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        cook_continuation_status, cook_report_with_continuation, cook_resolved_policy_disclosure,
        durable_cook_identity_lines, preflight_continue_cook,
    };
    use crate::commands::agent_task::args::CookContinueArgs;

    #[test]
    fn durable_cook_identity_block_leads_with_the_run_id_and_follow_up_commands() {
        let lines = durable_cook_identity_lines(Some("cook-10419"), "cook-10419-attempt-1");

        // The very first thing an interrupted caller sees must be the handle.
        assert!(
            lines[0].contains("cook-10419-attempt-1"),
            "identity block must lead with the durable run id: {lines:?}"
        );
        assert!(lines[0].contains("cook-10419"));
        let joined = lines.join("\n");
        assert!(joined.contains("homeboy agent-task status cook-10419-attempt-1"));
        assert!(joined.contains("homeboy agent-task logs cook-10419-attempt-1"));
        assert!(joined.contains("homeboy agent-task cancel cook-10419-attempt-1"));
    }

    #[test]
    fn durable_cook_identity_block_omits_a_cook_alias_equal_to_the_run_id() {
        let lines = durable_cook_identity_lines(Some("run-9163"), "run-9163");

        assert!(!lines[0].contains("(cook"), "no redundant alias: {lines:?}");
        assert!(lines[0].contains("run-9163"));
    }

    #[test]
    fn resolved_cook_policy_disclosure_reports_the_execution_budget() {
        let mut plan = homeboy::agents::agent_task_scheduler::AgentTaskPlan::new("plan", vec![]);
        plan.options.execution_budget =
            homeboy::agents::agent_task_scheduler::AgentTaskExecutionBudget::new(4, 1, 2);

        assert_eq!(
            cook_resolved_policy_disclosure(2, &plan),
            "cook: retry policy: 1 initial execution, 1 same-provider remediation retry(ies), 2 rotation(s), 4 provider execution(s) maximum"
        );
    }

    #[test]
    fn in_flight_cook_report_keeps_provider_state_and_managed_continuation_separate() {
        let report = cook_report_with_continuation(serde_json::json!({
            "cook_id": "cook-1",
            "latest_run_id": "cook-1-attempt-1",
            "status": "in_flight",
            "attempts": [{ "run_state": "Running" }],
        }));

        assert_eq!(report["provider"]["state"], "Running");
        assert_eq!(
            report["remaining_phases"],
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"])
        );
        assert_eq!(
            report["continuation_command"],
            "homeboy agent-task cook-continue cook-1-attempt-1"
        );
    }

    #[test]
    fn cook_continue_reports_runner_capacity_without_offering_a_duplicate_dispatch() {
        crate::test_support::with_isolated_home(|_| {
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![]);
            homeboy::agents::agent_tasks::lifecycle::submit_plan(
                &plan,
                Some("cook-queue-attempt-1"),
            )
            .expect("persist queued attempt");
            homeboy::agents::agent_tasks::lifecycle::rewrite_record_for_test(
                "cook-queue-attempt-1",
                |record| {
                    record.metadata = serde_json::json!({
                        "runner_id": "fixture-lab",
                        "runner_job_status": "queued",
                        "runner_queue": { "state": "waiting_for_capacity" },
                    });
                },
            )
            .expect("record accepted queue ownership");
            let record = homeboy::agents::agent_tasks::lifecycle::status("cook-queue-attempt-1")
                .expect("read queued attempt");

            let report = cook_continuation_status("cook-queue", &record);

            assert_eq!(report["status"], "accepted_unscheduled");
            assert_eq!(report["guidance"]["action"], "await_runner_capacity");
            assert_eq!(
                report["guidance"]["command"],
                "homeboy runner status fixture-lab"
            );
            assert!(report.get("continuation_command").is_none());
        });
    }

    #[test]
    fn cook_continue_reports_a_queued_record_without_runner_as_unscheduled() {
        crate::test_support::with_isolated_home(|_| {
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![]);
            homeboy::agents::agent_tasks::lifecycle::submit_plan(
                &plan,
                Some("cook-unassigned-attempt-1"),
            )
            .expect("persist queued attempt");
            let record =
                homeboy::agents::agent_tasks::lifecycle::status("cook-unassigned-attempt-1")
                    .expect("read queued attempt without a runner boundary");

            let report = cook_continuation_status("cook-unassigned", &record);

            assert_eq!(report["status"], "accepted_unscheduled");
            assert_eq!(report["guidance"]["action"], "schedule_queued_run");
            assert_eq!(
                report["guidance"]["command"],
                "homeboy agent-task run cook-unassigned-attempt-1"
            );
            assert!(report.get("continuation_command").is_none());
        });
    }

    #[test]
    fn cook_continue_reports_active_runner_work_with_a_watch_command() {
        crate::test_support::with_isolated_home(|_| {
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![]);
            homeboy::agents::agent_tasks::lifecycle::submit_plan(
                &plan,
                Some("cook-active-attempt-1"),
            )
            .expect("persist active attempt");
            homeboy::agents::agent_tasks::lifecycle::mark_running("cook-active-attempt-1")
                .expect("runner starts the provider attempt");
            homeboy::agents::agent_tasks::lifecycle::rewrite_record_for_test(
                "cook-active-attempt-1",
                |record| {
                    record.metadata = serde_json::json!({
                        "runner_id": "fixture-lab",
                        "runner_job_status": "running",
                        "provider_run_ids": ["fixture-provider-run-1"],
                        "phase": "provider_execution",
                    });
                },
            )
            .expect("record active runner and provider evidence");
            let record = homeboy::agents::agent_tasks::lifecycle::status("cook-active-attempt-1")
                .expect("read active runner-backed provider attempt");

            let report = cook_continuation_status("cook-active", &record);

            assert_eq!(report["status"], "observation_in_progress");
            assert_eq!(report["guidance"]["action"], "watch_provider");
            assert_eq!(
                report["guidance"]["command"],
                "homeboy agent-task logs cook-active-attempt-1"
            );
            assert!(report.get("continuation_command").is_none());
        });
    }

    #[test]
    fn cook_continue_preflight_reports_a_read_only_rejection_without_creating_a_run() {
        crate::test_support::with_isolated_home(|_| {
            let before = homeboy::agents::agent_tasks::lifecycle::list_records()
                .expect("read initial lifecycle records");
            let (report, exit_code) = preflight_continue_cook(CookContinueArgs {
                cook_or_attempt_id: "missing-cook".to_string(),
                preflight: true,
                rearm: false,
                full: false,
            })
            .expect("preflight returns a machine-readable rejection");
            let after = homeboy::agents::agent_tasks::lifecycle::list_records()
                .expect("read lifecycle records after preflight");

            assert_eq!(exit_code, 1);
            assert_eq!(report["admitted"], false);
            assert_eq!(report["phases"][0]["phase"], "recipe");
            assert_eq!(report["side_effects"]["provider_dispatch"], false);
            assert_eq!(report["side_effects"]["git_mutation"], false);
            assert_eq!(report["side_effects"]["github_mutation"], false);
            assert_eq!(report["side_effects"]["finalization"], false);
            assert_eq!(
                before.len(),
                after.len(),
                "preflight must not materialize a run"
            );
        });
    }
}
