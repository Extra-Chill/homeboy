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
pub mod contract;
pub mod controller;
pub mod doctor;
pub mod fanout;
pub mod loop_definition;
pub mod prompts;
pub mod review;
pub mod run;
pub mod status;
pub mod tool;

pub use args::{
    AgentTaskArgs, AgentTaskAuthArgs, AgentTaskAuthCommand, AgentTaskCommand,
    AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs, AgentTaskControllerCommand,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerInitArgs,
    AgentTaskControllerMarkHumanReadyArgs, AgentTaskControllerMaterializeArgs,
    AgentTaskControllerRunArgs, AgentTaskControllerRunNextArgs, AgentTaskControllerStatusArgs,
    AgentTaskDoctorArgs, AgentTaskFanoutArgs, AgentTaskFanoutCommand, AgentTaskFanoutInputArgs,
    AgentTaskFanoutPlanArgs, AgentTaskFanoutPlaneArg, AgentTaskFanoutRunPlanArgs,
    AgentTaskFanoutSubmitArgs, AgentTaskLoopArgs, CancelArgs, CompileLoopArgs, ContractArgs,
    ContractFormat, FinalizePrArgs, GateFeedbackArgs, PromoteArgs, ProvidersArgs, RetryArgs,
    ReviewArgs, RunPlanArgs, StatusArgs, SubmitArgs, VerifyGateArgs,
};
pub(crate) use status::diagnostic_summary_from_aggregate;

pub fn run(args: AgentTaskArgs, global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Doctor(doctor_args) => doctor::doctor(doctor_args),
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
        AgentTaskCommand::List => status::list_runs(
            agent_task_service::AgentTaskDiscoveryFilter::All,
            discovery_options_from_raw_args(),
        ),
        AgentTaskCommand::Active => {
            // `Active` is a unit clap variant whose flag surface lives in the
            // fleet-owned args module; until a typed `--reconcile`/`--dry-run`/
            // `--limit` flag can be added there, detect them from the raw
            // process args so the safe stale-run reconcile path and the shared
            // `--limit` pagination cap are reachable today (#5682, #5681).
            let raw: Vec<String> = std::env::args().collect();
            let reconcile = raw.iter().any(|arg| arg == "--reconcile");
            let dry_run = raw.iter().any(|arg| arg == "--dry-run");
            if reconcile {
                status::reconcile_active(dry_run)
            } else {
                status::list_active(discovery_options_from_raw_args())
            }
        }
        AgentTaskCommand::Latest => status::list_runs(
            agent_task_service::AgentTaskDiscoveryFilter::Latest,
            discovery_options_from_raw_args(),
        ),
        AgentTaskCommand::Logs(status_args) => status::logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => status::artifacts(status_args),
        AgentTaskCommand::Cancel(cancel_args) => status::cancel(cancel_args),
        AgentTaskCommand::Resume(status_args) => run::resume(status_args),
        AgentTaskCommand::Retry(retry_args) => run::retry(retry_args),
        AgentTaskCommand::Fanout(fanout_args) => fanout::fanout(fanout_args),
        AgentTaskCommand::Review(review_args) => review::review(review_args),
        AgentTaskCommand::Promote(promote_args) => review::promote_artifact(promote_args),
        AgentTaskCommand::FinalizePr(finalize_args) => review::finalize_pull_request(finalize_args),
        AgentTaskCommand::GateFeedback(feedback_args) => review::gate_feedback(feedback_args),
        AgentTaskCommand::Providers(providers_args) => review::providers(providers_args),
        AgentTaskCommand::Prompts(prompts_args) => prompts::prompts(prompts_args),
        AgentTaskCommand::Contract(contract_args) => contract::contract(contract_args),
        AgentTaskCommand::CompileLoop(compile_args) => loop_definition::compile_loop(compile_args),
        AgentTaskCommand::Auth(auth_args) => auth::auth(auth_args),
        AgentTaskCommand::Controller(controller_args) => controller::controller(controller_args),
        AgentTaskCommand::Tool(tool_args) => match tool_args.command {
            tool::AgentTaskToolCommand::Dispatch(_) => {
                Err(homeboy::core::Error::validation_invalid_argument(
                    "agent-task tool dispatch",
                    "this internal bridge command is handled by the raw CLI runtime",
                    None,
                    None,
                ))
            }
        },
    }
}

use homeboy::core::agent_task_service as agent_task_service_direct;
use homeboy::core::agent_tasks::service as agent_task_service;

/// Parse shared discovery options (`--limit`) from the raw process args for the
/// `list`/`active`/`latest` unit clap variants. Their typed flag surface lives
/// in the fleet-owned args module; mirroring the existing #5682
/// `--reconcile`/`--dry-run` raw-arg detection, this lets the `--limit`
/// pagination cap ship without editing that module. Accepts both `--limit N`
/// and `--limit=N`; a missing/invalid value leaves the limit unset so the full
/// list is returned (#5681).
fn discovery_options_from_raw_args() -> agent_task_service_direct::AgentTaskDiscoveryOptions {
    let raw: Vec<String> = std::env::args().collect();
    let mut limit = None;
    let mut iter = raw.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--limit=") {
            if let Ok(parsed) = value.parse::<usize>() {
                limit = Some(parsed);
            }
        } else if arg == "--limit" {
            if let Some(parsed) = iter.next().and_then(|value| value.parse::<usize>().ok()) {
                limit = Some(parsed);
            }
        }
    }
    agent_task_service_direct::AgentTaskDiscoveryOptions { limit }
}

pub(crate) fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

#[cfg(test)]
mod tests;
