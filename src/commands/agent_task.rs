use clap::{Args, Subcommand};
use homeboy::core::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopStatus,
};
use homeboy::core::agent_task_finalization::{
    finalize_pr, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails,
    AgentTaskPrSourceRelationship, AgentTaskPrVerification,
};
use homeboy::core::agent_task_gate::AgentTaskGateRevealPolicy;
use homeboy::core::agent_task_promotion::{
    promote, AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use serde::Serialize;
use serde_json::Value;
use std::io::Read;

use homeboy::core::agent_task::AgentTaskRequest;
use homeboy::core::agent_task_lifecycle;
use homeboy::core::agent_task_loop_controller::{
    self, AgentTaskLoopActionStatus, AgentTaskLoopControllerRecord, AgentTaskLoopExternalEvent,
    AgentTaskLoopHistoryEvent, AgentTaskLoopPolicyAction, AgentTaskLoopPolicyActionRecord,
    AgentTaskLoopRunRef, AgentTaskLoopWait, AgentTaskLoopWaitStatus,
};
use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::{
    AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler,
};
use homeboy::core::agent_task_secrets;
use homeboy::core::config;

use super::agent_task_dispatch::{dispatch_with_executor, run as dispatch, DispatchArgs};
use super::{CmdResult, GlobalArgs};
use crate::commands::utils::tty::prompt_password;

pub mod review;

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    /// Sync a workspace if --runner is supplied, dispatch a repo-cooking task, and return the durable run id.
    Cook(DispatchArgs),
    /// Run a durable repo cook loop: dispatch, promote, verify, retry red gates, and finalize.
    Loop(AgentTaskLoopArgs),
    /// Build and dispatch common repo-cooking agent tasks without hand-authored provider JSON.
    Dispatch(DispatchArgs),
    /// Run an agent-task plan through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// Execute a previously submitted durable agent-task run.
    Run(StatusArgs),
    /// Claim and execute the oldest queued durable agent-task run.
    RunNext,
    /// Persist an agent-task plan and return a durable run id without executing it.
    Submit(SubmitArgs),
    /// Read durable agent-task run status.
    Status(StatusArgs),
    /// Read durable agent-task run scheduler events.
    Logs(StatusArgs),
    /// List artifacts and evidence refs recorded for a completed run.
    Artifacts(StatusArgs),
    /// Mark a queued or stale-running durable agent-task run as cancelled.
    Cancel(CancelArgs),
    /// Resume a queued or stale-running durable run.
    Resume(StatusArgs),
    /// Submit a fresh durable run from an existing run's plan.
    Retry(RetryArgs),
    /// Build a durable aggregate review envelope from run state, logs, artifacts, and promotion hints.
    Review(ReviewArgs),
    /// Promote a completed generic patch artifact into a managed worktree.
    Promote(PromoteArgs),
    /// Finalize a green cook run into a review-ready pull request.
    FinalizePr(FinalizePrArgs),
    /// Convert deterministic gate results into a cook-loop retry or stop decision.
    GateFeedback(GateFeedbackArgs),
    /// List extension-declared agent-task executor providers and optional secret readiness.
    Providers(ProvidersArgs),
    /// Configure and inspect agent-task provider authentication secrets.
    Auth(AgentTaskAuthArgs),
    /// Create, inspect, and resume durable multi-agent loop controller state.
    Controller(AgentTaskControllerArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthArgs {
    #[command(subcommand)]
    pub command: AgentTaskAuthCommand,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerArgs {
    #[command(subcommand)]
    pub command: AgentTaskControllerCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskControllerCommand {
    /// Create a durable loop controller record.
    Init(AgentTaskControllerInitArgs),
    /// Read a durable loop controller record.
    Status(AgentTaskControllerStatusArgs),
    /// List durable loop controller records.
    List,
    /// Apply an external event and resume matching waits.
    ApplyEvent(AgentTaskControllerApplyEventArgs),
    /// Claim and execute the next pending controller action.
    RunNext(AgentTaskControllerRunNextArgs),
    /// Claim and execute one pending controller action.
    Run(AgentTaskControllerRunArgs),
    /// Execute pending controller actions until no executable action remains.
    Resume(AgentTaskControllerRunNextArgs),
    /// Mark a tracked entity as human-ready work.
    MarkHumanReady(AgentTaskControllerMarkHumanReadyArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerInitArgs {
    /// Durable loop id. Unsafe path characters are normalized for storage.
    pub loop_id: String,

    /// Initial controller phase.
    #[arg(long, default_value = "init", value_name = "PHASE")]
    pub phase: String,

    /// Declared graph/config version for resume compatibility.
    #[arg(long = "config-version", default_value = "v1", value_name = "VERSION")]
    pub config_version: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerStatusArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerApplyEventArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// External event type, for example github.pr.merged or task.completed.
    #[arg(long = "event-type", value_name = "TYPE")]
    pub event_type: String,

    /// Stable event id. Generated from the loop history length when omitted.
    #[arg(long = "event-id", value_name = "ID")]
    pub event_id: Option<String>,

    /// Optional deterministic event key, such as repo#pr or a check-suite id.
    #[arg(long = "event-key", value_name = "KEY")]
    pub event_key: Option<String>,

    /// Optional target entity id for wait matching and lineage.
    #[arg(long = "entity-id", value_name = "ID")]
    pub entity_id: Option<String>,

    /// Event payload JSON, @file, or - for stdin. May contain a `policy` object to evaluate.
    #[arg(long, value_name = "JSON")]
    pub payload: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerRunNextArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerRunArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// Pending controller action id to execute.
    #[arg(long = "action-id", value_name = "ID")]
    pub action_id: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerMarkHumanReadyArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// Entity id to mark human-ready.
    #[arg(long = "entity-id", value_name = "ID")]
    pub entity_id: String,

    /// Operator-visible reason stored in loop history.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskAuthCommand {
    /// Show redacted readiness for provider secret environment variables.
    Status(AgentTaskAuthStatusArgs),
    /// Store a provider secret in the OS keychain and map it to a required env name.
    SetKeychain(AgentTaskAuthSetKeychainArgs),
    /// Store a provider secret in Homeboy global config and map it to a required env name.
    SetConfig(AgentTaskAuthSetConfigArgs),
    /// Store a JSON secret bundle in one OS keychain item.
    SetKeychainBundle(AgentTaskAuthSetKeychainBundleArgs),
    /// Map a required provider env name to another process env var.
    MapEnv(AgentTaskAuthMapEnvArgs),
    /// Map a required provider env name to a field in a JSON keychain bundle.
    MapKeychainBundle(AgentTaskAuthMapKeychainBundleArgs),
    /// Remove a provider secret source mapping.
    Remove(AgentTaskAuthRemoveArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthStatusArgs {
    /// Secret environment variable name to check without exposing its value. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthSetKeychainArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Secret value. Omit to prompt securely.
    #[arg(value_name = "VALUE")]
    pub value: Option<String>,

    /// Read the secret value from stdin.
    #[arg(long)]
    pub value_stdin: bool,

    /// Keychain scope. Defaults to agent-task.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Keychain entry name. Defaults to ENV.
    #[arg(long = "name", value_name = "NAME")]
    pub keychain_name: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthSetConfigArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Secret value. Omit to prompt securely.
    #[arg(value_name = "VALUE")]
    pub value: Option<String>,

    /// Read the secret value from stdin.
    #[arg(long)]
    pub value_stdin: bool,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthSetKeychainBundleArgs {
    /// Logical bundle id to store.
    #[arg(value_name = "BUNDLE")]
    pub bundle: String,

    /// JSON bundle value. Omit to prompt securely.
    #[arg(value_name = "JSON")]
    pub value: Option<String>,

    /// Read the JSON bundle value from stdin.
    #[arg(long)]
    pub value_stdin: bool,

    /// Keychain scope. Defaults to agent-task.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Keychain entry name. Defaults to BUNDLE.
    #[arg(long = "name", value_name = "NAME")]
    pub keychain_name: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthMapEnvArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Source process environment variable. Defaults to ENV.
    #[arg(long = "from", value_name = "ENV")]
    pub source_env: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthMapKeychainBundleArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Logical bundle id to read.
    #[arg(long, value_name = "BUNDLE")]
    pub bundle: String,

    /// Field path inside the JSON bundle, using dots for nested objects.
    #[arg(long, value_name = "FIELD")]
    pub field: String,

    /// Keychain scope. Defaults to agent-task.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Keychain entry name. Defaults to BUNDLE.
    #[arg(long = "name", value_name = "NAME")]
    pub keychain_name: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthRemoveArgs {
    /// Required provider environment variable name whose mapping should be removed.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Also remove the mapped keychain entry when the mapping points at keychain.
    #[arg(long)]
    pub keychain: bool,
}

#[derive(Args, Debug)]
pub struct ProvidersArgs {
    /// Secret environment variable name to check without exposing its value. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskLoopArgs {
    #[command(flatten)]
    pub dispatch: DispatchArgs,

    /// Repo-cooking goal. Alias for the dispatch prompt when --prompt is omitted.
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,

    /// Target managed worktree handle where candidate patches are promoted.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,

    /// External workspace provider command. When omitted, HOMEBOY_AGENT_TASK_PROMOTION_COMMAND is used.
    #[arg(long, value_name = "COMMAND")]
    pub provider_command: Option<String>,

    /// Deterministic verification command to run after each promotion.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,

    /// Private deterministic verification command to run after each promotion.
    #[arg(long = "private-verify", value_name = "COMMAND")]
    pub private_verify: Vec<String>,

    /// Feedback policy for failed private gates.
    #[arg(
        long = "private-gate-reveal",
        default_value = "summary-only",
        value_name = "POLICY"
    )]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,

    /// Maximum cook-loop gate attempts, including the first candidate.
    #[arg(long = "max-attempts", default_value_t = 3, value_name = "N")]
    pub max_attempts: u32,

    /// Stop after the first green promotion without committing, pushing, or opening/updating a PR.
    #[arg(long = "no-finalize")]
    pub no_finalize: bool,

    /// PR base branch used by finalization.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,

    /// PR head branch. Defaults to the current branch in the promoted worktree.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,

    /// PR title. Defaults to a title derived from --repo or --task-url.
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,

    /// Commit message for the verified candidate changes.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: Option<String>,

    /// Protected branch that may not be finalized directly. Repeatable.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,

    /// AI tool disclosure line for the PR body.
    #[arg(long, default_value = "OpenCode (GPT-5.5)", value_name = "TEXT")]
    pub ai_tool: String,

    /// AI assistance scope for the PR body.
    #[arg(
        long,
        default_value = "Drafted implementation and tests; Chris reviews and owns the change.",
        value_name = "TEXT"
    )]
    pub ai_used_for: String,
}

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Also persist the completed run lifecycle record under this id.
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct SubmitArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct RetryArgs {
    /// Existing durable run id whose plan should be retried.
    pub run_id: String,

    /// Optional durable run id for the retry. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub new_run_id: Option<String>,

    /// Execute the newly queued retry immediately.
    #[arg(long)]
    pub run: bool,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,

    /// Operator-visible reason stored on the durable run record.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// Durable run id returned by `agent-task submit`, `dispatch`, or `run-plan --record-run-id`.
    pub run_id: String,

    /// Target workspace handle to include in generated promotion commands.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: Option<String>,

    /// External workspace provider command to include in generated promotion commands.
    #[arg(long, value_name = "COMMAND")]
    pub provider_command: Option<String>,
}

#[derive(Args, Debug)]
pub struct PromoteArgs {
    /// AgentTaskOutcome or AgentTaskAggregate JSON file, @file, or - for stdin.
    #[arg(value_name = "SOURCE")]
    pub source: String,

    /// Target workspace handle to apply into.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,

    /// External workspace provider command. When omitted, HOMEBOY_AGENT_TASK_PROMOTION_COMMAND is used.
    #[arg(long, value_name = "COMMAND")]
    pub provider_command: Option<String>,

    /// Outcome task id to select when SOURCE is an aggregate.
    #[arg(long, value_name = "TASK_ID")]
    pub task_id: Option<String>,

    /// Patch artifact id to select when the outcome contains multiple patches.
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub artifact_id: Option<String>,

    /// Validate and report the selected promotion without creating/applying.
    #[arg(long)]
    pub dry_run: bool,

    /// Verification command to run in the promoted worktree after apply.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,

    /// Private verification command to run after apply without exposing full failure details to follow-up agents.
    #[arg(long = "private-verify", value_name = "COMMAND")]
    pub private_verify: Vec<String>,

    /// Feedback policy for failed private gates.
    #[arg(
        long = "private-gate-reveal",
        default_value = "summary-only",
        value_name = "POLICY"
    )]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
}

#[derive(Args, Debug)]
pub struct FinalizePrArgs {
    /// Durable cook/agent-task run id to link in the PR body.
    #[arg(long, value_name = "ID")]
    pub run_id: String,

    /// Verified candidate worktree path.
    #[arg(long, value_name = "PATH")]
    pub path: String,

    /// PR base branch.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,

    /// PR head branch. Defaults to the current branch in --path.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,

    /// PR title.
    #[arg(long, value_name = "TEXT")]
    pub title: String,

    /// Commit message for the verified candidate changes.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: String,

    #[command(flatten)]
    pub evidence: review::FinalizePrEvidenceArgs,

    /// Green gate result as name=status or name=status:detail. Repeatable.
    #[arg(long = "gate-result", value_name = "NAME=STATUS[:DETAIL]")]
    pub gate_results: Vec<String>,

    /// Changed file summary to include in output/PR body. Defaults to git status discovery.
    #[arg(long = "changed-file", value_name = "PATH")]
    pub changed_files: Vec<String>,

    /// Protected branch that may not be finalized directly. Repeatable.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,

    /// AI assistance scope for the PR body.
    #[arg(
        long,
        default_value = "Drafted implementation and tests; Chris reviews and owns the change.",
        value_name = "TEXT"
    )]
    pub ai_used_for: String,
}

#[derive(Args, Debug)]
pub struct GateFeedbackArgs {
    /// AgentTaskPromotionReport JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub promotion: String,

    /// Original AgentTaskRequest JSON file, @file, or - for stdin.
    #[arg(long = "source-task", value_name = "PATH")]
    pub source_task: String,

    /// Current deterministic gate attempt number.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub attempt: u32,

    /// Maximum deterministic gate attempts allowed for this cook loop.
    #[arg(long = "max-attempts", default_value_t = 3, value_name = "N")]
    pub max_attempts: u32,

    /// Durable source run id to include in evidence refs.
    #[arg(long = "source-run-id", value_name = "ID")]
    pub source_run_id: Option<String>,

    /// Current candidate diff/context as a string, @file, or - for stdin.
    #[arg(long = "current-diff", value_name = "SPEC")]
    pub current_diff: Option<String>,
}

pub fn run(args: AgentTaskArgs, global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Cook(dispatch_args) => dispatch(dispatch_args, global),
        AgentTaskCommand::Loop(loop_args) => run_loop(loop_args),
        AgentTaskCommand::Dispatch(dispatch_args) => dispatch(dispatch_args, global),
        AgentTaskCommand::RunPlan(run_args) => run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run_submitted(status_args),
        AgentTaskCommand::RunNext => run_next(),
        AgentTaskCommand::Submit(submit_args) => submit(submit_args),
        AgentTaskCommand::Status(status_args) => status(status_args),
        AgentTaskCommand::Logs(status_args) => logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => artifacts(status_args),
        AgentTaskCommand::Cancel(cancel_args) => cancel(cancel_args),
        AgentTaskCommand::Resume(status_args) => resume(status_args),
        AgentTaskCommand::Retry(retry_args) => retry(retry_args),
        AgentTaskCommand::Review(review_args) => review::review(review_args),
        AgentTaskCommand::Promote(promote_args) => review::promote_artifact(promote_args),
        AgentTaskCommand::FinalizePr(finalize_args) => review::finalize_pull_request(finalize_args),
        AgentTaskCommand::GateFeedback(feedback_args) => review::gate_feedback(feedback_args),
        AgentTaskCommand::Providers(providers_args) => review::providers(providers_args),
        AgentTaskCommand::Auth(auth_args) => auth(auth_args),
        AgentTaskCommand::Controller(controller_args) => controller(controller_args),
    }
}

fn controller(args: AgentTaskControllerArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskControllerCommand::Init(init_args) => Ok((
            command_json_value(agent_task_loop_controller::create_controller(
                &init_args.loop_id,
                &init_args.phase,
                &init_args.config_version,
            )?)?,
            0,
        )),
        AgentTaskControllerCommand::Status(status_args) => Ok((
            command_json_value(agent_task_loop_controller::load_controller(
                &status_args.loop_id,
            )?)?,
            0,
        )),
        AgentTaskControllerCommand::List => Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-loop-controller-list/v1",
                "controllers": agent_task_loop_controller::list_controllers()?,
            }),
            0,
        )),
        AgentTaskControllerCommand::ApplyEvent(event_args) => apply_controller_event(event_args),
        AgentTaskControllerCommand::RunNext(run_args) => controller_run_next(run_args),
        AgentTaskControllerCommand::Run(run_args) => controller_run_action(run_args),
        AgentTaskControllerCommand::Resume(run_args) => controller_resume(run_args),
        AgentTaskControllerCommand::MarkHumanReady(ready_args) => {
            let mut record = agent_task_loop_controller::load_controller(&ready_args.loop_id)?;
            record.mark_human_ready(&ready_args.entity_id, ready_args.reason)?;
            agent_task_loop_controller::write_controller(&record)?;
            Ok((command_json_value(record)?, 0))
        }
    }
}

fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

fn apply_controller_event(args: AgentTaskControllerApplyEventArgs) -> CmdResult<Value> {
    let mut record = agent_task_loop_controller::load_controller(&args.loop_id)?;
    let payload = match args.payload {
        Some(spec) => {
            serde_json::from_str(&config::read_json_spec_to_string(&spec)?).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "payload",
                    error.to_string(),
                    Some(spec),
                    None,
                )
            })?
        }
        None => Value::Null,
    };
    let event_id = args
        .event_id
        .unwrap_or_else(|| format!("event-{}", record.history.len() + 1));
    let actions = record.apply_event(AgentTaskLoopExternalEvent {
        event_id,
        event_type: args.event_type,
        event_key: args.event_key,
        entity_id: args.entity_id,
        payload,
    });
    agent_task_loop_controller::write_controller(&record)?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-loop-controller-event-result/v1",
            "controller": record,
            "actions": actions,
        }),
        0,
    ))
}

fn controller_run_next(args: AgentTaskControllerRunNextArgs) -> CmdResult<Value> {
    controller_run_next_with_executor(args.loop_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn controller_run_action(args: AgentTaskControllerRunArgs) -> CmdResult<Value> {
    controller_run_action_with_executor(
        args.loop_id,
        args.action_id,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn controller_resume(args: AgentTaskControllerRunNextArgs) -> CmdResult<Value> {
    controller_resume_with_executor(args.loop_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn controller_run_next_with_executor<E>(loop_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let mut record = agent_task_loop_controller::load_controller(&loop_id)?;
    let Some(action_id) = first_pending_action_id(&record) else {
        return Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-loop-controller-action-result/v1",
                "loop_id": record.loop_id,
                "claimed": false,
                "controller": record,
            }),
            0,
        ));
    };
    execute_controller_action(&mut record, &action_id, executor)
}

fn controller_run_action_with_executor<E>(
    loop_id: String,
    action_id: String,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let mut record = agent_task_loop_controller::load_controller(&loop_id)?;
    execute_controller_action(&mut record, &action_id, executor)
}

fn controller_resume_with_executor<E>(loop_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let mut results = Vec::new();
    loop {
        let record = agent_task_loop_controller::load_controller(&loop_id)?;
        let Some(action_id) = first_pending_action_id(&record) else {
            return Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-loop-controller-resume-result/v1",
                    "loop_id": record.loop_id,
                    "claimed": false,
                    "results": results,
                    "controller": record,
                }),
                0,
            ));
        };
        let (value, exit_code) =
            controller_run_action_with_executor(loop_id.clone(), action_id, executor.clone())?;
        results.push(value);
        if exit_code != 0 {
            let record = agent_task_loop_controller::load_controller(&loop_id)?;
            return Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-loop-controller-resume-result/v1",
                    "loop_id": record.loop_id,
                    "claimed": true,
                    "results": results,
                    "controller": record,
                }),
                exit_code,
            ));
        }
    }
}

