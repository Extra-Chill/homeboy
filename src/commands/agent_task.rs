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
    ActiveArgs, AgentTaskArgs, AgentTaskAuthArgs, AgentTaskAuthCommand, AgentTaskCommand,
    AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs, AgentTaskControllerCommand,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerInitArgs,
    AgentTaskControllerMarkHumanReadyArgs, AgentTaskControllerMaterializeArgs,
    AgentTaskControllerRunArgs, AgentTaskControllerRunFromSpecArgs, AgentTaskControllerRunNextArgs,
    AgentTaskControllerStatusArgs, AgentTaskControllerValidateProofArgs, AgentTaskCookArgs,
    AgentTaskDoctorArgs, AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs,
    AgentTaskFanoutCommand, AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs,
    AgentTaskFanoutPlanArgs, AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs,
    AgentTaskFanoutSubmitBatchArgs, AgentTaskLoopArgs, AgentTaskLoopCommand,
    AgentTaskLoopDefineArgs, AgentTaskLoopResumeArgs, AgentTaskLoopStatusArgs, CancelArgs,
    CompileLoopArgs, ContractArgs, ContractFormat, EvidenceArgs, FinalizePrArgs, GateFeedbackArgs,
    LatestArgs, ListArgs, PromoteArgs, ProvidersArgs, RetryArgs, ReviewArgs, RunPlanArgs,
    StatusArgs, SubmitArgs, VerifyGateArgs,
};
pub(crate) use status::diagnostic_summary_from_aggregate;

pub fn run(args: AgentTaskArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Doctor(doctor_args) => doctor::doctor(doctor_args),
        AgentTaskCommand::Cook(cook_args) => run::run_cook(cook_args),
        AgentTaskCommand::Loop(loop_args) => controller::loop_command(loop_args),
        AgentTaskCommand::RunPlan(run_args) => run::run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run::run_submitted(status_args),
        AgentTaskCommand::RunNext => run::run_next(),
        AgentTaskCommand::Submit(submit_args) => run::submit(submit_args),
        AgentTaskCommand::Status(status_args) => status::status(status_args),
        AgentTaskCommand::List(list_args) => status::list_runs(
            agent_task_service::AgentTaskDiscoveryFilter::All,
            list_args.into(),
        ),
        AgentTaskCommand::Active(active_args) => {
            if active_args.reconcile {
                status::reconcile_active(active_args.dry_run)
            } else {
                status::list_active(active_args.into())
            }
        }
        AgentTaskCommand::Latest(latest_args) => status::list_runs(
            agent_task_service::AgentTaskDiscoveryFilter::Latest,
            latest_args.into(),
        ),
        AgentTaskCommand::Logs(status_args) => status::logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => status::artifacts(status_args),
        AgentTaskCommand::Evidence(evidence_args) => status::evidence(evidence_args),
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

use homeboy::core::agent_tasks::service as agent_task_service;

pub(crate) fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

#[cfg(test)]
mod tests;
