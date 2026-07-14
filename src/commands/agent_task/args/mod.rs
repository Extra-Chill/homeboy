//! Clap argument and subcommand definitions for the `agent-task` command tree.
//!
//! These types are the durable CLI contract surface. Keeping them in one
//! sibling module lets the `agent_task` root stay a thin dispatcher over the
//! handler modules (`auth`, `controller`, `run`, `status`, `review`). The
//! command tree is grouped by boundary: lifecycle commands own durable run
//! records, cook commands compose lifecycle primitives into reviewer
//! workflows, provider commands expose executor contracts/auth, and controller
//! commands own long-running loop state.

use clap::{Args, Subcommand, ValueEnum};
use homeboy::core::agent_task_service::AgentTaskDiscoveryOptions;
use homeboy::core::agent_tasks::gate::{AgentTaskGateRevealPolicy, VerifyGateOptions};

use super::super::agent_task_dispatch::DispatchArgs;
use super::prompts::AgentTaskPromptsArgs;
use super::review;
use super::tool::AgentTaskToolArgs;

mod auth;
mod controller;
mod fanout;
mod lifecycle;

pub use auth::{
    AgentTaskAuthCommand, AgentTaskAuthMapEnvArgs, AgentTaskAuthMapKeychainBundleArgs,
    AgentTaskAuthRemoveArgs, AgentTaskAuthSetConfigArgs, AgentTaskAuthSetKeychainArgs,
    AgentTaskAuthSetKeychainBundleArgs, AgentTaskAuthStatusArgs,
};
pub use fanout::{
    AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs, AgentTaskFanoutPlanArgs,
    AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs, AgentTaskFanoutSubmitBatchArgs,
};
pub use lifecycle::{
    CancelArgs, DiagnoseArgs, EvidenceArgs, ReplayProviderBoundaryArgs, RetryArgs, RunArgs,
    RunPlanArgs, StatusArgs, SubmitArgs,
};

