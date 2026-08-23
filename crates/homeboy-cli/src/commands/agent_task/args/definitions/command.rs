use clap::{Args, Subcommand, ValueEnum};
use homeboy::agents::agent_task_service::AgentTaskDiscoveryOptions;

/// Agent-facing discovery defaults to a finite page. A caller can request a
/// different page with `--limit`; full durable records remain available through
/// focused status/artifact commands and `--output` artifacts.
const DEFAULT_DISCOVERY_LIMIT: usize = 20;

fn positive_discovery_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value.parse::<usize>().map_err(|error| error.to_string())?;
    if limit == 0 {
        return Err("must be greater than zero".to_string());
    }
    Ok(limit)
}

use super::super::super::prompts::AgentTaskPromptsArgs;
use super::super::super::tool::AgentTaskToolArgs;
use super::cook::{AgentTaskCookArgs, AgentTaskLoopArgs, PromotionProviderArgs};
use super::fanout::AgentTaskFanoutArgs;
use super::lifecycle::{
    AdoptArgs, CancelArgs, DiagnoseArgs, EvidenceArgs, FinalizePrArgs, GateFeedbackArgs,
    LifecycleReadArgs, LogsArgs, PromoteArgs, QuarantineArgs, RearmArgs,
    RecordReplacementGateProofArgs, ReplayProviderBoundaryArgs, RetryArgs, ReviewArgs, RunArgs,
    RunNextArgs, RunPlanArgs, RuntimeRecoverArgs, RuntimeValidateArgs, StatusArgs, SubmitArgs,
    ValidatePlanArgs, VerifyReplacementArgs,
};

