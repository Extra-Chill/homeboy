use clap::{Args, Subcommand};

use super::cook::{
    parse_provider_evidence_input, AgentTaskProviderEvidenceInput, VerifyGateArgs,
    PROVIDER_EVIDENCE_DECLARATION,
};

pub const VERIFICATION_PROFILES_EXAMPLE: &str = r#"{"profiles":{"review":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test","my-component"],"suite_timeout_seconds":1800}}},"assignments":[{"selector":"https://github.com/owner/repo/issues/123","profile":"review"}]}"#;

const VERIFICATION_PROFILES_HELP: &str = r#"JSON verification profile declaration, inline or @file.json.

Profiles select one typed `plan`; shared `--verify` and `--private-verify` remain explicit shell escape hatches. Assignment selectors accept a full issue URL, an `owner/repo#number` issue key, or the generated `issue-number` child selector.

Complete example:
  {"profiles":{"review":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test","my-component"],"suite_timeout_seconds":1800}}},"assignments":[{"selector":"https://github.com/owner/repo/issues/123","profile":"review"}]}"#;

#[derive(Args, Debug)]
pub struct AgentTaskFanoutArgs {
    #[command(subcommand)]
    pub command: AgentTaskFanoutCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskFanoutCommand {
    /// Cook a wave of independent tasks, one child cook per issue.
    ///
    /// TWO-PHASE MODEL: this command plans first and executes only when told
    /// to. Without `--run-plan` it resolves the batch — issues, repository,
    /// default branch, gates, backend — and creates or reuses every child
    /// worktree, but it does NOT dispatch any cook; run the returned
    /// `fanout run-plan` command (or pass `--run-plan`) to execute the wave.
    /// `--preview` (historical spelling `--dry-run`) is the fully static form:
    /// it validates the same plan without touching repositories, providers,
    /// worktrees, or files, mirroring `agent-task cook --preview`.
    ///
    /// Every child requires a deterministic gate from shared --verify/
    /// --private-verify inputs or --verification-profiles. A child that cannot
    /// verify its work cannot promote it (#9838).
    CookBatch(Box<AgentTaskFanoutCookBatchArgs>),
    /// Normalize and inspect a batch-cook plan without submitting or running it.
    ///
    /// Reads an existing persisted plan with `--input <SPEC>`, or plans
    /// statically from `--repo <REPO_SLUG>` plus one or more issue URLs — the
    /// same input `fanout cook-batch` accepts, without any side effects.
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
    /// GitHub issue URL cooked by one child of the wave. Repeat for multiple
    /// issues; every URL must be unique and resolve through the tracker.
    #[arg(value_name = "ISSUE_URL", required = true)]
    pub issues: Vec<String>,
    /// Registered repository/component slug or exact registered primary checkout path.
    ///
    /// Component identities and aliases resolve to their canonical owning
    /// repository before child planning and worktree handoff.
    #[arg(long = "repo", value_name = "REPO_SLUG_OR_PRIMARY_PATH")]
    pub repo: String,
    /// Controller-resolved component selector retained across replay.
    #[arg(long = "component", value_name = "COMPONENT_ID", hide = true)]
    pub component: Option<String>,
    /// Source ref used to create every child worktree. When omitted, this is
    /// inferred from the repository default branch. An explicit value wins and
    /// must resolve to the same commit as --base.
    #[arg(long = "from", value_name = "REF")]
    pub from: Option<String>,
    /// Pull-request base branch. When omitted, Homeboy resolves the registered
    /// repository's remote default branch before any worktree mutation.
    #[arg(long = "base", value_name = "BRANCH")]
    pub base: Option<String>,
    /// Controller-resolved default-branch provenance persisted with the plan.
    #[arg(skip)]
    pub base_resolution: Option<serde_json::Value>,
    /// Prefix for generated child branches. Each child branch is
    /// `<PREFIX>/issue-<number>-<repo-slug>` (default `fix`, yielding
    /// `fix/issue-12-owner-repo`).
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,
    /// Explicit identity for this batch plan and its durable records. Defaults
    /// to a content-derived `cook-batch-...` id from the resolved children;
    /// supply your own to keep a stable identity across replans.
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    /// Bind one issue URL to an existing provider-managed worktree handle.
    /// Repeat as `--worktree ISSUE_URL=HANDLE`. Every supplied issue must have
    /// exactly one binding; Homeboy validates and adopts the exact destination
    /// instead of requesting provider creation.
    #[arg(long = "worktree", value_name = "ISSUE_URL=HANDLE")]
    pub worktrees: Vec<String>,
    /// Prompt template rendered for every child cook. `{issue_url}`,
    /// `{issue_ref}`, `{repo}`, `{branch}`, and `{worktree}` are substituted.
    /// Omit for the default fix-the-issue prompt.
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,
    /// Executor backend serving every child cook. Omit to use the configured
    /// `agent_task.default_backend`; the resolved backend is validated up front
    /// and pinned identically for all children.
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
    /// Named provider profile declared by an installed provider's CLI,
    /// supplying default backend/selector/model/provider-config values for
    /// every child. Explicit flags win over the profile.
    #[arg(long = "provider-profile", value_name = "PROFILE")]
    pub provider_profile: Option<String>,
    /// Name of an environment variable that holds a provider credential for
    /// this batch. Repeatable. Values are resolved by the provider at
    /// execution, never read or recorded here.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    /// Provider-specific configuration forwarded to every child's provider
    /// invocation, as inline JSON, `@FILE`, or `-` for stdin.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,
    #[arg(long = "provider-evidence", value_name = "JSON", help = PROVIDER_EVIDENCE_DECLARATION, value_parser = parse_provider_evidence_input)]
    pub provider_evidence_inputs: Vec<AgentTaskProviderEvidenceInput>,
    /// AI tool disclosure recorded in every child PR's assistance attribution.
    /// When omitted, each child derives its disclosure from its effective provider
    /// and model selection.
    #[arg(long = "ai-tool", value_name = "TEXT")]
    pub ai_tool: Option<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    #[arg(
        long = "verification-profiles",
        value_name = "JSON",
        help = VERIFICATION_PROFILES_HELP
    )]
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
        value_parser = clap::value_parser!(u32).range(1..),
        value_name = "N"
    )]
    pub max_concurrency: Option<u32>,
    /// Wall-clock budget, in seconds, for the whole batch — every child, every
    /// attempt, and every gate.
    ///
    /// The per-provider and per-gate timeouts bound only the parts, and the
    /// parts multiply: with `--max-attempts 3` and five gates a single child
    /// can legally run for hours. On expiry each child terminalizes as
    /// `timed_out` at its next attempt or gate boundary, and a provider still
    /// running has its process tree terminated. Unset means no budget, which
    /// is the existing behaviour.
    #[arg(
        long = "max-duration",
        value_parser = clap::value_parser!(u64).range(1..),
        value_name = "SECONDS"
    )]
    pub max_duration: Option<u64>,
    /// Resolve and validate the batch without side effects: no repository
    /// hydration, provider dispatch, worktree creation, or file reads. Prints
    /// the static plan, worktree projection, preflight, and a replayable
    /// command — the batch-wide counterpart of `agent-task cook --preview`.
    /// `--dry-run` is accepted as the historical spelling of this flag.
    #[arg(long = "preview", alias = "dry-run")]
    pub preview: bool,
    /// Maximum wall-clock budget for each bounded static --preview planning
    /// phase (default 10 seconds per phase).
    #[arg(
        long = "dry-run-planner-timeout-seconds",
        value_parser = clap::value_parser!(u64).range(1..),
        value_name = "SECONDS"
    )]
    pub dry_run_planner_timeout_seconds: Option<u64>,
    /// Execute the planned wave in this invocation. After admission and
    /// worktree preflight, every child cook runs through the cook-loop service
    /// and successful children open or update their own pull requests.
    /// Without this flag the command only plans the batch and creates or
    /// reuses the child worktrees — see the two-phase model above.
    #[arg(long = "run-plan")]
    pub run_plan: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutInputArgs {
    /// Plan input: inline JSON, `@FILE`, or `-` for stdin. `plan` and `submit`
    /// expect a batch-cook fanout plan (`homeboy/agent-task-batch-cook-plan/v1`);
    /// `submit-batch` and `run-plan` expect an `AgentTaskPlan` JSON spec.
    #[arg(long = "input", value_name = "SPEC")]
    pub input: String,
    /// Stable identity recorded for the submitted batch. Omit to keep the
    /// identity already carried by the plan.
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    /// Executor backend override applied to the loaded plan's cooks.
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
    /// Model name override forwarded to the selected provider.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
}