pub use controller::{
    AgentTaskControllerApplyEventArgs, AgentTaskControllerCommand, AgentTaskControllerDispatchArgs,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerInitArgs,
    AgentTaskControllerMarkHumanReadyArgs, AgentTaskControllerMaterializeArgs,
    AgentTaskControllerPlanArgs, AgentTaskControllerProofArgs, AgentTaskControllerRunArgs,
    AgentTaskControllerRunFromSpecArgs, AgentTaskControllerRunNextArgs,
    AgentTaskControllerStatusArgs, AgentTaskControllerValidateProofArgs,
};

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    /// Readiness: run the one-command cook readiness repair chain and return a single ready/blocked verdict.
    Doctor(AgentTaskDoctorArgs),
    /// Cook: dispatch, promote, verify, retry red gates, and finalize a one-shot PR task.
    Cook(AgentTaskCookArgs),
    /// Loop: operate a durable multi-agent loop with on/off and resume controls.
    Loop(AgentTaskLoopArgs),
    /// Lifecycle: run an agent-task plan through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// Lifecycle: execute a previously submitted durable agent-task run.
    Run(RunArgs),
    /// Lifecycle: claim and execute the oldest queued durable agent-task run.
    RunNext,
    /// Lifecycle: persist an agent-task plan and return a durable run id without executing it.
    Submit(SubmitArgs),
    /// Lifecycle: read durable agent-task run status.
    Status(StatusArgs),
    /// Lifecycle: list durable agent-task runs, newest first.
    List(ListArgs),
    /// Lifecycle: list queued and running durable agent-task runs, newest first.
    Active(ActiveArgs),
    /// Lifecycle: show the latest durable agent-task run.
    Latest(LatestArgs),
    /// Lifecycle: read durable agent-task run scheduler events.
    Logs(StatusArgs),
    /// Lifecycle: list artifacts and evidence refs recorded for a completed run.
    Artifacts(StatusArgs),
    /// Lifecycle: hydrate and summarize evidence refs recorded for a completed run.
    Evidence(EvidenceArgs),
    /// Lifecycle: hydrate run evidence into a structured diagnostic report.
    Diagnose(DiagnoseArgs),
    /// Provider: inspect the latest raw executor input at the provider boundary.
    ReplayProviderBoundary(ReplayProviderBoundaryArgs),
    /// Lifecycle: mark a queued or stale-running durable agent-task run as cancelled.
    Cancel(CancelArgs),
    /// Lifecycle: resume a queued or stale-running durable run.
    Resume(StatusArgs),
    /// Lifecycle: submit a fresh durable run from an existing run's plan.
    Retry(RetryArgs),
    /// Cook/review: run many independent cooks, each with its own worktree/branch/PR.
    Fanout(AgentTaskFanoutArgs),
    /// Review: build a durable aggregate envelope from run state, logs, artifacts, and promotion hints.
    Review(ReviewArgs),
    /// Review: promote a completed generic patch artifact into a managed worktree.
    Promote(PromoteArgs),
    /// Internal adapter that applies a promotion patch to an already materialized workspace.
    #[command(hide = true)]
    PromotionProvider(PromotionProviderArgs),
    /// Review: finalize a green cook run into a review-ready pull request.
    FinalizePr(FinalizePrArgs),
    /// Review: convert deterministic gate results into a cook retry or stop decision.
    GateFeedback(GateFeedbackArgs),
    /// Provider: list extension-declared agent-task executor providers and optional secret readiness.
    Providers(ProvidersArgs),
    /// Prompt store: save, list, show, and remove markdown prompts in Homeboy-owned storage.
    Prompts(AgentTaskPromptsArgs),
    /// Provider: export Homeboy's machine-readable agent-task core contract metadata.
    Contract(ContractArgs),
    /// Controller: compile a declarative loop definition into an agent-task plan.
    CompileLoop(CompileLoopArgs),
    /// Provider: configure and inspect agent-task provider authentication secrets.
    Auth(AgentTaskAuthArgs),
    /// Controller: create, inspect, and resume durable multi-agent loop controller state.
    Controller(AgentTaskControllerArgs),
    /// Internal bridge for provider-runtime agent tool requests.
    #[command(hide = true)]
    Tool(AgentTaskToolArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Maximum number of durable runs to return.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct ActiveArgs {
    /// Maximum number of active durable runs to return.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,

    /// Cancel stale/suspect/unreconciled active runs through the lifecycle path.
    #[arg(long = "reconcile")]
    pub reconcile: bool,

    /// Report reconcile candidates without cancelling durable run records.
    #[arg(long = "dry-run", requires = "reconcile")]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct LatestArgs {
    /// Maximum number of latest durable runs to return.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

impl From<ListArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ListArgs) -> Self {
        AgentTaskDiscoveryOptions { limit: args.limit }
    }
}

impl From<ActiveArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ActiveArgs) -> Self {
        AgentTaskDiscoveryOptions { limit: args.limit }
    }
}

impl From<LatestArgs> for AgentTaskDiscoveryOptions {
    fn from(args: LatestArgs) -> Self {
        AgentTaskDiscoveryOptions { limit: args.limit }
    }
}

/// Shared deterministic verification gate flags. Flattened into every
/// agent-task arg struct that runs `--verify` / `--private-verify` gates so the
/// field group lives in exactly one place while CLI flag names stay identical.
#[derive(Args, Debug, Clone)]
pub struct VerifyGateArgs {
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
}

impl VerifyGateArgs {
    pub fn has_deterministic_gate(&self) -> bool {
        !self.verify.is_empty() || !self.private_verify.is_empty()
    }
}

impl From<VerifyGateArgs> for VerifyGateOptions {
    fn from(args: VerifyGateArgs) -> Self {
        VerifyGateOptions {
            verify: args.verify,
            private_verify: args.private_verify,
            private_gate_reveal: args.private_gate_reveal,
        }
    }
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

#[derive(Args, Debug)]
pub struct ProvidersArgs {
    /// Executor backend to validate against this machine/runner's provider readiness.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Provider id to disambiguate when more than one provider exists for the backend.
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,

    /// Secret environment variable name to check without exposing its value. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,

    /// Fail non-zero when the selected provider is registered but not executable here.
    #[arg(long = "validate-readiness")]
    pub validate_readiness: bool,

