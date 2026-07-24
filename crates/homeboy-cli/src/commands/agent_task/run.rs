//! Durable run lifecycle handlers: cook, run-plan, run, run-next, submit,
//! resume, and retry.

use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use homeboy::agents::agent_tasks::dispatch_service;
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
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

use super::super::CmdResult;
use super::args::{
    AgentTaskCookArgs, PromotionProviderArgs, RetryArgs, RunArgs, RunPlanArgs, StatusArgs,
    SubmitArgs,
};

const MAX_PROMOTION_PROVIDER_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

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
    run_cook_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

/// Converge a Cook promotion destination before compiling a task plan. This is
/// controller-owned so local and Lab dispatch use the same managed checkout.
pub(crate) fn provision_cook_destination(args: &AgentTaskCookArgs) -> homeboy::core::Result<Value> {
    let destination = std::path::Path::new(&args.to_worktree);
    if destination.is_dir()
        || homeboy::core::worktree::resolve_workspace_ref_if_present(&args.to_worktree)?.is_some()
    {
        return Ok(
            serde_json::json!({ "action": "existing", "kind": "path_or_homeboy", "handle": args.to_worktree }),
        );
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
            handle: args.to_worktree.clone(),
            repo,
            base: args.base.clone(),
            head,
            task_url,
        },
        &defaults::load_config(),
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

pub(crate) fn record_cook_provision(plan: &mut AgentTaskPlan, provision: Value) {
    if let Some(task) = plan.tasks.first_mut() {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["worktree_provision"] = provision;
    }
}

pub(crate) fn validate_cook_request(args: &AgentTaskCookArgs) -> homeboy::core::Result<()> {
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
    Ok(())
}

pub(super) fn promotion_provider(args: PromotionProviderArgs) -> CmdResult<Value> {
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
    run_cook_with_executor_and_dispatcher_with_progress(args, executor, attempt_dispatcher, None)
}

pub(crate) fn run_cook_with_executor_and_dispatcher_with_progress<E>(
    args: AgentTaskCookArgs,
    executor: E,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    progress: Option<&dyn Fn(&str, Option<&str>, Option<&str>) -> homeboy::core::Result<()>>,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    validate_cook_request(&args)?;
    // Deterministic gates exist to make *publication* safe: a green gate is the
    // proof a cook may commit, push, and open a PR. A `--no-finalize` cook does
    // none of those, so a gate is not meaningful there — read-only/exploratory
    // cooks legitimately have nothing to verify. The promotion lifecycle already
    // treats an empty gate set as a vacuously-green run
    // (`run_promotion_gates` -> `PromotionGateRun::without_gates`), so relaxing
    // the requirement here is safe end to end (#7608). Finalizing cooks still
    // require a gate, but now say so with a copy-pasteable example instead of a
    // bare rejection.
    let mut dispatch_args = args.dispatch.clone();
    if dispatch_args.prompt.is_none() {
        dispatch_args.prompt = args.goal.clone();
    }
    if dispatch_args.cwd.is_none() && dispatch_args.workspace.is_none() {
        dispatch_args.workspace = Some(args.to_worktree.clone());
    }
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
    if let Some(progress) = progress {
        progress("preparing", None, None)?;
    }
    let provision = provision_cook_destination(&args)?;
    let (run_id, mut initial_plan) = if let Some(attempt_plan) = args.attempt_plan.as_deref() {
        let run_id = dispatch_args.run_id.clone().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task cook attempt plan requires an attempt run id".to_string(),
            )
        })?;
        (run_id, agent_task_service::read_plan(attempt_plan)?)
    } else {
        let mut request = dispatch_service::resolve_dispatch_request(dispatch_args.into())?;
        let plan = match dispatch_service::build_controller_dispatch_plan(&mut request) {
            Ok(plan) => plan,
            // A managed promotion handle may be intentionally unavailable until
            // after provider execution. Keep the compiled task durable and let
            // promotion report its established controlled policy failure.
            Err(error)
                if request.workspace.as_deref() == Some(args.to_worktree.as_str())
                    && error.message.contains(
                        "neither an existing directory nor a resolvable managed worktree handle",
                    ) =>
            {
                request.workspace = None;
                dispatch_service::build_controller_dispatch_plan(&mut request)?
            }
            Err(error) => return Err(error),
        };
        (run_id, plan)
    };
    record_cook_provision(&mut initial_plan, provision);
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
    let durable_observer = |cook_id: &str, run_id: &str| {
        progress
            .map(|progress| progress("in_flight", Some(cook_id), Some(run_id)))
            .unwrap_or(Ok(()))
    };
    let result = agent_task_service::run_cook_with_durable_observer(
        agent_task_service::AgentTaskCookServiceOptions {
            cook_id,
            initial_run_id: run_id,
            initial_plan,
            to_worktree: args.to_worktree,
            source_worktree_path,
            provider_command: args.provider_command,
            provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
                argv: args.provider_argv,
                ..Default::default()
            }),
            gates: args.gates.into(),
            max_attempts: args.max_attempts,
            no_finalize: args.no_finalize,
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
            ai_model: args.dispatch.model,
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
            serde_json::to_value(result.value).unwrap_or(Value::Null),
            args.full,
        ),
        result.exit_code,
    ))
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

pub(super) fn resume(args: StatusArgs) -> CmdResult<Value> {
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
    let result = agent_task_service::retry(&args.run_id, args.new_run_id.as_deref(), args.run)?;
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