fn execute_controller_action<E>(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let action = claim_controller_action(record, action_id)?;
    agent_task_loop_controller::write_controller(record)?;

    match execute_claimed_controller_action(record, &action, executor) {
        Ok((execution, exit_code)) => {
            complete_controller_action(record, action_id, &execution, exit_code)?;
            agent_task_loop_controller::write_controller(record)?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-loop-controller-action-result/v1",
                    "loop_id": record.loop_id,
                    "claimed": true,
                    "action_id": action_id,
                    "status": if exit_code == 0 { "completed" } else { "failed" },
                    "execution": execution,
                    "controller": record,
                }),
                exit_code,
            ))
        }
        Err(error) => {
            fail_controller_action(record, action_id, &error.to_string())?;
            agent_task_loop_controller::write_controller(record)?;
            Err(error)
        }
    }
}

fn execute_claimed_controller_action<E>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key,
            entity_id,
            request,
        } => execute_spawn_task_action(
            record,
            action,
            dedupe_key,
            entity_id.as_deref(),
            request,
            executor,
        ),
        AgentTaskLoopPolicyAction::FanOut {
            dedupe_key,
            entity_ids,
            request_template,
        } => execute_fan_out_action(
            record,
            action,
            dedupe_key,
            entity_ids,
            request_template,
            executor,
        ),
        AgentTaskLoopPolicyAction::Join { wait_key } => Ok((
            serde_json::json!({ "mode": "join", "wait_key": wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Ok((
            serde_json::json!({ "mode": "wait_for_event", "wait_key": wait.wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => Ok((
            serde_json::json!({
                "mode": "run_gates",
                "bundle_id": bundle_id,
                "entity_id": entity_id,
                "queued": true,
                "note": "gate bundle execution is represented in controller state; concrete gate runners consume gate_bundles"
            }),
            0,
        )),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
            record.mark_human_ready(entity_id, reason.clone())?;
            Ok((
                serde_json::json!({ "mode": "mark_human_ready", "entity_id": entity_id }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Complete { reason } => Ok((
            serde_json::json!({ "mode": "complete", "reason": reason }),
            0,
        )),
        AgentTaskLoopPolicyAction::Abandon { reason } => Ok((
            serde_json::json!({ "mode": "abandon", "reason": reason }),
            0,
        )),
        AgentTaskLoopPolicyAction::Escalate { reason } => Ok((
            serde_json::json!({ "mode": "escalate", "reason": reason }),
            0,
        )),
        AgentTaskLoopPolicyAction::RouteFinding { .. }
        | AgentTaskLoopPolicyAction::ValidateCandidatePatch { .. }
        | AgentTaskLoopPolicyAction::Retry { .. }
        | AgentTaskLoopPolicyAction::RequestChanges { .. } => {
            Err(homeboy::core::Error::validation_invalid_argument(
                "action_id",
                format!(
                    "controller action '{}' is not executable by the generic controller runner yet",
                    action.action_id
                ),
                Some(action.action_id.clone()),
                None,
            ))
        }
    }
}

fn execute_spawn_task_action<E>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request: &Value,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("run_plan");
    match mode {
        "run_plan" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            let (aggregate, exit_code) =
                run_submitted_with_executor(submitted.run_id.clone(), executor)?;
            Ok((
                serde_json::json!({
                    "mode": mode,
                    "run_id": submitted.run_id,
                    "submitted": submitted,
                    "aggregate": aggregate,
                }),
                exit_code,
            ))
        }
        "submit" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            Ok((
                serde_json::json!({
                    "mode": mode,
                    "run_id": submitted.run_id,
                    "submitted": submitted,
                }),
                0,
            ))
        }
        "run" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let (aggregate, exit_code) = run_submitted_with_executor(run_id.clone(), executor)?;
            Ok((
                serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate }),
                exit_code,
            ))
        }
        "resume" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let (aggregate, exit_code) = run_resume_with_executor(run_id.clone(), executor)?;
            Ok((
                serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate }),
                exit_code,
            ))
        }
        "run_next" => {
            let (value, exit_code) = run_next_with_executor(executor)?;
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                serde_json::json!({ "mode": mode, "result": value }),
                exit_code,
            ))
        }
        "dispatch" => {
            let dispatch_args = dispatch_args_from_controller_request(request)?;
            let (value, exit_code) = dispatch_with_executor(dispatch_args, executor)?;
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                serde_json::json!({ "mode": mode, "result": value }),
                exit_code,
            ))
        }
        other => Err(homeboy::core::Error::validation_invalid_argument(
            "request.mode",
            format!("unsupported spawn_task request mode '{other}'"),
            Some(other.to_string()),
            Some(vec![
                "Supported modes: run_plan, submit, run, resume, run_next, dispatch".to_string(),
            ]),
        )),
    }
}

