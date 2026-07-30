//! `agent-task` command tree root.
//!
//! This module is a thin dispatcher. The CLI contract (arg/subcommand types)
//! lives in [`args`], and each command family is handled by a focused sibling
//! module: [`auth`], [`controller`], [`run`], [`status`], and [`review`].

use serde::Serialize;
use serde_json::Value;

use super::CmdResult;

pub mod args;
pub mod auth;
pub(crate) mod candidate;
pub mod contract;
pub mod controller;
pub mod doctor;
pub mod fanout;
pub mod loop_definition;
pub mod prompts;
pub mod retained_artifacts;
pub mod review;
pub mod run;
pub mod status;
pub mod tool;

pub use args::{
    ActiveArgs, AdoptArgs, AgentTaskArgs, AgentTaskAuthArgs, AgentTaskAuthCommand,
    AgentTaskCommand, AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs,
    AgentTaskControllerCommand, AgentTaskControllerDispatchArgs, AgentTaskControllerFromSpecArgs,
    AgentTaskControllerInitArgs, AgentTaskControllerMarkHumanReadyArgs,
    AgentTaskControllerMaterializeArgs, AgentTaskControllerRunArgs,
    AgentTaskControllerRunFromSpecArgs, AgentTaskControllerRunNextArgs,
    AgentTaskControllerStatusArgs, AgentTaskControllerValidateProofArgs, AgentTaskCookArgs,
    AgentTaskDoctorArgs, AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs,
    AgentTaskFanoutCommand, AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs,
    AgentTaskFanoutPlanArgs, AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs,
    AgentTaskFanoutSubmitBatchArgs, AgentTaskLoopArgs, AgentTaskLoopCommand,
    AgentTaskLoopDefineArgs, AgentTaskLoopResumeArgs, AgentTaskLoopStatusArgs, CancelArgs,
    CompileLoopArgs, ContractArgs, ContractFormat, CookContinueArgs, DiagnoseArgs, EvidenceArgs,
    FinalizePrArgs, GateFeedbackArgs, LatestArgs, ListArgs, LogsArgs, PromoteArgs,
    PromotionProviderArgs, ProvidersArgs, ReconcileRecordsArgs, ReplayProviderBoundaryArgs,
    RetainedArtifactsArgs, RetainedArtifactsCommand, RetryArgs, ReviewArgs, RunPlanArgs,
    RuntimeRecoverArgs, RuntimeValidateArgs, StatusArgs, SubmitArgs, VerifyGateArgs,
};
pub(crate) use status::diagnostic_summary_from_aggregate;

pub fn run(args: AgentTaskArgs) -> CmdResult<Value> {
    // Announce durable identity exactly once, on the first progress event that
    // carries a run id, and do it outside the TTY gate. Phase chatter stays
    // TTY-gated so non-interactive logs are not spammed, but the operator
    // handle itself must reach every caller — a non-TTY client that is
    // interrupted mid-cook otherwise has no way to answer "what did I just
    // start?" (#10419).
    let announced_identity = std::sync::atomic::AtomicBool::new(false);
    let progress = |phase: &str, cook_id: Option<&str>, run_id: Option<&str>| {
        if let Some(run_id) = run_id {
            if !announced_identity.swap(true, std::sync::atomic::Ordering::SeqCst) {
                run::announce_durable_cook_identity(cook_id, run_id);
            }
        }
        emit_cook_progress(phase, cook_id, run_id);
        Ok(())
    };
    run_with_cook_progress(args, Some(&progress))
}

fn cook_progress_message(phase: &str, cook_id: Option<&str>, run_id: Option<&str>) -> String {
    let identity = match (cook_id, run_id) {
        (_, Some(run_id)) => format!(" [{run_id}]"),
        (Some(cook_id), None) => format!(" [{cook_id}]"),
        (None, None) => String::new(),
    };
    match run_id {
        Some(run_id) => format!(
            "Cook {phase}: durable run `{run_id}`. Status: `homeboy agent-task status {run_id}`. Evidence: `homeboy agent-task evidence {run_id} --full`."
        ),
        None => format!("Cook {phase}{identity}."),
    }
}