    /// Re-discover provider runtime manifests before printing the catalog.
    #[arg(long = "refresh")]
    pub refresh: bool,
}

/// Arguments for `agent-task doctor`: the single preflight/repair path for cook
/// readiness. It chains the provider-contract check and the runner readiness
/// (`runner doctor --scope lab-offload`) checks the operator previously ran by
/// hand, then emits one ready/blocked verdict.
#[derive(Args, Debug)]
pub struct AgentTaskDoctorArgs {
    /// Runner ID to verify readiness against. Use `local`/`localhost`/`self`
    /// for this machine; other values resolve through `homeboy runner` config.
    #[arg(long, value_name = "RUNNER")]
    pub runner: String,

    /// Executor backend the cook will request. Defaults to the configured
    /// coding backend when omitted.
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Provider id to disambiguate when more than one provider exists for the backend.
    #[arg(long, visible_alias = "provider-id", value_name = "PROVIDER_ID")]
    pub selector: Option<String>,

    /// Component/workspace path used as the runner extension parity probe cwd.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,

    /// Required extension ID to resolve on the runner. Repeatable.
    #[arg(long = "extension", value_name = "EXTENSION")]
    pub extensions: Vec<String>,

    /// Required command to resolve on the runner PATH. Repeatable.
    #[arg(long = "require-tool", value_name = "TOOL")]
    pub required_tools: Vec<String>,

    /// Secret environment variable name to check without exposing its value. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,

    /// Apply safe repairs in sequence (reconnect a stale Lab daemon, etc.) instead of reporting only.
    #[arg(long)]
    pub repair: bool,
}

#[derive(Args, Debug)]
pub struct ContractArgs {
    /// Output format for the contract export.
    #[arg(long, default_value = "json", value_enum)]
    pub format: ContractFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ContractFormat {
    Json,
}

#[derive(Args, Debug)]
pub struct CompileLoopArgs {
    /// AgentTaskLoopDefinition JSON file, @file, inline JSON, or - for stdin.
    #[arg(long, value_name = "SPEC")]
    pub definition: String,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskCookArgs {
    #[command(flatten)]
    pub dispatch: DispatchArgs,

    /// First-attempt lifecycle id injected by a controller when execution is
    /// handed to a runner.
    #[arg(long, hide = true)]
    pub attempt_run_id: Option<String>,

    /// Controller-compiled attempt plan transferred to a Lab runner. This keeps
    /// the runner from rebuilding a plan after the controller has persisted it.
    #[arg(long, hide = true)]
    pub attempt_plan: Option<String>,

    /// Repo-cooking goal. Alias for the dispatch prompt when --prompt is omitted.
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,

    /// Target managed worktree handle where candidate patches are promoted.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,

    /// External workspace provider command. When omitted, HOMEBOY_AGENT_TASK_PROMOTION_COMMAND is used.
    #[arg(long, value_name = "COMMAND")]
    pub provider_command: Option<String>,

    /// One argument for an argv-safe external workspace provider invocation.
    /// Repeat this option in provider argv order.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command"
    )]
    pub provider_argv: Vec<String>,

    #[command(flatten)]
    pub gates: VerifyGateArgs,