fn execute_fan_out_action<E>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_ids: &[String],
    request_template: &Value,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let mut results = Vec::new();
    let mut exit_code = 0;
    for entity_id in entity_ids {
        let request = materialize_fan_out_request(request_template, entity_id);
        let child_dedupe_key = format!("{dedupe_key}:{entity_id}");
        let child_action = AgentTaskLoopPolicyActionRecord {
            action_id: format!("{}:{entity_id}", action.action_id),
            action: AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: child_dedupe_key.clone(),
                entity_id: Some(entity_id.clone()),
                request: request.clone(),
            },
            status: AgentTaskLoopActionStatus::Running,
            reason: action.reason.clone(),
            created_at: action.created_at.clone(),
            dedupe_key: Some(child_dedupe_key.clone()),
        };
        let (result, child_exit_code) = execute_spawn_task_action(
            record,
            &child_action,
            &child_dedupe_key,
            Some(entity_id),
            &request,
            executor.clone(),
        )?;
        if child_exit_code != 0 {
            exit_code = child_exit_code;
        }
        results.push(result);
    }
    Ok((
        serde_json::json!({ "mode": "fan_out", "results": results }),
        exit_code,
    ))
}

fn claim_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
) -> homeboy::core::Result<AgentTaskLoopPolicyActionRecord> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    if action.status != AgentTaskLoopActionStatus::Pending {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "action_id",
            format!(
                "controller action '{}' is {:?}, not pending",
                action.action_id, action.status
            ),
            Some(action.action_id.clone()),
            None,
        ));
    }
    action.status = AgentTaskLoopActionStatus::Running;
    let action = action.clone();
    push_controller_history(
        record,
        "controller.action.claimed",
        None,
        serde_json::json!({ "action_id": action.action_id, "dedupe_key": action.dedupe_key }),
    );
    Ok(action)
}

fn complete_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    execution: &Value,
    exit_code: i32,
) -> homeboy::core::Result<()> {
    let status = if exit_code == 0 {
        AgentTaskLoopActionStatus::Completed
    } else {
        AgentTaskLoopActionStatus::Failed
    };
    set_controller_action_status(record, action_id, status)?;
    push_controller_history(
        record,
        if exit_code == 0 {
            "controller.action.completed"
        } else {
            "controller.action.failed"
        },
        None,
        serde_json::json!({ "action_id": action_id, "exit_code": exit_code, "execution": execution }),
    );
    Ok(())
}

fn fail_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    message: &str,
) -> homeboy::core::Result<()> {
    set_controller_action_status(record, action_id, AgentTaskLoopActionStatus::Failed)?;
    push_controller_history(
        record,
        "controller.action.failed",
        None,
        serde_json::json!({ "action_id": action_id, "error": message }),
    );
    Ok(())
}

fn set_controller_action_status(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    status: AgentTaskLoopActionStatus,
) -> homeboy::core::Result<()> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    action.status = status;
    Ok(())
}

fn record_controller_spawn(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    run_id: &str,
    request: &Value,
) -> homeboy::core::Result<()> {
    if let Some(dedupe) = record.dedupe_keys.get_mut(dedupe_key) {
        dedupe.run_id = Some(run_id.to_string());
    }
    if let Some(entity_id) = entity_id {
        if let Some(entity) = record.entities.get_mut(entity_id) {
            if !entity.run_refs.iter().any(|run| run.run_id == run_id) {
                entity.run_refs.push(AgentTaskLoopRunRef {
                    run_id: run_id.to_string(),
                    task_id: None,
                    role: Some("spawn_task".to_string()),
                });
            }
        }
    }
    if !record
        .task_lineage
        .iter()
        .any(|lineage| lineage.run_id == run_id)
    {
        record.task_lineage.push(
            homeboy::core::agent_task_loop_controller::AgentTaskLoopTaskLineage {
                run_id: run_id.to_string(),
                task_id: None,
                parent_run_id: None,
                parent_task_id: None,
                entity_id: entity_id.map(str::to_string),
                dedupe_key: Some(dedupe_key.to_string()),
                artifact_refs: Vec::new(),
                inputs: request.clone(),
                outputs: Value::Null,
            },
        );
    }
    push_controller_history(
        record,
        "controller.action.spawned_run",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "run_id": run_id,
        }),
    );
    agent_task_loop_controller::write_controller(record)?;
    Ok(())
}

fn first_pending_action_id(record: &AgentTaskLoopControllerRecord) -> Option<String> {
    record
        .next_actions
        .iter()
        .find(|action| action.status == AgentTaskLoopActionStatus::Pending)
        .map(|action| action.action_id.clone())
}

fn plan_from_controller_request(request: &Value) -> homeboy::core::Result<AgentTaskPlan> {
    let plan_value = request.get("plan").unwrap_or(request);
    serde_json::from_value(plan_value.clone()).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("controller spawn_task plan".to_string()),
            Some(plan_value.to_string()),
        )
    })
}

fn dispatch_args_from_controller_request(request: &Value) -> homeboy::core::Result<DispatchArgs> {
    let dispatch = request.get("dispatch").unwrap_or(request);
    Ok(DispatchArgs {
        prompt: optional_string(dispatch, "prompt"),
        tasks: optional_string_array(dispatch, "tasks")?,
        tasks_json: optional_string(dispatch, "tasks_json"),
        cwd: optional_string(dispatch, "cwd"),
        workspace: optional_string(dispatch, "workspace"),
        repo: optional_string(dispatch, "repo"),
        task_url: optional_string(dispatch, "task_url"),
        backend: optional_string(dispatch, "backend").unwrap_or_else(|| "codebox".to_string()),
        selector: optional_string(dispatch, "selector"),
        model: optional_string(dispatch, "model"),
        secret_env: optional_string_array(dispatch, "secret_env")?,
        provider_config: optional_string(dispatch, "provider_config"),
        client_context: optional_string(dispatch, "client_context"),
        concurrency: optional_usize(dispatch, "concurrency")?.unwrap_or(1),
        attempts: optional_u32(dispatch, "attempts")?.unwrap_or(1),
        run_id: optional_string(dispatch, "run_id"),
        queue_only: optional_bool(dispatch, "queue_only").unwrap_or(false),
    })
}