/// `fanout plan` reads one of two mutually exclusive inputs: an existing
/// persisted plan (`--input`), or the same `--repo` plus issue URLs surface
/// `fanout cook-batch` accepts, planned statically with no side effects.
#[derive(Args, Debug)]
pub struct AgentTaskFanoutPlanArgs {
    /// Existing batch-cook fanout plan to normalize and inspect: inline JSON,
    /// `@FILE`, `-` for stdin, or the `@<path>` controller-owned private plan
    /// artifact. Omit this and pass `--repo` plus issue URLs to plan statically
    /// instead.
    #[arg(
        long = "input",
        value_name = "SPEC",
        required_unless_present_any = ["repo", "issues"],
        conflicts_with_all = ["repo", "issues"]
    )]
    pub input: Option<String>,
    /// Explicit identity for the planned batch. Only used with the `--repo`
    /// issue-planning input; a loaded plan keeps its own identity.
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    /// Executor backend serving every planned child cook. Omit to use the
    /// configured `agent_task.default_backend`.
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
    /// Model name forwarded to the selected provider for every planned child.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
    /// Registered repository slug or exact registered primary checkout path to
    /// plan children for. Required with (and only with) issue URLs.
    #[arg(
        long = "repo",
        value_name = "REPO_SLUG_OR_PRIMARY_PATH",
        requires = "issues",
        conflicts_with = "input"
    )]
    pub repo: Option<String>,
    /// GitHub issue URL to plan one child cook for. Repeat for a wave. Every
    /// child still requires a deterministic gate: pass shared --verify /
    /// --private-verify inputs or --verification-profiles, exactly as
    /// `fanout cook-batch --preview` requires.
    #[arg(value_name = "ISSUE_URL", requires = "repo")]
    pub issues: Vec<String>,
    /// Source ref the planned child worktrees would be created from. When
    /// omitted, this is inferred from the repository default branch.
    #[arg(long = "from", value_name = "REF")]
    pub from: Option<String>,
    /// Pull-request base branch for the planned children. When omitted,
    /// Homeboy resolves the registered repository's remote default branch.
    #[arg(long = "base", value_name = "BRANCH")]
    pub base: Option<String>,
    /// Prefix for generated child branches (default `fix`).
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,
    /// Prompt template rendered for every planned child cook. `{issue_url}`,
    /// `{issue_ref}`, `{repo}`, `{branch}`, and `{worktree}` are substituted.
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,
    /// JSON verification profile declaration, inline or @file.json. Profiles
    /// append to or replace shared --verify/--private-verify gates per issue.
    #[arg(long = "verification-profiles", value_name = "JSON")]
    pub verification_profiles: Option<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Accepted for verb consistency with `agent-task cook --preview` and
    /// `fanout cook-batch --preview`: `fanout plan` is always side-effect
    /// free, so this flag changes nothing. `--dry-run` is accepted as the
    /// historical spelling.
    #[arg(long = "preview", alias = "dry-run")]
    pub preview: bool,
}

