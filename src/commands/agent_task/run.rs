//! Durable run lifecycle handlers: loop, run-plan, run, run-next, submit,
//! resume, and retry.

use serde_json::Value;

use homeboy::core::agent_tasks::dispatch_service;
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy::core::agent_tasks::service as agent_task_service;

use super::super::CmdResult;
use super::args::{AgentTaskLoopArgs, RetryArgs, RunPlanArgs, StatusArgs, SubmitArgs};

/// Serialize a completed run aggregate and, when the run did not fully succeed,
/// surface a prominent top-level `failure_reasons` summary so the operator sees
/// the root cause (recipe validation, PHP fatal, provider registration, missing
/// path) without hand-digging the nested outcome JSON (#3806). The full nested
/// payload is preserved unchanged; this only ADDS the surfaced summary.
fn aggregate_value_with_failure_reasons(aggregate: &AgentTaskAggregate) -> Value {
    let mut value = serde_json::to_value(aggregate).unwrap_or(Value::Null);
    let failure_reasons = super::status::failure_reasons_from_aggregate(aggregate);
    if !failure_reasons.is_empty() {
        if let Value::Object(map) = &mut value {
            map.insert("failure_reasons".to_string(), Value::Array(failure_reasons));
        }
    }
    value
}

pub(super) fn run_loop(args: AgentTaskLoopArgs) -> CmdResult<Value> {
    run_loop_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

pub(super) fn run_loop_with_executor<E>(args: AgentTaskLoopArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    if args.gates.verify.is_empty() && args.gates.private_verify.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task loop requires at least one deterministic --verify or --private-verify gate",
            None,
            None,
        ));
    }

    let mut dispatch_args = args.dispatch.clone();
    if dispatch_args.prompt.is_none() {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args.core.queue_only = false;
    let (dispatch_value, _dispatch_exit) =
        dispatch_service::run_dispatch_command(dispatch_args.into(), executor.clone())?;
    let run_id = dispatch_value["run_id"]
        .as_str()
        .ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task dispatch did not return a run_id".to_string(),
            )
        })?
        .to_string();
    let loop_id = run_id.clone();
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| default_loop_title(&args));
    let commit_message = args
        .commit_message
        .clone()
        .unwrap_or_else(|| default_loop_commit_message(&args));
    let result = agent_task_service::run_cook_loop(
        agent_task_service::AgentTaskLoopServiceOptions {
            loop_id,
            initial_run_id: run_id,
            to_worktree: args.to_worktree,
            provider_command: args.provider_command,
            gates: args.gates.into(),
            max_attempts: args.max_attempts,
            no_finalize: args.no_finalize,
            base: args.base,
            head: args.head,
            title,
            commit_message,
            source_refs: args.dispatch.task_url.into_iter().collect(),
            protected_branches: args.protected_branches,
            ai_tool: args.ai_tool.clone(),
            ai_model: args
                .dispatch
                .model
                .or_else(|| ai_model_from_tool(&args.ai_tool)),
            ai_used_for: args.ai_used_for,
        },
        executor,
    )?;
    Ok((
        serde_json::to_value(result.value).unwrap_or(Value::Null),
        result.exit_code,
    ))
}

fn ai_model_from_tool(ai_tool: &str) -> Option<String> {
    let start = ai_tool.find('(')?;
    let end = ai_tool[start + 1..].find(')')? + start + 1;
    let model = ai_tool[start + 1..end].trim();
    (!model.is_empty()).then(|| model.to_string())
}

fn default_loop_title(args: &AgentTaskLoopArgs) -> String {
    let target = args
        .dispatch
        .repo
        .as_deref()
        .or(args.dispatch.task_url.as_deref())
        .unwrap_or("agent task");
    format!("Cook {target}")
}

fn default_loop_commit_message(args: &AgentTaskLoopArgs) -> String {
    let target = args.dispatch.repo.as_deref().unwrap_or("agent task");
    format!("fix: cook {target}")
}

pub(super) fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let plan = agent_task_service::read_plan(&args.plan)?;
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
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

pub(super) fn run_submitted(args: StatusArgs) -> CmdResult<Value> {
    run_submitted_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

pub(super) fn run_submitted_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_submitted(run_id, executor)?;
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
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_next(executor)?;
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
            ExtensionProviderAgentTaskExecutor::discover(),
        );
    }
    Ok((
        serde_json::to_value(result.record).unwrap_or(Value::Null),
        0,
    ))
}
