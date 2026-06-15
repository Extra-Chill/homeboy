use clap::{Args, Subcommand};
use homeboy::core::agent_tasks::gate::AgentTaskGateRevealPolicy;
use serde::Serialize;
use serde_json::Value;
use std::io::Read;

use homeboy::core::agent_tasks::controller_service as agent_task_controller_service;
use homeboy::core::agent_tasks::controller_service::{
    ControllerApplyEventRequest, ControllerDispatchHook, ControllerInitRequest,
    ControllerMarkHumanReadyRequest,
};
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};
use homeboy::core::agent_tasks::secrets as agent_task_secrets;
use homeboy::core::agent_tasks::service as agent_task_service;
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
    /// List durable agent-task runs, newest first.
    List,
    /// List queued and running durable agent-task runs, newest first.
    Active,
    /// Show the latest durable agent-task run.
    Latest,
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
        AgentTaskCommand::Cook(dispatch_args) => {
            super::agent_task_dispatch::cook(dispatch_args, global)
        }
        AgentTaskCommand::Loop(loop_args) => run_loop(loop_args),
        AgentTaskCommand::Dispatch(dispatch_args) => dispatch(dispatch_args, global),
        AgentTaskCommand::RunPlan(run_args) => run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run_submitted(status_args),
        AgentTaskCommand::RunNext => run_next(),
        AgentTaskCommand::Submit(submit_args) => submit(submit_args),
        AgentTaskCommand::Status(status_args) => status(status_args),
        AgentTaskCommand::List => list_runs(agent_task_service::AgentTaskDiscoveryFilter::All),
        AgentTaskCommand::Active => list_runs(agent_task_service::AgentTaskDiscoveryFilter::Active),
        AgentTaskCommand::Latest => list_runs(agent_task_service::AgentTaskDiscoveryFilter::Latest),
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
        AgentTaskControllerCommand::Init(init_args) => {
            let record = agent_task_controller_service::init(ControllerInitRequest {
                loop_id: init_args.loop_id,
                phase: init_args.phase,
                config_version: init_args.config_version,
            })?;
            Ok((command_json_value(record)?, 0))
        }
        AgentTaskControllerCommand::Status(status_args) => {
            let report = homeboy::core::agent_tasks::loop_controller::controller_status_report(
                &status_args.loop_id,
            )?;
            Ok((command_json_value(report)?, 0))
        }
        AgentTaskControllerCommand::List => {
            let report = agent_task_controller_service::list()?;
            Ok((command_json_value(report)?, 0))
        }
        AgentTaskControllerCommand::ApplyEvent(event_args) => apply_controller_event(event_args),
        AgentTaskControllerCommand::RunNext(run_args) => controller_run_next(run_args),
        AgentTaskControllerCommand::Run(run_args) => controller_run_action(run_args),
        AgentTaskControllerCommand::Resume(run_args) => controller_resume(run_args),
        AgentTaskControllerCommand::MarkHumanReady(ready_args) => {
            let record =
                agent_task_controller_service::mark_human_ready(ControllerMarkHumanReadyRequest {
                    loop_id: ready_args.loop_id,
                    entity_id: ready_args.entity_id,
                    reason: ready_args.reason,
                })?;
            Ok((command_json_value(record)?, 0))
        }
    }
}

fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

/// Bridge controller spawn-task `"dispatch"` requests into the CLI dispatch path.
#[derive(Clone)]
struct CliDispatchHook<E> {
    executor: E,
}

impl<E> ControllerDispatchHook for CliDispatchHook<E>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    fn dispatch(&self, request: &Value) -> homeboy::core::Result<(Value, i32)> {
        let dispatch_args = dispatch_args_from_controller_request(request)?;
        dispatch_with_executor(dispatch_args, self.executor.clone())
    }
}

