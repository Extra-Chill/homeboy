use clap::{Args, Subcommand};

use super::cook::{
    parse_provider_evidence_input, AgentTaskProviderEvidenceInput, VerifyGateArgs,
    PROVIDER_EVIDENCE_DECLARATION,
};

#[derive(Args, Debug)]
pub struct AgentTaskFanoutArgs {
    #[command(subcommand)]
    pub command: AgentTaskFanoutCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskFanoutCommand {
    /// Cook a wave of independent issues, one child Cook per issue.
    ///
    /// Shorthand for a `homeboy/workflow/v1` instance: a `prepare` step
    /// (`agent-task batch-prepare`) resolves the base once, names each child's
    /// branch, and seeds the Lab runner with the shared base workspace bundle;
    /// a `cook` fan-out step then runs one `agent-task cook` per issue, at most
    /// `--max-concurrency` at a time, and its `--join` decides whether the batch
    /// succeeded. Without `--run-plan` the command prints the compiled workflow
    /// and admits nothing. Observe and cancel the instance with `homeboy
    /// workflow status|cancel <instance>`; re-running the same command reports
    /// the same instance.
    ///
    /// Every child requires a deterministic gate from shared --verify/
    /// --private-verify inputs. A child that cannot verify its work cannot
    /// promote it (#9838).
    #[command(
        after_help = "Quick start:\n  homeboy agent-task fanout cook-batch --repo REPO --verify 'homeboy review test REPO' ISSUE_URL...\n\nOne repository per batch: every issue in a cook-batch belongs to --repo.\n\nTwo phases: without --run-plan, cook-batch validates the batch and prints the compiled workflow. Add --run-plan to admit it; then follow it with `homeboy workflow status <instance> --watch` and stop it with `homeboy workflow cancel <instance>`.\n\nVerification is required: every child needs a shared --verify/--private-verify gate.\n\nPlacement: run every child on Lab with:\n  homeboy --placement lab agent-task fanout cook-batch --repo REPO --verify 'homeboy review test REPO' ISSUE_URL... --run-plan\n\nJoin: `--join all` (default) needs every child to succeed; `--join any` one; `--join N` at least N.\n\nUse --help-full for provider, gate, and environment controls."
    )]
    CookBatch(Box<AgentTaskFanoutCookBatchArgs>),
}