fn materialize_fan_out_request(template: &Value, entity_id: &str) -> Value {
    let mut request = template.clone();
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "entity_id".to_string(),
            Value::String(entity_id.to_string()),
        );
        object.entry("run_id".to_string()).or_insert_with(|| {
            Value::String(format!(
                "controller-{}",
                entity_id.replace([':', '/', '#'], "_")
            ))
        });
    }
    request
}

fn controller_request_run_id(request: &Value, dedupe_key: &str, action_id: &str) -> String {
    optional_string(request, "run_id").unwrap_or_else(|| {
        format!(
            "controller-{}-{}",
            action_id,
            dedupe_key.replace([':', '/', '#', ' '], "_")
        )
    })
}

fn required_string(value: &Value, key: &str) -> homeboy::core::Result<String> {
    optional_string(value, key).ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            key,
            format!("controller action request requires string field '{key}'"),
            None,
            None,
        )
    })
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn optional_u32(value: &Value, key: &str) -> homeboy::core::Result<Option<u32>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    homeboy::core::Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a u32"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

fn optional_usize(value: &Value, key: &str) -> homeboy::core::Result<Option<usize>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    homeboy::core::Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a usize"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

fn optional_string_array(value: &Value, key: &str) -> homeboy::core::Result<Vec<String>> {
    let Some(value) = value.get(key) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            key,
            format!("controller action request field '{key}' must be an array of strings"),
            Some(value.to_string()),
            None,
        ));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    key,
                    format!("controller action request field '{key}' must contain only strings"),
                    Some(value.to_string()),
                    None,
                )
            })
        })
        .collect()
}

fn push_controller_history(
    record: &mut AgentTaskLoopControllerRecord,
    event_type: &str,
    entity_id: Option<String>,
    payload: Value,
) {
    record.history.push(AgentTaskLoopHistoryEvent {
        event_id: format!("event-{}", record.history.len() + 1),
        event_type: event_type.to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entity_id,
        payload,
    });
}

fn auth(args: AgentTaskAuthArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskAuthCommand::Status(status_args) => Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-auth-status/v1",
                "secret_env": agent_task_secrets::secret_env_status(&status_args.secret_env),
            }),
            0,
        )),
        AgentTaskAuthCommand::SetKeychain(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let status = agent_task_secrets::set_keychain_secret(
                &set_args.secret_env,
                &value,
                set_args.scope.as_deref(),
                set_args.keychain_name.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::SetConfig(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let status = agent_task_secrets::set_config_secret(&set_args.secret_env, &value)?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::SetKeychainBundle(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let keychain_name = agent_task_secrets::set_keychain_bundle(
                &set_args.bundle,
                &value,
                set_args.scope.as_deref(),
                set_args.keychain_name.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-bundle-configured/v1",
                    "bundle": set_args.bundle,
                    "source": "keychain-bundle",
                    "keychain_name": keychain_name,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::MapEnv(map_args) => {
            let status = agent_task_secrets::map_secret_to_env(
                &map_args.secret_env,
                map_args.source_env.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::MapKeychainBundle(map_args) => {
            let status = agent_task_secrets::map_secret_to_keychain_bundle(
                &map_args.secret_env,
                &map_args.bundle,
                &map_args.field,
                map_args.scope.as_deref(),
                map_args.keychain_name.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::Remove(remove_args) => {
            let status = agent_task_secrets::remove_secret_mapping(
                &remove_args.secret_env,
                remove_args.keychain,
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
    }
}

fn read_agent_task_secret_value(
    value: Option<String>,
    value_stdin: bool,
) -> homeboy::core::Result<String> {
    match (value, value_stdin) {
        (Some(_), true) => Err(homeboy::core::Error::validation_invalid_argument(
            "value-stdin",
            "cannot combine VALUE with --value-stdin",
            None,
            None,
        )),
        (Some(value), false) => Ok(value),
        (None, true) => {
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some("read agent-task secret value from stdin".to_string()),
                )
            })?;
            Ok(raw.trim_end_matches(['\r', '\n']).to_string())
        }
        (None, false) => prompt_password("Secret value: "),
    }
}

#[derive(Debug, Clone, Serialize)]
struct AgentTaskLoopReport {
    schema: &'static str,
    loop_id: String,
    status: String,
    attempts: Vec<AgentTaskLoopAttemptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finalization: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AgentTaskLoopAttemptReport {
    attempt: u32,
    run_id: String,
    run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    aggregate_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    promotion: Option<AgentTaskPromotionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback: Option<homeboy::core::agent_task_cook_loop::AgentTaskCookLoopReport>,
}

fn run_loop(args: AgentTaskLoopArgs) -> CmdResult<Value> {
    run_loop_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_loop_with_executor<E>(args: AgentTaskLoopArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    if args.verify.is_empty() && args.private_verify.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task loop requires at least one deterministic --verify or --private-verify gate",
            None,
            None,
        ));
    }

    let max_attempts = args.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut dispatch_args = args.dispatch.clone();
    if dispatch_args.prompt.is_none() {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args.queue_only = false;
    let (dispatch_value, _dispatch_exit) = dispatch_with_executor(dispatch_args, executor.clone())?;
    let mut run_id = dispatch_value["run_id"]
        .as_str()
        .ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task dispatch did not return a run_id".to_string(),
            )
        })?
        .to_string();
    let loop_id = run_id.clone();

    for attempt in 1..=max_attempts {
        let record = agent_task_lifecycle::status(&run_id)?;
        let plan = agent_task_lifecycle::load_plan(&run_id)?;
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(loop_report(
                loop_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task loop requires a plan with one source task".to_string()),
                1,
            ));
        };
        if plan.tasks.len() != 1 {
            return Ok(loop_report(
                loop_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task loop currently supports one task per cook attempt".to_string()),
                1,
            ));
        }

        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        ) {
            attempts.push(AgentTaskLoopAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(loop_report(
                loop_id,
                "provider_failure",
                attempts,
                None,
                Some(format!(
                    "agent-task run {run_id} ended in state {:?}",
                    record.state
                )),
                1,
            ));
        }

        let promotion = match promote_attempt(&args, &run_id) {
            Ok(report) => report,
            Err(error) => {
                attempts.push(AgentTaskLoopAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                return Ok(loop_report(
                    loop_id,
                    "policy_failure",
                    attempts,
                    None,
                    Some(error.to_string()),
                    1,
                ));
            }
        };

        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt,
            max_attempts,
            source_run_id: Some(run_id.clone()),
            current_diff: String::new(),
            metadata: Value::Null,
        });
        let feedback_status = feedback.status;
        let follow_up_request = feedback.follow_up_request.clone();
        attempts.push(AgentTaskLoopAttemptReport {
            attempt,
            run_id: run_id.clone(),
            run_state: format!("{:?}", record.state),
            aggregate_path: record.aggregate_path,
            promotion: Some(promotion.clone()),
            feedback: Some(feedback.clone()),
        });

        match feedback_status {
            AgentTaskCookLoopStatus::GreenCompleted => {
                if args.no_finalize {
                    return Ok(loop_report(
                        loop_id,
                        "green_no_finalize",
                        attempts,
                        None,
                        Some(
                            "deterministic gates completed green; --no-finalize skipped commit, push, and PR finalization"
                                .to_string(),
                        ),
                        0,
                    ));
                }
                let finalization = finalize_loop_pr(&args, &loop_id, &promotion)?;
                let final_status = finalization["status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let exit_code = if matches!(final_status.as_str(), "review_ready" | "no_changes") {
                    0
                } else {
                    1
                };
                return Ok(loop_report(
                    loop_id,
                    &final_status,
                    attempts,
                    Some(finalization),
                    None,
                    exit_code,
                ));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(follow_up_request) = follow_up_request else {
                    return Ok(loop_report(
                        loop_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cook-loop feedback requested retry without a follow-up request"
                                .to_string(),
                        ),
                        1,
                    ));
                };
                let next_run_id = format!("{loop_id}-attempt-{}", attempt + 1);
                let follow_up_plan = AgentTaskPlan::new(
                    format!("{loop_id}-cook-loop-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                run_loaded_plan(follow_up_plan, Some(&next_run_id), executor.clone())?;
                run_id = next_run_id;
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                return Ok(loop_report(
                    loop_id,
                    "retries_exhausted",
                    attempts,
                    None,
                    Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    1,
                ));
            }
        }
    }

    Ok(loop_report(
        loop_id,
        "retries_exhausted",
        attempts,
        None,
        Some("cook-loop attempt budget exhausted".to_string()),
        1,
    ))
}

fn promote_attempt(
    args: &AgentTaskLoopArgs,
    run_id: &str,
) -> homeboy::core::Result<AgentTaskPromotionReport> {
    let (source, source_path) = review::read_promotion_source(run_id)?;
    promote(AgentTaskPromotionOptions {
        source,
        source_path,
        to_worktree: args.to_worktree.clone(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        verify: args.verify.clone(),
        private_verify: args.private_verify.clone(),
        private_gate_reveal: args.private_gate_reveal,
        provider_command: args.provider_command.clone(),
    })
}

fn finalize_loop_pr(
    args: &AgentTaskLoopArgs,
    loop_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> homeboy::core::Result<Value> {
    if promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "promotion",
            "agent-task loop finalization requires an applied promotion with green gates",
            None,
            None,
        ));
    }
    let path = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "promotion provider did not report the applied worktree path",
                None,
                None,
            )
        })?
        .to_string();
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| default_loop_title(args));
    let commit_message = args
        .commit_message
        .clone()
        .unwrap_or_else(|| default_loop_commit_message(args));
    let source_refs = args
        .dispatch
        .task_url
        .iter()
        .cloned()
        .chain(std::iter::once(format!(
            "homeboy://agent-task/run/{loop_id}"
        )))
        .collect();
    let artifact_refs = std::iter::once(promotion.patch_artifact.path.clone()).collect();
    let report = finalize_pr(AgentTaskPrFinalizationOptions {
        path,
        run_id: loop_id.to_string(),
        base: args.base.clone(),
        head: args.head.clone(),
        title,
        commit_message,
        gate_results: Vec::new(),
        normalized_gate_results: promotion.gate_results.clone(),
        changed_files: promotion.changed_files.clone(),
        evidence: AgentTaskPrEvidence {
            source_refs,
            artifact_refs,
            attempt_summary: format!(
                "{} deterministic cook-loop gate attempt(s) completed green",
                promotion.deterministic_gates.len()
            ),
            ai_tool: args.ai_tool.clone(),
            ai_model: args
                .dispatch
                .model
                .clone()
                .or_else(|| ai_model_from_tool(&args.ai_tool)),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification {
                targeted_checks_run: args.verify.clone(),
                targeted_checks_unavailable: None,
                ci_expected: vec!["Homeboy CI after push".to_string()],
                manual_reviewer_check: None,
            },
            runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
        },
        ai_used_for: args.ai_used_for.clone(),
        protected_branches: args.protected_branches.clone(),
    })?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
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

