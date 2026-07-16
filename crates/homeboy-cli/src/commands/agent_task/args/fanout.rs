use clap::{Args, Subcommand};

use super::VerifyGateArgs;

#[derive(Args, Debug)]
pub struct AgentTaskFanoutArgs {
    #[command(subcommand)]
    pub command: AgentTaskFanoutCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskFanoutCommand {
    /// Build an operator-ready batch-cook fanout from issue URLs and create/reuse task worktrees.
    CookBatch(AgentTaskFanoutCookBatchArgs),
    /// Normalize a batch-cook fanout plan with independent cooks, worktrees, and PR targets.
    Plan(AgentTaskFanoutPlanArgs),
    /// Return the independent cook commands for a batch-cook fanout plan.
    Submit(AgentTaskFanoutSubmitArgs),
    /// Submit each independent fanout task as its own durable child run.
    SubmitBatch(AgentTaskFanoutSubmitBatchArgs),
    /// Reconcile and read a durable fanout batch.
    Status(AgentTaskFanoutBatchStatusArgs),
    /// Collate artifacts from every child run in a durable fanout batch.
    Artifacts(AgentTaskFanoutBatchStatusArgs),
    /// Run each independent cook and let each cook open/update its own PR.
    RunPlan(AgentTaskFanoutRunPlanArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutCookBatchArgs {
    /// Issue URLs to cook. Each URL becomes one branch, worktree, prompt, and PR target.
    #[arg(value_name = "ISSUE_URL", required = true)]
    pub issues: Vec<String>,

    /// Registered component/repo handle.
    #[arg(long = "repo", value_name = "REPO")]
    pub repo: String,

    /// Git ref used when creating worktrees.
    #[arg(long = "from", default_value = "origin/main", value_name = "REF")]
    pub from: String,

    /// Pull request base branch used by cook finalization.
    #[arg(long = "base", default_value = "main", value_name = "BRANCH")]
    pub base: String,

    /// Prefix for generated branches.
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,

    /// Batch fanout id. Defaults to cook-batch-<repo>-<first-issue>-<count>.
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,

    /// Prompt template. Supports {issue_url}, {issue_ref}, {repo}, {branch}, and {worktree}.
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,

    /// Default executor backend for all generated cooks.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Provider id for all generated cooks.
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,

    /// Optional model override passed through to the provider and PR disclosure.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,

    /// Named provider profile declared by an installed executor provider. Explicit --backend/--model values win.
    #[arg(long = "provider-profile", value_name = "PROFILE")]
    pub provider_profile: Option<String>,

    /// Secret environment variable name to hydrate for the provider. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,

    /// Provider config JSON object, @file, or - for stdin. Reused by every cook.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,

    #[command(flatten)]
    pub gates: VerifyGateArgs,

    /// Report commands/spec without creating worktrees.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Run the generated batch-cook plan immediately after worktree creation succeeds.
    #[arg(long = "run-plan")]
    pub run_plan: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutInputArgs {
    /// Batch-cook fanout spec JSON file, @file, inline JSON, or - for stdin.
    #[arg(long = "input", value_name = "SPEC")]
    pub input: String,

    /// Batch fanout id. Overrides the spec fanout_id when supplied.
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,

    /// Default executor backend for cooks without a backend.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Default extension-provider selector for cooks without one. Run `homeboy agent-task providers` for valid provider ids.
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,

    /// Default model override for cooks without one.
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

    /// Optional durable run id. Generated when omitted.
    #[arg(long = "run-id", value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitBatchArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,

    /// Optional durable batch id. Generated when omitted.
    #[arg(long = "batch-id", value_name = "ID")]
    pub batch_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutBatchStatusArgs {
    /// Durable fanout batch id returned by submit-batch.
    pub batch_id: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutRunPlanArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,

    /// Also persist the completed run lifecycle record under this id.
    #[arg(long = "record-run-id", value_name = "ID")]
    pub record_run_id: Option<String>,
}