    /// Maximum cook gate attempts, including the first candidate.
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
    #[arg(long, default_value = "AI-assisted", value_name = "TEXT")]
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
pub struct PromotionProviderArgs {
    /// Materialized Git workspace that receives the promotion patch.
    #[arg(long, value_name = "PATH")]
    pub workspace: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskLoopArgs {
    #[command(subcommand)]
    pub command: AgentTaskLoopCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskLoopCommand {
    /// Define or update a durable loop from a repo-authored multi-agent spec.
    Define(AgentTaskLoopDefineArgs),
    /// Read durable loop state, revolution counters, pending handoffs, and diagnostics.
    Status(AgentTaskLoopStatusArgs),
    /// Turn a durable loop on and execute pending handoffs until it stops or hits its revolution limit.
    Resume(AgentTaskLoopResumeArgs),
    /// Turn a durable loop off without deleting its state.
    Stop(AgentTaskLoopStatusArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskLoopDefineArgs {
    /// Repo loop spec JSON, @file, or - for stdin.
    #[arg(value_name = "SPEC")]
    pub spec: String,

    /// Define the loop in the on state so resume may execute handoffs.
    #[arg(long, conflicts_with = "off")]
    pub on: bool,

    /// Define the loop in the off state; state is persisted but handoffs are not resumed.
    #[arg(long, conflicts_with = "on")]
    pub off: bool,

    /// Maximum revolutions before resume stops the loop.
    #[arg(long = "revolution-limit", value_name = "N")]
    pub revolution_limit: Option<u32>,

    /// Execute pending handoffs after defining the loop. Requires --on.
    #[arg(long)]
    pub resume: bool,

    /// Executor backend to use for loop-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,

    /// Extension-provider selector: the Homeboy executor provider id (e.g.
    /// `sample.executor-provider`) that runs loop-spawned dispatch
    /// actions when the action omits one. This is not model/runtime provider
    /// configuration; pass runtime-specific values in --dispatch-provider-config.
    /// Run `homeboy agent-task providers` for valid ids.
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,

    /// Model override to use for loop-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,

    /// Agent/model provider config (JSON, @file, or -): the nested AI
    /// runtime/provider/model the selected executor uses for loop-spawned
    /// dispatch actions when the action omits one. Put runtime-specific provider
    /// selection here, not in --dispatch-selector.
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskLoopStatusArgs {
    /// Durable loop id returned by `agent-task loop define`.
    pub loop_id: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskLoopResumeArgs {
    /// Durable loop id returned by `agent-task loop define`.
    pub loop_id: String,

    /// Override the persisted maximum revolutions before resume stops the loop.
    #[arg(long = "revolution-limit", value_name = "N")]
    pub revolution_limit: Option<u32>,

    /// Executor backend to use for loop-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,

    /// Extension-provider selector: the Homeboy executor provider id (e.g.
    /// `sample.executor-provider`) that runs loop-spawned dispatch
    /// actions when the action omits one. This is not model/runtime provider
    /// configuration; pass runtime-specific values in --dispatch-provider-config.
    /// Run `homeboy agent-task providers` for valid ids.
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,

    /// Model override to use for loop-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,

    /// Agent/model provider config (JSON, @file, or -): the nested AI
    /// runtime/provider/model the selected executor uses for loop-spawned
    /// dispatch actions when the action omits one. Put runtime-specific provider
    /// selection here, not in --dispatch-selector.
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
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

    /// One argument for an argv-safe external workspace provider invocation.
    /// Repeat this option in provider argv order.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command"
    )]
    pub provider_argv: Vec<String>,
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

    /// One argument for an argv-safe external workspace provider invocation.
    /// Repeat this option in provider argv order.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command"
    )]
    pub provider_argv: Vec<String>,

    /// Outcome task id to select when SOURCE is an aggregate.
    #[arg(long, value_name = "TASK_ID")]
    pub task_id: Option<String>,

    /// Patch artifact id to select when the outcome contains multiple patches.
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub artifact_id: Option<String>,

    /// Validate and report the selected promotion without creating/applying.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub gates: VerifyGateArgs,
}

#[derive(Args, Debug)]
pub struct FinalizePrArgs {
    /// Durable cook/agent-task run id to hydrate before finalization.
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

    /// Reviewer-facing summary. When omitted, --title is used for legacy compatibility.
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,

    /// Reviewer-relevant behavior change. Repeatable.
    #[arg(long = "what-changed", value_name = "TEXT")]
    pub what_changed: Vec<String>,

    /// Runnable reviewer test as COMMAND=>EXPECTED. Legacy targeted checks become COMMAND=>passes.
    #[arg(long = "test-step", value_name = "TEXT")]
    pub test_steps: Vec<String>,

    /// Compatibility impact. When omitted, a deterministic legacy disclosure is used.
    #[arg(long, value_name = "TEXT")]
    pub compatibility: Option<String>,

    /// Typed issue reference closed by this PR. Repeatable; never inferred from URLs.
    #[arg(long = "closes", value_name = "REF")]
    pub closes: Vec<String>,

    /// Typed issue reference related to this PR. Repeatable.
    #[arg(long = "relates-to", value_name = "REF")]
    pub relates_to: Vec<String>,

    /// Typed reviewer section override as TARGET=VALUE@PROVENANCE. TARGET is summary, what_changed, or compatibility.
    #[arg(long = "review-override", value_name = "TARGET=VALUE@PROVENANCE")]
    pub review_overrides: Vec<String>,

    /// Explicit migration mode for a manually prepared candidate without a durable run record.
    #[arg(long)]
    pub manual_finalization: bool,
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
