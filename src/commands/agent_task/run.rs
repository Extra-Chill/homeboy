//! Durable run lifecycle handlers: run-plan, run, run-next, submit, resume, and retry.

use serde_json::Value;

use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy::core::agent_tasks::service as agent_task_service;

use super::super::CmdResult;
use super::args::{RetryArgs, RunPlanArgs, StatusArgs, SubmitArgs};

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
