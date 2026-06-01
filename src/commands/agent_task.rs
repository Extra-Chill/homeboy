use clap::{Args, Subcommand};
use serde_json::Value;

use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::{AgentTaskPlan, AgentTaskScheduler};
use homeboy::core::config;

use super::{CmdResult, GlobalArgs};

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    /// Run an agent-task plan through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// List extension-declared agent-task executor providers.
    Providers,
}

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
}

pub fn run(args: AgentTaskArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::RunPlan(run_args) => run_plan(run_args),
        AgentTaskCommand::Providers => providers(),
    }
}

fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let raw = config::read_json_spec_to_string(&args.plan)?;
    let plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::discover());
    let aggregate = scheduler.run(plan);
    let exit_code = if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    };
    Ok((
        serde_json::to_value(aggregate).unwrap_or(Value::Null),
        exit_code,
    ))
}

fn providers() -> CmdResult<Value> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    Ok((
        serde_json::to_value(executor.providers()).unwrap_or(Value::Null),
        0,
    ))
}
