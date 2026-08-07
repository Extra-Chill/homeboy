use clap::{Args, Subcommand};

use super::cook::VerifyGateArgs;

#[derive(Args, Debug)]
pub struct AgentTaskFanoutArgs {
    #[command(subcommand)]
    pub command: AgentTaskFanoutCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskFanoutCommand {
    /// Cook a wave of independent tasks, one child cook per issue.
    ///
    /// Every child requires a deterministic gate from shared --verify/
    /// --private-verify inputs or --verification-profiles. A child that cannot
    /// verify its work cannot promote it (#9838).
    CookBatch(Box<AgentTaskFanoutCookBatchArgs>),
    /// Normalize and inspect a batch-cook plan without submitting or running it.
    Plan(AgentTaskFanoutPlanArgs),
    /// Submit a batch of independent cooks and print the exact per-cook commands
    /// for runner or operator execution.
    Submit(AgentTaskFanoutSubmitArgs),
    /// Submit a durable batch of independent `AgentTaskPlan` tasks as one queued
    /// child run per packet.
    ///
    /// Provider-neutral by design: drive execution with `agent-task run-next` or
    /// an existing runner queue loop, then reconcile with `fanout status` and
    /// `fanout artifacts`.
    SubmitBatch(AgentTaskFanoutSubmitBatchArgs),
    /// Read durable batch state and per-child run status.
    Status(AgentTaskFanoutBatchStatusArgs),
    /// Resume a durable fanout batch after coordinator loss: idempotently harvest terminal children through gates, commit, push, and PR finalization.
    Resume(AgentTaskFanoutBatchStatusArgs),
    /// List artifacts recorded by a durable batch's child runs.
    Artifacts(AgentTaskFanoutBatchStatusArgs),
    /// Execute each cook in a batch-cook plan through the cook-loop service and
    /// return a batch summary.
    ///
    /// Successful child cooks open or update their own pull requests.
    RunPlan(AgentTaskFanoutRunPlanArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutCookBatchArgs {
    #[arg(value_name = "ISSUE_URL", required = true)]
    pub issues: Vec<String>,
    #[arg(long = "repo", value_name = "REPO")]
    pub repo: String,
    #[arg(long = "from", default_value = "origin/main", value_name = "REF")]
    pub from: String,
    #[arg(long = "base", default_value = "main", value_name = "BRANCH")]
    pub base: String,
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
    #[arg(long = "provider-profile", value_name = "PROFILE")]
    pub provider_profile: Option<String>,
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,
    /// AI tool disclosure recorded in every child PR's assistance attribution.
    /// When omitted, each child derives its disclosure from its effective provider
    /// and model selection.
    #[arg(long = "ai-tool", value_name = "TEXT")]
    pub ai_tool: Option<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// JSON verification profile declaration, inline or @file.json. Profiles
    /// append to or replace shared --verify/--private-verify gates per issue.
    #[arg(long = "verification-profiles", value_name = "JSON")]
    pub verification_profiles: Option<String>,
    /// Maximum number of child cooks to run at once.
    ///
    /// Without this, the ceiling is the host's
    /// `/agent_task/max_batch_concurrency` config or a built-in default. A
    /// declared resource budget and the number of children can lower the
    /// result further, never raise it. The effective limit and the reason for
    /// it are reported as `concurrency` in the batch result.
    #[arg(
        long = "max-concurrency",
        value_parser = clap::value_parser!(usize).range(1..),
        value_name = "N"
    )]
    pub max_concurrency: Option<usize>,
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    #[arg(long = "run-plan")]
    pub run_plan: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutInputArgs {
    #[arg(long = "input", value_name = "SPEC")]
    pub input: String,
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutPlanArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    #[arg(long = "run-id", value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitBatchArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    #[arg(long = "batch-id", value_name = "ID")]
    pub batch_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutBatchStatusArgs {
    pub batch_id: String,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutRunPlanArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    #[arg(long = "record-run-id", value_name = "ID")]
    pub record_run_id: Option<String>,
    /// AI tool disclosure recorded in every child PR's assistance attribution.
    /// Overrides the persisted plan value for this execution.
    #[arg(long = "ai-tool", value_name = "TEXT")]
    pub ai_tool: Option<String>,
    /// Maximum number of child cooks to run at once. See
    /// `fanout cook-batch --max-concurrency`.
    #[arg(
        long = "max-concurrency",
        value_parser = clap::value_parser!(usize).range(1..),
        value_name = "N"
    )]
    pub max_concurrency: Option<usize>,
}