fn dispatch_args_from_controller_request(request: &Value) -> homeboy::core::Result<DispatchArgs> {
    use agent_task_controller_service::{
        optional_bool, optional_string, optional_string_array, optional_u32, optional_usize,
    };
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

fn apply_controller_event(args: AgentTaskControllerApplyEventArgs) -> CmdResult<Value> {
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
    let report = agent_task_controller_service::apply_event(ControllerApplyEventRequest {
        loop_id: args.loop_id,
        event_type: args.event_type,
        event_id: args.event_id,
        event_key: args.event_key,
        entity_id: args.entity_id,
        payload,
    })?;
    Ok((command_json_value(report)?, 0))
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
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
    };
    let result = agent_task_controller_service::run_next(&loop_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

fn controller_run_action_with_executor<E>(
    loop_id: String,
    action_id: String,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
    };
    let result =
        agent_task_controller_service::run_action(&loop_id, &action_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

fn controller_resume_with_executor<E>(loop_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
    };
    let result = agent_task_controller_service::resume(&loop_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
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

    let mut dispatch_args = args.dispatch.clone();
    if dispatch_args.prompt.is_none() {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args.queue_only = false;
    let (dispatch_value, _dispatch_exit) = dispatch_with_executor(dispatch_args, executor.clone())?;
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
            verify: args.verify,
            private_verify: args.private_verify,
            private_gate_reveal: args.private_gate_reveal,
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

fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let plan = agent_task_service::read_plan(&args.plan)?;
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn run_loaded_plan<E>(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_loaded_plan(plan, record_run_id, executor)?;
    Ok((
        serde_json::to_value(result.value).unwrap_or(Value::Null),
        result.exit_code,
    ))
}

fn run_submitted(args: StatusArgs) -> CmdResult<Value> {
    run_submitted_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_submitted_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_submitted(run_id, executor)?;
    Ok((
        serde_json::to_value(result.value).unwrap_or(Value::Null),
        result.exit_code,
    ))
}

fn run_next() -> CmdResult<Value> {
    run_next_with_executor(ExtensionProviderAgentTaskExecutor::discover())
}

fn run_next_with_executor<E>(executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_next(executor)?;
    let Some(aggregate) = result.value else {
        return Ok((serde_json::json!({ "claimed": false }), 0));
    };
    Ok((
        serde_json::to_value(aggregate).unwrap_or(Value::Null),
        result.exit_code,
    ))
}

fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let record = agent_task_service::submit_plan_spec(&args.plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn status(args: StatusArgs) -> CmdResult<Value> {
    let record = agent_task_service::status(&args.run_id)?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn list_runs(filter: agent_task_service::AgentTaskDiscoveryFilter) -> CmdResult<Value> {
    let report = agent_task_service::discover_runs(filter)?;
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

fn logs(args: StatusArgs) -> CmdResult<Value> {
    let log = agent_task_service::logs(&args.run_id)?;
    Ok((serde_json::to_value(log).unwrap_or(Value::Null), 0))
}

fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    Ok((serde_json::to_value(artifacts).unwrap_or(Value::Null), 0))
}

fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let record = agent_task_service::cancel(&args.run_id, args.reason.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn resume(args: StatusArgs) -> CmdResult<Value> {
    run_resume_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_resume_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::resume(run_id, executor)?;
    Ok((
        serde_json::to_value(result.value).unwrap_or(Value::Null),
        result.exit_code,
    ))
}

fn retry(args: RetryArgs) -> CmdResult<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_tasks::lifecycle::{
        self as agent_task_lifecycle, status as lifecycle_status, AgentTaskRunRecord,
        AgentTaskRunState,
    };
    use homeboy::core::agent_tasks::loop_controller::{
        self as agent_task_loop_controller, AgentTaskLoopActionStatus, AgentTaskLoopPolicyAction,
    };
    use homeboy::core::agent_tasks::scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use homeboy::core::agent_tasks::{
        AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskLimits,
        AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
        AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
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
            homeboy::core::agent_tasks::AgentTaskWorkspaceMode::Existing
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
                AgentTaskLoopPolicyAction::WaitForEvent(
                    agent_task_loop_controller::AgentTaskLoopWait {
                        wait_key: "wait-a".to_string(),
                        event_type: "task.completed".to_string(),
                        entity_id: None,
                        external_ref: None,
                        timeout_at: None,
                        escalation_policy: None,
                        status: agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    },
                ),
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