impl AgentTaskFanoutPlanArgs {
    /// Project the issue-planning inputs onto the cook-batch surface for
    /// static preview planning. Gate inputs ride along untouched; execution
    /// flags are pinned off because `fanout plan` never executes.
    pub(crate) fn into_cook_batch_preview(self) -> AgentTaskFanoutCookBatchArgs {
        AgentTaskFanoutCookBatchArgs {
            issues: self.issues,
            repo: self.repo.unwrap_or_default(),
            // `fanout plan` does not expose component selection.
            component: None,
            from: self.from,
            base: self.base,
            base_resolution: None,
            branch_prefix: self.branch_prefix,
            fanout_id: self.fanout_id,
            worktrees: Vec::new(),
            prompt_template: self.prompt_template,
            backend: self.backend,
            selector: self.selector,
            model: self.model,
            provider_profile: None,
            secret_env: Vec::new(),
            provider_config: None,
            provider_evidence_inputs: Vec::new(),
            ai_tool: None,
            gates: self.gates,
            verification_profiles: self.verification_profiles,
            max_concurrency: None,
            max_duration: None,
            preview: true,
            dry_run_planner_timeout_seconds: None,
            run_plan: false,
        }
    }
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    /// Durable run ID to assign while submitting the loaded batch-cook plan.
    #[arg(long = "run-id", value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitBatchArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    /// Durable batch ID to assign while submitting the loaded agent-task plan.
    #[arg(long = "batch-id", value_name = "ID")]
    pub batch_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutBatchStatusArgs {
    /// Durable fanout batch ID whose status, resume result, or artifacts to read.
    pub batch_id: String,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutRunPlanArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    /// Durable run id recorded for this execution of the plan.
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
        value_parser = clap::value_parser!(u32).range(1..),
        value_name = "N"
    )]
    pub max_concurrency: Option<u32>,
    /// Wall-clock budget, in seconds, for the whole batch. See
    /// `fanout cook-batch --max-duration`.
    #[arg(
        long = "max-duration",
        value_parser = clap::value_parser!(u64).range(1..),
        value_name = "SECONDS"
    )]
    pub max_duration: Option<u64>,
}