/// Non-TTY callers need durable recovery coordinates in their captured stderr,
/// not transient terminal-only status. TTY output stays compact.
pub(crate) fn emit_cook_progress(phase: &str, cook_id: Option<&str>, run_id: Option<&str>) {
    if crate::commands::utils::tty::is_stdout_tty() {
        let identity = match (cook_id, run_id) {
            (_, Some(run_id)) => format!(" [{run_id}]"),
            (Some(cook_id), None) => format!(" [{cook_id}]"),
            (None, None) => String::new(),
        };
        crate::commands::utils::tty::status(&format!("cook: {phase}{identity}"));
    } else {
        eprintln!("{}", cook_progress_message(phase, cook_id, run_id));
    }
}

pub(crate) fn run_with_cook_progress(
    args: AgentTaskArgs,
    progress: Option<
        &(dyn Fn(&str, Option<&str>, Option<&str>) -> homeboy::core::Result<()> + Send + Sync),
    >,
) -> CmdResult<Value> {
    run_with_cook_progress_and_provenance(args, progress, None)
}

pub(crate) fn run_with_cook_progress_and_provenance(
    args: AgentTaskArgs,
    progress: Option<
        &(dyn Fn(&str, Option<&str>, Option<&str>) -> homeboy::core::Result<()> + Send + Sync),
    >,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Doctor(doctor_args) => doctor::doctor(doctor_args),
        AgentTaskCommand::Cook(cook_args) => {
            // Reject unsupported Cook source shapes before discovering a provider
            // or preparing the local/Lab execution route.
            run::validate_cook_request_with_provenance(&cook_args, provenance)?;
            if progress.is_some() {
                run::run_cook_with_executor_and_dispatcher_with_progress(
                    cook_args,
                    homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor::discover(),
                    None,
                    progress,
                    provenance,
                )
            } else {
                run::run_cook(cook_args)
            }
        }
        AgentTaskCommand::CookContinue(args) => run::continue_cook(args),
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
                status::reconcile_active(!active_args.apply)
            } else {
                status::list_active(active_args.into())
            }
        }
        AgentTaskCommand::Reconcile(args) => status::reconcile_run(&args.run_id, !args.apply),
        AgentTaskCommand::ReconcileRecords(args) => status::reconcile_records(args.dry_run),
        AgentTaskCommand::Latest(latest_args) => status::list_runs(
            agent_task_service::AgentTaskDiscoveryFilter::Latest,
            latest_args.into(),
        ),
        AgentTaskCommand::Logs(status_args) => status::logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => status::artifacts(status_args),
        AgentTaskCommand::RetainedArtifacts(args) => retained_artifacts::run(args),
        AgentTaskCommand::Evidence(evidence_args) => status::evidence(evidence_args),
        AgentTaskCommand::Diagnose(diagnose_args) => status::diagnose(diagnose_args),
        AgentTaskCommand::RuntimeRecover(args) => status::recover_runtime(args),
        AgentTaskCommand::RuntimeValidate(args) => status::validate_runtime(args),
        AgentTaskCommand::ReplayProviderBoundary(replay_args) => {
            status::replay_provider_boundary(replay_args)
        }
        AgentTaskCommand::Cancel(cancel_args) => status::cancel(cancel_args),
        AgentTaskCommand::Resume(status_args) => run::resume(status_args),
        AgentTaskCommand::Retry(retry_args) => run::retry(retry_args),
        AgentTaskCommand::Fanout(fanout_args) => fanout::fanout(fanout_args),
        AgentTaskCommand::Review(review_args) => review::review(review_args),
        AgentTaskCommand::Promote(promote_args) => review::promote_artifact(promote_args),
        AgentTaskCommand::Adopt(adopt_args) => review::adopt_candidate(adopt_args),
        AgentTaskCommand::PromotionProvider(provider_args) => {
            run::promotion_provider(provider_args)
        }
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

#[cfg(test)]
mod progress_tests {
    use super::cook_progress_message;

    #[test]
    fn non_tty_cook_progress_includes_durable_reconnect_commands() {
        // The formatter is the non-TTY client contract: callers can reconnect
        // after an interrupted observation without guessing the run identity.
        let message = cook_progress_message("provider_ready", None, Some("run-123"));
        assert!(message.contains("agent-task status run-123"));
        assert!(message.contains("agent-task evidence run-123 --full"));
    }
}

use homeboy::agents::agent_tasks::service as agent_task_service;

pub(crate) fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

#[cfg(test)]
mod tests;