fn loop_report(
    loop_id: String,
    status: &str,
    attempts: Vec<AgentTaskLoopAttemptReport>,
    finalization: Option<Value>,
    stop_reason: Option<String>,
    exit_code: i32,
) -> (Value, i32) {
    let report = AgentTaskLoopReport {
        schema: "homeboy/agent-task-loop/v1",
        loop_id,
        status: status.to_string(),
        attempts,
        finalization,
        stop_reason,
    };
    (
        serde_json::to_value(report).unwrap_or(Value::Null),
        exit_code,
    )
}

fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let plan = read_plan(&args.plan)?;
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn run_loaded_plan<E>(
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    normalize_plan_workspaces(&mut plan)?;

    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
    }

    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    }
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

fn run_submitted(args: StatusArgs) -> CmdResult<Value> {
    run_submitted_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_submitted_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_running(&run_id)?;
    run_claimed(run_id, executor)
}

fn run_next() -> CmdResult<Value> {
    run_next_with_executor(ExtensionProviderAgentTaskExecutor::discover())
}

fn run_next_with_executor<E>(executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok((serde_json::json!({ "claimed": false }), 0));
    };

    run_claimed(record.run_id, executor)
}

fn run_claimed<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let plan = agent_task_lifecycle::load_plan(&run_id)?;
    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
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

fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let plan = read_plan(&args.plan)?;
    let record = agent_task_lifecycle::submit_plan(&plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn status(args: StatusArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::status(&args.run_id)?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn logs(args: StatusArgs) -> CmdResult<Value> {
    let log = agent_task_lifecycle::logs(&args.run_id)?;
    Ok((serde_json::to_value(log).unwrap_or(Value::Null), 0))
}

fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_lifecycle::artifacts(&args.run_id)?;
    Ok((serde_json::to_value(artifacts).unwrap_or(Value::Null), 0))
}

fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::cancel_run(&args.run_id, args.reason.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn resume(args: StatusArgs) -> CmdResult<Value> {
    run_resume_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_resume_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_resuming(&run_id)?;
    run_claimed(run_id, executor)
}

fn retry(args: RetryArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::retry(&args.run_id, args.new_run_id.as_deref())?;
    if args.run {
        return run_submitted_with_executor(
            record.run_id,
            ExtensionProviderAgentTaskExecutor::discover(),
        );
    }
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn read_plan(spec: &str) -> homeboy::core::Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> homeboy::core::Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn normalize_component_worktree_workspace(
    request: &mut AgentTaskRequest,
) -> homeboy::core::Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        ));
    };

    let resolved_root = request
        .workspace
        .root
        .clone()
        .or_else(|| materialization_string(&request.workspace.materialization, "root"))
        .or_else(|| materialization_string(&request.workspace.materialization, "resolved_root"));

    let Some(root) = resolved_root else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workspace.root",
            format!(
                "agent-task task '{}' requested component-worktree workspace for component '{}' but no resolved root was provided; creating component worktrees depends on the generic Homeboy worktree primitive tracked by Extra-Chill/homeboy#3362",
                request.task_id, component_id
            ),
            None,
            None,
        ));
    };

    request.workspace.kind = None;
    request.workspace.mode = homeboy::core::agent_task::AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(root);
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = None;
    request.workspace.materialization = Value::Null;

    Ok(())
}

