//! `agent-task` command tree root.
//!
//! This module is a thin dispatcher. The CLI contract (arg/subcommand types)
//! lives in [`args`], and each command family is handled by a focused sibling
//! module: [`auth`], [`controller`], [`run`], [`status`], and [`review`].

use serde::Serialize;
use serde_json::Value;

use super::{CmdResult, GlobalArgs};

pub mod args;
pub mod auth;
pub mod controller;
pub mod review;
pub mod run;
pub mod status;

pub use args::{
    AgentTaskArgs, AgentTaskAuthArgs, AgentTaskAuthCommand, AgentTaskCommand,
    AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs, AgentTaskControllerCommand,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerInitArgs,
    AgentTaskControllerMarkHumanReadyArgs, AgentTaskControllerRunArgs,
    AgentTaskControllerRunNextArgs, AgentTaskControllerStatusArgs, AgentTaskLoopArgs, CancelArgs,
    FinalizePrArgs, GateFeedbackArgs, PromoteArgs, ProvidersArgs, RetryArgs, ReviewArgs,
    RunPlanArgs, StatusArgs, SubmitArgs, VerifyGateArgs,
};
pub(crate) use status::diagnostic_summary_from_aggregate;

pub fn run(args: AgentTaskArgs, global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Cook(dispatch_args) => {
            super::agent_task_dispatch::cook(dispatch_args, global)
        }
        AgentTaskCommand::Loop(loop_args) => run::run_loop(loop_args),
        AgentTaskCommand::Dispatch(dispatch_args) => {
            super::agent_task_dispatch::run(dispatch_args, global)
        }
        AgentTaskCommand::RunPlan(run_args) => run::run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run::run_submitted(status_args),
        AgentTaskCommand::RunNext => run::run_next(),
        AgentTaskCommand::Submit(submit_args) => run::submit(submit_args),
        AgentTaskCommand::Status(status_args) => status::status(status_args),
        AgentTaskCommand::List => {
            status::list_runs(agent_task_service::AgentTaskDiscoveryFilter::All)
        }
        AgentTaskCommand::Active => {
            status::list_runs(agent_task_service::AgentTaskDiscoveryFilter::Active)
        }
        AgentTaskCommand::Latest => {
            status::list_runs(agent_task_service::AgentTaskDiscoveryFilter::Latest)
        }
        AgentTaskCommand::Logs(status_args) => status::logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => status::artifacts(status_args),
        AgentTaskCommand::Cancel(cancel_args) => status::cancel(cancel_args),
        AgentTaskCommand::Resume(status_args) => run::resume(status_args),
        AgentTaskCommand::Retry(retry_args) => run::retry(retry_args),
        AgentTaskCommand::Review(review_args) => review::review(review_args),
        AgentTaskCommand::Promote(promote_args) => review::promote_artifact(promote_args),
        AgentTaskCommand::FinalizePr(finalize_args) => review::finalize_pull_request(finalize_args),
        AgentTaskCommand::GateFeedback(feedback_args) => review::gate_feedback(feedback_args),
        AgentTaskCommand::Providers(providers_args) => review::providers(providers_args),
        AgentTaskCommand::Auth(auth_args) => auth::auth(auth_args),
        AgentTaskCommand::Controller(controller_args) => controller::controller(controller_args),
    }
}

use homeboy::core::agent_tasks::service as agent_task_service;

pub(crate) fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

#[cfg(test)]
mod tests;