#[derive(Args, Debug, Clone)]
#[command(disable_help_flag = true)]
pub struct AgentTaskFanoutCookBatchArgs {
    /// Show compact task-first cook-batch help. Use `--help-full` for the
    /// complete cook-batch option reference.
    #[arg(short = 'h', long, action = clap::ArgAction::HelpShort)]
    pub help: Option<bool>,
    /// Show the complete cook-batch option reference.
    #[arg(long = "help-full", action = clap::ArgAction::HelpLong)]
    pub help_full: Option<bool>,
    /// GitHub issue URL cooked by one child of the wave. Repeat for multiple
    /// issues; every URL must be unique.
    #[arg(value_name = "ISSUE_URL", required = true)]
    pub issues: Vec<String>,
    /// Registered repository/component slug or exact registered primary checkout path.
    #[arg(long = "repo", value_name = "REPO_SLUG_OR_PRIMARY_PATH")]
    pub repo: String,
    /// Pull-request base branch every child is cut from and targets. When
    /// omitted, `batch-prepare` resolves the repository's remote default branch.
    #[arg(long = "base", value_name = "BRANCH")]
    pub base: Option<String>,
    /// Prefix for generated child branches. Each child branch is
    /// `<PREFIX>/issue-<number>-<repo-slug>` (default `fix`).
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,
    /// Workflow instance name. Distinct names run the same batch separately;
    /// the same name (or none) re-attaches to the existing instance.
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    /// Prompt template rendered for every child cook. `{issue_url}`,
    /// `{issue_ref}`, `{repo}`, `{branch}`, and `{worktree}` are substituted.
    /// When omitted, each child's prompt is its issue URL.
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,
    /// Executor backend serving every child cook.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    /// Executor provider ID selecting which installed provider serves the
    /// backend. Only needed when one backend is served by multiple providers.
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    /// Model name forwarded to the selected provider for every child cook.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
    /// Explicitly acknowledge that the model may displace configured routes.
    #[arg(long = "acknowledge-model-override")]
    pub acknowledge_model_override: bool,
    /// Named provider profile declared by an installed provider's CLI,
    /// supplying default backend/selector/model/provider-config values for
    /// every child. Explicit flags win over the profile.
    #[arg(long = "provider-profile", value_name = "PROFILE")]
    pub provider_profile: Option<String>,
    /// Name of an environment variable that holds a provider credential for
    /// every child. Repeatable. Values are resolved by the provider at
    /// execution, never read or recorded here.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    /// Provider-specific configuration forwarded to every child's provider
    /// invocation, as inline JSON.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,
    #[arg(long = "provider-evidence", value_name = "JSON", help = PROVIDER_EVIDENCE_DECLARATION, value_parser = parse_provider_evidence_input)]
    pub provider_evidence_inputs: Vec<AgentTaskProviderEvidenceInput>,
    /// AI tool disclosure recorded in every child PR's assistance attribution.
    #[arg(long = "ai-tool", value_name = "TEXT")]
    pub ai_tool: Option<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Maximum number of child cooks to run at once. Defaults to the host's
    /// `/agent_task/max_batch_concurrency` config or 4.
    #[arg(
        long = "max-concurrency",
        value_parser = clap::value_parser!(u32).range(1..),
        value_name = "N"
    )]
    pub max_concurrency: Option<u32>,
    /// Wall-clock budget, in seconds, for the whole batch. On expiry the
    /// workflow ends `budget_exhausted` and cancels every running child.
    #[arg(
        long = "max-duration",
        value_parser = clap::value_parser!(u64).range(1..),
        value_name = "SECONDS"
    )]
    pub max_duration: Option<u64>,
    /// When the batch succeeds: `all` (default) every child, `any` at least
    /// one, or a number N for at least N children.
    #[arg(long = "join", default_value = "all", value_name = "all|any|N", value_parser = parse_join)]
    pub join: CookBatchJoin,
    /// Validate the batch and print the compiled workflow without admitting it.
    /// This is also the default without `--run-plan`.
    #[arg(long = "preview", conflicts_with = "run_plan")]
    pub preview: bool,
    /// Admit the compiled workflow instance and return its identity.
    #[arg(long = "run-plan")]
    pub run_plan: bool,
}

/// The `--join` fan-in policy of a cook-batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookBatchJoin {
    All,
    Any,
    MinSucceeded(u32),
}

fn parse_join(value: &str) -> Result<CookBatchJoin, String> {
    match value {
        "all" => Ok(CookBatchJoin::All),
        "any" => Ok(CookBatchJoin::Any),
        number => number
            .parse::<u32>()
            .ok()
            .filter(|min| *min >= 1)
            .map(CookBatchJoin::MinSucceeded)
            .ok_or_else(|| "expected `all`, `any`, or a positive number of children".to_string()),
    }
}

/// `agent-task batch-prepare`: the planning step of a cook-batch workflow.
#[derive(Args, Debug, Clone)]
pub struct AgentTaskBatchPrepareArgs {
    /// GitHub issue URL of one child. Repeat for every child of the batch.
    #[arg(value_name = "ISSUE_URL", required = true)]
    pub issues: Vec<String>,
    /// Registered repository/component slug or exact registered primary checkout path.
    #[arg(long = "repo", value_name = "REPO_SLUG_OR_PRIMARY_PATH")]
    pub repo: String,
    /// Base branch. When omitted, the repository's remote default branch.
    #[arg(long = "base", value_name = "BRANCH")]
    pub base: Option<String>,
    /// Prefix for generated child branches (default `fix`).
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,
    /// Prompt template rendered for every child; see `fanout cook-batch`.
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,
    /// Lab runner the children run on. Its workspace bundle for the base
    /// commit is transferred once here and reused by every child.
    #[arg(long = "bundle-runner", value_name = "RUNNER_ID")]
    pub bundle_runner: Option<String>,
}
