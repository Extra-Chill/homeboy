//! Durable run lifecycle handlers: cook, run-plan, run, run-next, submit,
//! resume, and retry.

use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use homeboy::core::agent_tasks::dispatch_service;
use homeboy::core::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy::core::agent_tasks::service as agent_task_service;
use homeboy::core::command_invocation::CommandInvocation;

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
    let aggregate = homeboy::core::agent_task_artifacts::reviewer_facing_aggregate(aggregate);
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
    homeboy::core::agent_task_promotion::apply_materialized_workspace_patch(
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
        Arc<dyn crate::core::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    if !args.gates.has_deterministic_gate() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task cook requires at least one deterministic --verify or --private-verify gate",
            None,
            None,
        ));
    }
    if args.dispatch.core.queue_only {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "queue-only",
            "agent-task cook cannot queue its controller-owned lifecycle; it must retain provider completion to ingest artifacts, promote candidates, run gates, and finalize",
            None,
            Some(vec![
                "Use `homeboy agent-task run-plan --plan <materialized-plan> --record-run-id <run-id> --queue-only` only when a controller owns the corresponding continuation.".to_string(),
            ]),
        ));
    }

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
    let (run_id, initial_plan) = if let Some(attempt_plan) = args.attempt_plan.as_deref() {
        let run_id = dispatch_args.run_id.clone().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task cook attempt plan requires an attempt run id".to_string(),
            )
        })?;
        (run_id, agent_task_service::read_plan(attempt_plan)?)
    } else {
        let run_id = dispatch_args
            .run_id
            .clone()
            .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
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
    let cook_id = requested_cook_id.unwrap_or_else(|| run_id.clone());
    // Capture the resolved task workspace before dispatch. The provider may
    // commit and leave a clean tree, so resolving this after it runs would
    // silently widen the promotion range.
    let source_worktree_path = initial_plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(std::path::PathBuf::from);
    let task_base_sha = source_worktree_path.as_deref().and_then(git_head_sha);
    if let Some(dispatcher) = &attempt_dispatcher {
        dispatcher.prepare_for_cook()?;
    }
    // Keep this cook on one runtime generation through provider work and PR finalization.
    let _runtime_generation = homeboy::core::runtime_promotion::pin_cook_generation(&cook_id)?;
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| default_loop_title(&args));
    let commit_message = args
        .commit_message
        .clone()
        .unwrap_or_else(|| default_loop_commit_message(&args));
    let result = agent_task_service::run_cook(
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
            ai_tool: args.ai_tool.clone(),
            ai_model: args
                .dispatch
                .model
                .or_else(|| agent_task_service::ai_model_from_tool(&args.ai_tool)),
            ai_used_for: args.ai_used_for,
            attempt_dispatcher,
            harvest_context:
                homeboy::core::agent_task_scheduler::HarvestExecutionContext::from_current_process(
                )?,
        },
        executor,
    )?;
    Ok((
        serde_json::to_value(result.value).unwrap_or(Value::Null),
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
    if std::env::var_os("HOMEBOY_LAB_RUNNER_ID").is_none() {
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
    Ok((
        aggregate_value_with_failure_reasons(&result.value),
        result.exit_code,
    ))
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
    let result = agent_task_service::run_submitted_with_timeout(run_id, timeout_ms, executor)?;
    Ok((
        aggregate_value_with_failure_reasons(&result.value),
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
    run_resume_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

pub(super) fn run_resume_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::resume(run_id, executor)?;
    Ok((
        aggregate_value_with_failure_reasons(&result.value),
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