fn materialization_string(materialization: &Value, key: &str) -> Option<String> {
    materialization
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_task::{
        AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskLimits,
        AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
        AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy::core::agent_task_lifecycle::{
        status as lifecycle_status, AgentTaskRunRecord, AgentTaskRunState,
    };
    use homeboy::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[test]
    fn submit_run_status_reports_terminal_state() {
        with_temp_home(|| {
            let plan = AgentTaskPlan::new(
                "plan-cli-terminal",
                vec![AgentTaskRequest {
                    schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "task-cli-terminal".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "missing-provider-test".to_string(),
                        selector: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "exercise durable terminal status".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            let plan_file = tempfile::NamedTempFile::new().expect("plan file");
            std::fs::write(
                plan_file.path(),
                serde_json::to_string(&plan).expect("plan json"),
            )
            .expect("write plan");
            let plan_path = format!("@{}", plan_file.path().display());

            submit(SubmitArgs {
                plan: plan_path,
                run_id: Some("run-cli-terminal".to_string()),
            })
            .expect("submitted");
            let (_, run_exit_code) = run_submitted(StatusArgs {
                run_id: "run-cli-terminal".to_string(),
            })
            .expect("run completed");
            let (status_json, status_exit_code) = status(StatusArgs {
                run_id: "run-cli-terminal".to_string(),
            })
            .expect("status loaded");
            let record: AgentTaskRunRecord = serde_json::from_value(status_json).expect("record");

            assert_eq!(run_exit_code, 1);
            assert_eq!(status_exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Failed);
            assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
            assert_eq!(record.totals.expect("totals").failed, 1);
        });
    }

    #[test]
    fn run_plan_record_run_id_persists_running_status_before_executor_runs() {
        with_temp_home(|| {
            let run_id = "run-plan-durable";
            let observed_status = Arc::new(Mutex::new(None));
            let executor = InspectingExecutor {
                run_id: run_id.to_string(),
                observed_status: Arc::clone(&observed_status),
            };

            let (_value, exit_code) =
                run_loaded_plan(test_plan(), Some(run_id), executor).expect("run-plan completed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed durable status");
            assert_eq!(exit_code, 0);
            assert_eq!(observed.state, AgentTaskRunState::Running);
            assert_eq!(observed.tasks[0].state, AgentTaskState::Running);
            assert_eq!(observed.metadata["runner_pid"], std::process::id());
            assert!(observed.aggregate_path.is_none());

            let completed = lifecycle_status(run_id).expect("completed status loaded");
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
            assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
            assert!(completed.aggregate_path.is_some());
        });
    }

    #[test]
    fn run_next_claims_oldest_queued_run_and_leaves_later_runs_queued() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-a"))
                .expect("first submitted");
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b"))
                .expect("second submitted");
            let observed_status = Arc::new(Mutex::new(None));

            let (_value, exit_code) = run_next_with_executor(InspectingExecutor {
                run_id: "run-next-a".to_string(),
                observed_status: Arc::clone(&observed_status),
            })
            .expect("claimed run completed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed claimed status");
            let first = lifecycle_status("run-next-a").expect("first status");
            let second = lifecycle_status("run-next-b").expect("second status");

            assert_eq!(exit_code, 0);
            assert_eq!(observed.state, AgentTaskRunState::Running);
            assert_eq!(first.state, AgentTaskRunState::Succeeded);
            assert_eq!(second.state, AgentTaskRunState::Queued);
        });
    }

    #[test]
    fn run_next_returns_unclaimed_when_no_queued_runs_exist() {
        with_temp_home(|| {
            let (value, exit_code) = run_next_with_executor(InspectingExecutor {
                run_id: "unused".to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            })
            .expect("run-next checked queue");

            assert_eq!(exit_code, 0);
            assert_eq!(value["claimed"], false);
        });
    }

    #[test]
    fn cancel_command_marks_queued_run_cancelled() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-cli-cancel"))
                .expect("submitted");

            let (value, exit_code) = cancel(CancelArgs {
                run_id: "run-cli-cancel".to_string(),
                reason: Some("not selected".to_string()),
            })
            .expect("cancelled");
            let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

            assert_eq!(exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Cancelled);
            assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
            assert_eq!(record.metadata["cancel_reason"], json!("not selected"));
        });
    }

    #[test]
    fn retry_command_submits_new_queued_run() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-retry-source"))
                .expect("submitted");

            let (value, exit_code) = retry(RetryArgs {
                run_id: "run-retry-source".to_string(),
                new_run_id: Some("run-retry-cli".to_string()),
                run: false,
            })
            .expect("retry queued");
            let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

            assert_eq!(exit_code, 0);
            assert_eq!(record.run_id, "run-retry-cli");
            assert_eq!(record.state, AgentTaskRunState::Queued);
            assert_eq!(record.metadata["retry_of"], json!("run-retry-source"));
        });
    }

    #[test]
    fn resume_command_executes_existing_run() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-resume-cli"))
                .expect("submitted");
            let observed_status = Arc::new(Mutex::new(None));

            let (_value, exit_code) = run_resume_with_executor(
                "run-resume-cli".to_string(),
                InspectingExecutor {
                    run_id: "run-resume-cli".to_string(),
                    observed_status: Arc::clone(&observed_status),
                },
            )
            .expect("resumed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed status");
            let completed = lifecycle_status("run-resume-cli").expect("completed status");

            assert_eq!(exit_code, 0);
            assert!(observed.metadata["resume_requested_at"].is_string());
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        });
    }

    #[test]
    fn run_plan_maps_resolved_component_worktree_before_provider_dispatch() {
        let observed_request = Arc::new(Mutex::new(None));
        let executor = CapturingExecutor {
            observed_request: Arc::clone(&observed_request),
        };
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
        plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());
        plan.tasks[0].workspace.base_ref = Some("origin/main".to_string());
        plan.tasks[0].workspace.task_url =
            Some("https://github.com/Extra-Chill/wp-coding-agents/issues/179".to_string());
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        plan.tasks[0].workspace.materialization = json!({
            "root": "/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance"
        });

        let (_value, exit_code) =
            run_loaded_plan(plan, None, executor).expect("run-plan completed");
        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 0);
        assert_eq!(
            observed.workspace.mode,
            homeboy::core::agent_task::AgentTaskWorkspaceMode::Existing
        );
        assert_eq!(
            observed.workspace.root.as_deref(),
            Some("/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance")
        );
        assert_eq!(observed.workspace.slug.as_deref(), Some("wp-coding-agents"));
        assert!(observed.workspace.kind.is_none());
        assert!(observed.workspace.component_id.is_none());
        assert!(observed.workspace.branch.is_none());
        assert!(observed.workspace.base_ref.is_none());
        assert!(observed.workspace.task_url.is_none());
        assert!(observed.workspace.cleanup.is_none());
        assert!(observed.workspace.materialization.is_null());
    }

    #[test]
    fn run_plan_rejects_unresolved_component_worktree_until_core_primitive_exists() {
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
        plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());

        let error = run_loaded_plan(plan, None, CapturingExecutor::default())
            .expect_err("unresolved component worktree rejected");
        let message = error.to_string();

        assert!(message.contains("component-worktree workspace"));
        assert!(message.contains("Extra-Chill/homeboy#3362"));
    }

    #[test]
    fn controller_run_next_executes_spawn_task_plan_and_records_dedupe_lineage() {
        with_temp_home(|| {
            let observed_request = Arc::new(Mutex::new(None));
            let mut controller = agent_task_loop_controller::create_controller(
                "loop-controller-run-next",
                "repair",
                "v1",
            )
            .expect("controller created");
            let mut plan = test_plan();
            plan.tasks[0].executor.selector = Some("homeboy-lab".to_string());
            plan.tasks[0].executor.config = json!({
                "artifact_root": "/tmp/homeboy-lab-artifacts/controller-run-next"
            });

            controller.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "finding:abc:repair".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "run_plan",
                        "run_id": "controller-run-next-a",
                        "plan": plan,
                    }),
                },
                "finding emitted",
            );
            agent_task_loop_controller::write_controller(&controller).expect("controller written");

            let (value, exit_code) = controller_run_next_with_executor(
                "loop-controller-run-next".to_string(),
                CapturingExecutor {
                    observed_request: Arc::clone(&observed_request),
                },
            )
            .expect("controller action executed");

            let observed = observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("provider saw request");
            let loaded = agent_task_loop_controller::load_controller("loop-controller-run-next")
                .expect("controller loaded");

            assert_eq!(exit_code, 0);
            assert_eq!(value["claimed"], true);
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Completed
            );
            assert_eq!(
                loaded.dedupe_keys["finding:abc:repair"].run_id.as_deref(),
                Some("controller-run-next-a")
            );
            assert_eq!(loaded.task_lineage[0].run_id, "controller-run-next-a");
            assert!(loaded
                .history
                .iter()
                .any(|event| event.event_type == "controller.action.claimed"));
            assert!(loaded
                .history
                .iter()
                .any(|event| event.event_type == "controller.action.completed"));
            assert_eq!(observed.executor.selector.as_deref(), Some("homeboy-lab"));
            assert_eq!(
                observed.executor.config["artifact_root"],
                json!("/tmp/homeboy-lab-artifacts/controller-run-next")
            );
        });
    }

    #[test]
    fn controller_run_executes_requested_action_id_only() {
        with_temp_home(|| {
            let mut controller = agent_task_loop_controller::create_controller(
                "loop-controller-run-action",
                "repair",
                "v1",
            )
            .expect("controller created");
            controller.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(AgentTaskLoopWait {
                    wait_key: "wait-a".to_string(),
                    event_type: "task.completed".to_string(),
                    entity_id: None,
                    external_ref: None,
                    timeout_at: None,
                    escalation_policy: None,
                    status: AgentTaskLoopWaitStatus::Open,
                    satisfied_by_event_id: None,
                }),
                "wait first",
            );
            controller.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("done".to_string()),
                },
                "complete second",
            );
            agent_task_loop_controller::write_controller(&controller).expect("controller written");

            let (_value, exit_code) = controller_run_action_with_executor(
                "loop-controller-run-action".to_string(),
                "action-2".to_string(),
                CapturingExecutor::default(),
            )
            .expect("specific action executed");
            let loaded = agent_task_loop_controller::load_controller("loop-controller-run-action")
                .expect("controller loaded");

            assert_eq!(exit_code, 0);
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Pending
            );
            assert_eq!(
                loaded.next_actions[1].status,
                AgentTaskLoopActionStatus::Completed
            );
        });
    }

    #[test]
    fn promotion_source_resolves_completed_run_id() {
        with_temp_home(|| {
            let run_id = "run-promotion-source";

            run_loaded_plan(test_plan(), Some(run_id), InspectingExecutor::noop(run_id))
                .expect("run completed");

            let (raw, path) =
                review::read_promotion_source(run_id).expect("promotion source resolved");

            assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
            assert_eq!(
                path.as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str()),
                Some("aggregate.json")
            );
        });
    }

    #[test]
    fn promotion_source_reads_bare_json_file_path() {
        let file = tempfile::NamedTempFile::new().expect("source file");
        std::fs::write(
            file.path(),
            r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
        )
        .expect("write source");

        let (raw, path) = review::read_promotion_source(&file.path().display().to_string())
            .expect("promotion source file resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(path.as_deref(), Some(file.path()));
    }

    #[test]
    fn review_reports_queued_run_without_chat_state() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-review-queued"))
                .expect("submitted");

            let (value, exit_code) = review::review(ReviewArgs {
                run_id: "run-review-queued".to_string(),
                to_worktree: None,
                provider_command: None,
            })
            .expect("review loaded");

            assert_eq!(exit_code, 0);
            assert_eq!(value["schema"], "homeboy/agent-task-review/v1");
            assert_eq!(value["run_id"], "run-review-queued");
            assert_eq!(value["state"], "queued");
            assert_eq!(value["transport"]["chat_state_required"], false);
            assert!(value["aggregate_review"].is_null());
            assert_eq!(value["logs"]["events"][0]["state"], "queued");
            assert!(value["next_actions"][0]
                .as_str()
                .expect("next action")
                .contains("run-next"));
        });
    }

    #[test]
    fn review_reports_completed_aggregate_and_promotion_hints() {
        with_temp_home(|| {
            run_loaded_plan(
                test_plan(),
                Some("run-review-completed"),
                ApplyArtifactExecutor,
            )
            .expect("run completed");

            let (value, exit_code) = review::review(ReviewArgs {
                run_id: "run-review-completed".to_string(),
                to_worktree: Some("homeboy@fix-review-flow".to_string()),
                provider_command: None,
            })
            .expect("review loaded");

            assert_eq!(exit_code, 0);
            assert_eq!(value["state"], "succeeded");
            assert_eq!(value["aggregate_review"]["summary"]["apply_candidates"], 1);
            assert_eq!(value["artifacts"]["artifacts"][0]["id"], "patch-a");
            assert_eq!(value["promotion_candidates"][0]["task_id"], "task-a");
            assert_eq!(value["promotion_candidates"][0]["artifact_id"], "patch-a");
            assert_eq!(value["promotion_candidates"][0]["ready"], true);
            assert_eq!(
                value["promotion_candidates"][0]["command"],
                json!([
                    "homeboy",
                    "agent-task",
                    "promote",
                    value["aggregate_path"].as_str().expect("aggregate path"),
                    "--task-id",
                    "task-a",
                    "--artifact-id",
                    "patch-a",
                    "--to-worktree",
                    "homeboy@fix-review-flow"
                ])
            );
            assert!(value["next_actions"][0]
                .as_str()
                .expect("next action")
                .contains("promotion_candidates"));
        });
    }

    #[test]
    fn loop_returns_durable_id_when_promotion_provider_is_missing() {
        with_temp_home(|| {
            let (value, exit_code) = run_loop_with_executor(
                AgentTaskLoopArgs {
                    dispatch: DispatchArgs {
                        prompt: None,
                        tasks: Vec::new(),
                        tasks_json: None,
                        cwd: None,
                        workspace: None,
                        repo: Some("homeboy".to_string()),
                        task_url: Some(
                            "https://github.com/Extra-Chill/homeboy/issues/3675".to_string(),
                        ),
                        backend: "fixture".to_string(),
                        selector: None,
                        model: None,
                        secret_env: Vec::new(),
                        provider_config: None,
                        client_context: None,
                        concurrency: 1,
                        attempts: 1,
                        run_id: Some("cook-loop-missing-provider".to_string()),
                        queue_only: false,
                    },
                    goal: Some("cook fixture".to_string()),
                    to_worktree: "homeboy@fix-agent-task-runner-cook".to_string(),
                    provider_command: None,
                    verify: vec!["cargo test --lib".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                    max_attempts: 2,
                    no_finalize: false,
                    base: "main".to_string(),
                    head: None,
                    title: None,
                    commit_message: None,
                    protected_branches: review::default_protected_branches(),
                    ai_tool: "OpenCode (GPT-5.5)".to_string(),
                    ai_used_for: "test".to_string(),
                },
                ExtensionProviderAgentTaskExecutor::default(),
            )
            .expect("loop reported controlled failure");

            assert_eq!(exit_code, 1);
            assert_eq!(value["schema"], "homeboy/agent-task-loop/v1");
            assert_eq!(value["loop_id"], "cook-loop-missing-provider");
            assert_eq!(value["status"], "policy_failure");
            assert_eq!(value["attempts"][0]["run_id"], "cook-loop-missing-provider");
            assert!(value["stop_reason"]
                .as_str()
                .expect("stop reason")
                .contains("workspace provider command"));
        });
    }

    struct InspectingExecutor {
        run_id: String,
        observed_status: Arc<Mutex<Option<AgentTaskRunRecord>>>,
    }

    impl InspectingExecutor {
        fn noop(run_id: &str) -> Self {
            Self {
                run_id: run_id.to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl AgentTaskExecutorAdapter for InspectingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let record =
                lifecycle_status(&self.run_id).expect("status exists before executor runs");
            *self
                .observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(record);

            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    #[derive(Clone, Default)]
    struct CapturingExecutor {
        observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    impl AgentTaskExecutorAdapter for CapturingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            *self
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());

            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    struct ApplyArtifactExecutor;

    impl AgentTaskExecutorAdapter for ApplyArtifactExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("produced patch".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch-a".to_string(),
                    kind: "patch".to_string(),
                    name: Some("changes.patch".to_string()),
                    path: Some("target/agent-task-review/changes.patch".to_string()),
                    url: None,
                    mime: Some("text/x-diff".to_string()),
                    size_bytes: Some(42),
                    sha256: Some("abc123".to_string()),
                    metadata: Value::Null,
                }],
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "target/agent-task-review/transcript.log".to_string(),
                    label: Some("transcript".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn with_temp_home(run: impl FnOnce()) {
        with_isolated_home(|_| run());
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "plan-a",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("fixture".to_string()),
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }
}