pub use super::super::auth::{
    AgentTaskAuthCommand, AgentTaskAuthMapEnvArgs, AgentTaskAuthMapKeychainBundleArgs,
    AgentTaskAuthRemoveArgs, AgentTaskAuthSetConfigArgs, AgentTaskAuthSetKeychainArgs,
    AgentTaskAuthSetKeychainBundleArgs, AgentTaskAuthStatusArgs,
};
pub use super::super::controller::{
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
    /// Diagnose provider and runtime readiness on a runner, and optionally repair it.
    Doctor(AgentTaskDoctorArgs),
    /// Submit an agent task, run its gates, and open a pull request.
    ///
    /// Provide the work with one `--prompt` and optional `--goal` framing, point
    /// `--to-worktree` at the existing worktree to edit (that checkout is
    /// authoritative — the agent's changes, the `--verify` gates, and the PR all
    /// operate on it), and give one or more `--verify` commands that must pass in
    /// that worktree before promotion. Cook then commits, runs the deterministic
    /// gates, and finalizes a `--base`-targeted PR (use `--no-finalize` to stop
    /// before opening the PR). Repeatable `--verify` gates all run; the run
    /// retries up to `--max-attempts` times. Use `agent-task fanout cook-batch`
    /// for independent task waves.
    ///
    /// WAIT POLICY: Cook always persists a durable run id before materialization,
    /// so a returned command is not by itself proof of a completed cook.
    ///
    /// By default Cook observes until the lifecycle is terminal and returns the
    /// terminal Cook report.
    ///
    /// `--detach-after-handoff` returns once the run is durably accepted. Its
    /// result describes a submission, not an outcome. It is honored on every
    /// placement: with `--placement local` the Cook is re-executed in its own
    /// session, so it survives a client that is interrupted or times out.
    ///
    /// Do not infer the wait policy from client interactivity. An orchestration
    /// client that needs the detached contract should pass
    /// `--detach-after-handoff` rather than rely on the default, and read the
    /// terminal outcome from `agent-task status <run-id>` in either case.
    #[command(
        after_help = "Quick start:\n  homeboy agent-task cook --repo REPO --task-url URL --prompt @task.md --verify 'cargo test'\n\nBackend selection: pass --backend explicitly, configure agent_task.default_backend, or use --preview to see the ready backend routes. Preview adds --backend to its replay command only when exactly one ready route is eligible; multiple ready routes require an explicit choice.\n\nInspect inferred inputs without side effects:\n  homeboy agent-task cook --repo REPO --task-url URL --prompt @task.md --verify 'cargo test' --preview\n\nUse --help-full for the complete advanced option reference."
    )]
    Cook(Box<AgentTaskCookArgs>),
    /// Continue a detached Cook from its durable Cook ID or provider attempt ID.
    /// The persisted recipe supplies the original prompt, transport, gates,
    /// worktree, and disclosure policy.
    CookContinue(CookContinueArgs),
    /// Operate durable defined multi-agent loops: define, inspect, resume, and stop.
    ///
    /// A loop is not a one-shot PR cook. It persists controller state, tracks
    /// whether it is on or off, counts revolutions, and records its continuation
    /// policy. Use `agent-task cook` for single-PR work.
    Loop(AgentTaskLoopArgs),
    /// Run an `AgentTaskPlan` through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// Execute a previously submitted durable run.
    Run(RunArgs),
    /// Claim and execute the oldest queued durable run, optionally within one fanout.
    RunNext(RunNextArgs),
    /// Persist an agent-task plan and return a durable run id without executing it.
    Submit(SubmitArgs),
    /// Validate a plan and provider readiness without creating a lifecycle record.
    ValidatePlan(ValidatePlanArgs),
    /// Read durable run status.
    Status(StatusArgs),
    /// Poll a run until it reaches a terminal state.
    ///
    /// This is an alias for `homeboy activity watch` — the same command the
    /// cook completion notification already points at — so a cook id, durable
    /// run id, observation run id, or runner job id all resolve here, including
    /// records still resident on a Lab runner. Unlike `agent-task status`, the
    /// underlying read does not reconcile.
    Watch(crate::commands::activity::ActivityWatchArgs),
    /// List durable runs, newest first.
    ///
    /// Discovery returns a finite agent-facing page by default; use `--limit` for
    /// a different page or `--full` for every matching record. Use `--latest` to
    /// search complete durable history and return the newest record matching the
    /// supplied list filters.
    List(ListArgs),
    /// List queued and running durable runs, newest first.
    ///
    /// `--reconcile` turns this into an explicit fleet operation: it previews
    /// every candidate by default and requires `--apply` to mutate the set.
    Active(ActiveArgs),
    /// Reconcile one durable agent-task run or explicit Cook group. This is a
    /// preview by default; use `--apply` only after reviewing the authoritative
    /// provider state. Success leaves every selected durable record reconciled
    /// to that state.
    Reconcile(ReconcileArgs),
    /// Reconcile stored durable run records against authoritative provider state.
    ReconcileRecords(ReconcileRecordsArgs),
    /// Show the latest durable run.
    Latest(LatestArgs),
    /// Read the canonical durable event stream for a run.
    ///
    /// `--raw` additionally emits transport frames for diagnostics.
    Logs(LogsArgs),
    /// List artifacts and evidence refs recorded for a completed run.
    Artifacts(LifecycleReadArgs),
    /// Discover or attach selected outputs retained in a terminal Lab Cook workspace.
    RetainedArtifacts(RetainedArtifactsArgs),
    /// Retrieve selected durable evidence recorded for a run.
    ///
    /// Narrow the result with `--task` or `--kind`; `--full` returns the
    /// unprojected evidence.
    Evidence(EvidenceArgs),
    /// Compute a root cause, causal chain, and next actions for a failed run.
    ///
    /// Next actions are derived from the failure classification, not from prose.
    Diagnose(DiagnoseArgs),
    /// Recover a missing or corrupted immutable controller runtime pin.
    RuntimeRecover(RuntimeRecoverArgs),
    /// Validate controller runtime eligibility without executing provider work.
    RuntimeValidate(RuntimeValidateArgs),
    /// Hydrate the latest raw executor input and print provider-boundary fields
    /// without relaunching a provider.
    ///
    /// Persists the inspection as `provider-boundary-replay` evidence. Use
    /// `--task <task-id>` for multi-task runs.
    ReplayProviderBoundary(ReplayProviderBoundaryArgs),
    /// Mark a queued or stale-running durable run as cancelled.
    Cancel(CancelArgs),
    /// Exclude one exact queued record while preserving its lifecycle and evidence.
    Quarantine(QuarantineArgs),
    /// Return one exact quarantined queued record to normal queue eligibility.
    Rearm(RearmArgs),
    /// Resume a queued or stale-running durable run.
    Resume(LifecycleReadArgs),
    /// Submit a fresh durable run from an existing run's plan.
    Retry(RetryArgs),
    /// Cook, submit, and inspect batches of independent tasks.
    ///
    /// Each child declares its own target worktree and optional head branch, runs
    /// through the same cook-loop path as a single PR cook, and finalizes its own
    /// pull request when its deterministic gates pass.
    Fanout(AgentTaskFanoutArgs),
    /// Build a durable aggregate review envelope from run state, logs, artifacts,
    /// and promotion hints.
    Review(ReviewArgs),
    /// Promote a completed generic patch artifact into a managed worktree.
    Promote(Box<PromoteArgs>),
    /// Adopt an immutable commit candidate through a tracked cook's normal gates and finalization.
    Adopt(AdoptArgs),
    #[command(hide = true)]
    PromotionProvider(PromotionProviderArgs),
    /// Finalize a green run, or recover publication from a durable Cook record.
    ///
    /// This is the core-owned publication boundary for external runtimes.
    FinalizePr(Box<FinalizePrArgs>),
    /// Attach authorized candidate-bound replacement gate proof after an infrastructure gate failure.
    RecordReplacementGateProof(RecordReplacementGateProofArgs),
    /// Run corrected gates against an already-applied failed candidate and record replacement proof.
    VerifyReplacement(VerifyReplacementArgs),
    /// Record an independent, durable acceptance verdict for a candidate.
    Accept(AcceptArgs),
    /// Convert deterministic gate results into a cook retry or stop decision.
    GateFeedback(GateFeedbackArgs),
    /// List extension-declared executor providers and optional secret/backend readiness.
    ///
    /// `--backend X` filters the presentation to X so output stays within caller
    /// display limits; pass `--catalog` for the full multi-backend catalog.
    Providers(ProvidersArgs),
    /// Manage markdown prompts in Homeboy-owned storage.
    ///
    /// Prompts are stored under Homeboy's data directory, not the current
    /// repo/worktree, and are referenced as `prompt:<id>` wherever a prompt
    /// string is accepted.
    Prompts(AgentTaskPromptsArgs),
    /// Export Homeboy's machine-readable agent-task core contract metadata.
    Contract(ContractArgs),
    /// Compile a declarative loop definition into an agent-task plan without
    /// submitting or running it.
    CompileLoop(CompileLoopArgs),
    /// Configure and inspect provider authentication secrets.
    Auth(AgentTaskAuthArgs),
    /// Create, inspect, and resume durable multi-agent loop controller state.
    Controller(AgentTaskControllerArgs),
    #[command(hide = true)]
    Tool(AgentTaskToolArgs),
}

#[derive(Args, Debug)]
pub struct AcceptArgs {
    pub run_id: String,
    #[arg(long, value_parser = ["accepted", "rejected"])]
    pub verdict: String,
    /// Opaque credential consumed by the configured acceptance verifier.
    #[arg(long)]
    pub token: String,
    #[arg(long = "evidence-ref")]
    pub evidence_refs: Vec<String>,
    /// Bounded reviewer remediation feedback retained with a rejected Cook
    /// candidate for its one authorized repair attempt.
    #[arg(long, value_name = "TEXT")]
    pub feedback: Option<String>,
}

#[derive(Args, Debug)]
pub struct CookContinueArgs {
    /// Durable Cook ID or one of its provider attempt IDs.
    pub cook_or_attempt_id: String,
    /// Validate continuation admission without dispatching a provider or mutating lifecycle state.
    #[arg(long)]
    pub preflight: bool,
    /// Explicitly rearm one failed terminal continuation before consuming it.
    #[arg(long, conflicts_with = "preflight")]
    pub rearm: bool,
    /// Select the patch artifact to promote when the durable attempt produced
    /// more than one patch candidate. This resumes controller-side promotion
    /// without dispatching another provider execution.
    #[arg(long = "artifact-id", value_name = "ID")]
    pub artifact_id: Option<String>,
    /// Include the complete Cook report rather than the compact lifecycle view.
    #[arg(long)]
    pub full: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    #[arg(long = "limit", value_name = "N", conflicts_with = "full")]
    pub limit: Option<usize>,
    /// Continue at this zero-based offset. Reuse every filter from the prior page.
    #[arg(long, value_name = "N", conflicts_with = "full")]
    pub cursor: Option<usize>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub worktree: Option<String>,
    #[arg(long = "task-url")]
    pub task_url: Option<String>,
    /// RFC3339 submission timestamp; excludes older records.
    #[arg(long = "submitted-after", value_name = "RFC3339")]
    pub submitted_after: Option<String>,
    #[arg(long, value_parser = ["queued", "running", "succeeded", "failed", "cancelled"])]
    pub state: Option<String>,
    /// Filter by recorded execution placement, not the global routing policy.
    #[arg(long = "run-placement", value_parser = ["local", "remote", "runner"])]
    pub run_placement: Option<String>,
    #[arg(long = "parent-id")]
    pub parent_id: Option<String>,
    /// Return every matching record. This is intentionally explicit because
    /// discovery defaults to a finite agent-facing page.
    #[arg(long)]
    pub full: bool,
    /// Return only the newest record matching the supplied list filters.
    #[arg(long, conflicts_with_all = ["limit", "cursor", "full"])]
    pub latest: bool,
}
#[derive(Args, Debug)]
pub struct ActiveArgs {
    /// Cap active discovery to a positive page size. Cannot be combined with
    /// `--full` or fleet-wide `--reconcile`.
    #[arg(
        long = "limit",
        value_name = "N",
        value_parser = positive_discovery_limit,
        conflicts_with_all = ["full", "reconcile"]
    )]
    pub limit: Option<usize>,
    /// Continue at this zero-based offset from the prior active page. Cannot be
    /// combined with `--full` or fleet-wide `--reconcile`.
    #[arg(long, value_name = "N", conflicts_with_all = ["full", "reconcile"])]
    pub cursor: Option<usize>,
    /// Return every matching record. This is intentionally explicit because
    /// discovery defaults to a finite agent-facing page and cannot scope
    /// fleet-wide `--reconcile`.
    #[arg(long, conflicts_with = "reconcile")]
    pub full: bool,
    #[arg(long = "reconcile")]
    pub reconcile: bool,
    #[arg(long = "dry-run", requires = "reconcile", conflicts_with = "apply")]
    pub dry_run: bool,
    #[arg(long = "apply", requires = "reconcile", conflicts_with = "dry_run")]
    pub apply: bool,
}
#[derive(Args, Debug)]
pub struct ReconcileArgs {
    pub run_id: String,
    /// Preview the selected durable run/group without persisted mutation. This
    /// is the default when `--apply` is omitted.
    #[arg(long = "dry-run", conflicts_with = "apply")]
    pub dry_run: bool,
    /// Apply the reviewed reconciliation to the selected durable run/group.
    #[arg(long = "apply", conflicts_with = "dry_run")]
    pub apply: bool,
}
#[derive(Args, Debug)]
pub struct ReconcileRecordsArgs {
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}
#[derive(Args, Debug)]
pub struct LatestArgs {
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}
impl From<ListArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ListArgs) -> Self {
        Self {
            limit: (!args.full).then(|| args.limit.unwrap_or(DEFAULT_DISCOVERY_LIMIT)),
            cursor: args.cursor.unwrap_or_default(),
            repo: args.repo,
            workspace: args.worktree,
            task_url: args.task_url,
            submitted_after: args.submitted_after,
            state: args.state,
            placement: args.run_placement,
            parent_id: args.parent_id,
        }
    }
}
impl From<ActiveArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ActiveArgs) -> Self {
        Self {
            limit: (!args.full).then(|| args.limit.unwrap_or(DEFAULT_DISCOVERY_LIMIT)),
            cursor: args.cursor.unwrap_or_default(),
            ..Default::default()
        }
    }
}
impl From<LatestArgs> for AgentTaskDiscoveryOptions {
    fn from(args: LatestArgs) -> Self {
        Self {
            limit: Some(args.limit.unwrap_or(DEFAULT_DISCOVERY_LIMIT)),
            ..Default::default()
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
pub struct RetainedArtifactsArgs {
    #[command(subcommand)]
    pub command: RetainedArtifactsCommand,
}

#[derive(Subcommand, Debug)]
pub enum RetainedArtifactsCommand {
    /// Resolve the retained workspace and print bounded, run-ID-only attach guidance.
    Discover { run_id: String },
    /// Attach one repository-relative file or directory from the retained workspace.
    Attach {
        run_id: String,
        /// Repository-relative path below the retained workspace.
        #[arg(long)]
        path: String,
        /// Durable artifact name to record on the owning run.
        #[arg(long)]
        name: String,
    },
}
#[derive(Args, Debug)]
pub struct ProvidersArgs {
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    /// Restrict results to the runtime that owns the provider.
    #[arg(long = "runtime", value_name = "RUNTIME")]
    pub runtime: Option<String>,
    /// Restrict results to `default`, `available`, or `unavailable` providers.
    ///
    /// `unavailable` means declared but not dispatchable — a provider whose
    /// declared credentials do not resolve here. Its `reason` names the missing
    /// credential (#11479).
    #[arg(long = "status", value_name = "STATUS")]
    pub status: Option<String>,
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    #[arg(long = "validate-readiness")]
    pub validate_readiness: bool,
    #[arg(long = "refresh")]
    pub refresh: bool,
    /// Return the full multi-backend catalog even when `--backend` is set.
    /// Without this, `--backend X` filters the presentation to X so the output
    /// stays within caller display limits (#9654).
    #[arg(long = "catalog", visible_alias = "all")]
    pub catalog: bool,
    /// Return the complete provider declarations and discovery diagnostics.
    #[arg(long)]
    pub full: bool,
    /// Emit the unfiltered provider DTO catalog for machine admission clients.
    #[arg(long, hide = true)]
    pub machine_catalog: bool,
}
#[derive(Args, Debug)]
pub struct AgentTaskDoctorArgs {
    #[arg(long, value_name = "RUNNER")]
    pub runner: String,
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(long, visible_alias = "provider-id", value_name = "PROVIDER_ID")]
    pub selector: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
    #[arg(long = "extension", value_name = "EXTENSION")]
    pub extensions: Vec<String>,
    #[arg(long = "require-tool", value_name = "TOOL")]
    pub required_tools: Vec<String>,
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    #[arg(long)]
    pub repair: bool,
}
#[derive(Args, Debug)]
pub struct ContractArgs {
    #[arg(long, default_value = "json", value_enum)]
    pub format: ContractFormat,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ContractFormat {
    Json,
}
#[derive(Args, Debug)]
pub struct CompileLoopArgs {
    #[arg(long, value_name = "SPEC")]
    pub definition: String,
}
