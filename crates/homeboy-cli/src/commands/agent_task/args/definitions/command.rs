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
use super::cook::{AgentTaskCookArgs, PromotionProviderArgs};
use super::fanout::{AgentTaskBatchPrepareArgs, AgentTaskFanoutArgs};
use super::lifecycle::{
    AdoptArgs, CancelArgs, DiagnoseArgs, EvidenceArgs, FinalizePrArgs, GateFeedbackArgs,
    LifecycleReadArgs, LogsArgs, PlacementUpdateArgs, PromoteArgs, QuarantineArgs, RearmArgs,
    RecoverArgs, ReplayProviderBoundaryArgs, ResumeArgs, RetryArgs, ReviewArgs, RunArgs,
    RunPlanArgs, StatusArgs, ValidatePlanArgs,
};

pub use super::super::auth::{
    AgentTaskAuthCommand, AgentTaskAuthMapEnvArgs, AgentTaskAuthMapKeychainBundleArgs,
    AgentTaskAuthRemoveArgs, AgentTaskAuthSetConfigArgs, AgentTaskAuthSetKeychainArgs,
    AgentTaskAuthSetKeychainBundleArgs, AgentTaskAuthStatusArgs,
};

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

impl AgentTaskCommand {
    /// True only when this invocation admits durable Cook-owned work.
    pub(crate) fn admits_durable_cook_work(&self) -> bool {
        match self {
            Self::Cook(cook) => !cook.preview,
            Self::Fanout(fanout) => match &fanout.command {
                super::fanout::AgentTaskFanoutCommand::CookBatch(batch) => batch.run_plan,
            },
            _ => false,
        }
    }
}

#[derive(Args, Debug)]
pub struct ActiveScopeArgs {
    /// Opaque caller reference whose active checkout ownership should be read.
    #[arg(long)]
    pub context: String,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    #[command(hide = true)]
    ExecutionStep {
        #[arg(long)]
        intent_id: String,
    },
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
    /// By default Cook returns once the controller durably owns the
    /// run. Its submission result reports `accepted` after executable-attempt
    /// materialization or `pending` while a live supervised child is still
    /// preparing. It is honored on every placement: with `--placement local`
    /// the Cook is re-executed in its own session, so it survives a client that
    /// is interrupted or times out.
    ///
    /// Pass global `--wait` to observe until terminal completion and return the
    /// terminal exit status. Placement and durable ownership are independent of
    /// this wait policy. Inspect with `agent-task status <run-id>` or follow with
    /// `runs watch <run-id>`. Preview remains synchronous and executes no work.
    #[command(
        after_help = "Quick start:\n  homeboy agent-task cook --repo REPO --task-url URL --prompt @task.md --verify 'homeboy review test homeboy'\n\nBackend selection: pass --backend explicitly, configure agent_task.default_backend, or use --preview to see the ready backend routes. Preview adds --backend to its replay command only when exactly one ready route is eligible; multiple ready routes require an explicit choice.\n\nNo default configured (agent_task.default_backend unset, e.g. a fresh or reset agent_task: {}): run `homeboy agent-task providers --set-default` to live-probe every declared backend and write a working default_backend/rotation from what actually authenticates here.\n\nInspect inferred inputs without side effects:\n  homeboy agent-task cook --repo REPO --task-url URL --prompt @task.md --verify 'homeboy review test homeboy' --preview\n\nUse --help-full for the complete advanced option reference."
    )]
    Cook(Box<AgentTaskCookArgs>),
    /// Attach candidate-bound review findings to an existing Cook.
    CookFeedback(CookFeedbackArgs),
    /// Run an `AgentTaskPlan` through the built-in Pi provider.
    RunPlan(RunPlanArgs),
    /// Execute a previously submitted durable run.
    Run(RunArgs),
    /// Validate a plan and provider readiness without creating a lifecycle record.
    ValidatePlan(ValidatePlanArgs),
    /// Read durable run status.
    Status(StatusArgs),
    /// List durable runs, newest first.
    ///
    /// Discovery returns a finite agent-facing page by default; use `--limit` for
    /// a different page or `--full` for every matching record. Use `--latest` to
    /// search complete durable history and return the newest record matching the
    /// supplied list filters.
    List(ListArgs),
    /// Read the indexed active checkout ownership for an opaque caller context.
    ActiveScope(ActiveScopeArgs),
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
    /// Read the canonical durable event stream for a run.
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
    /// Validate controller runtime eligibility without executing provider work.
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
    ///
    /// Given a Cook ID whose durable state names a recovery step, `resume`
    /// performs that step through `agent-task recover` instead of deciding
    /// one itself.
    Resume(ResumeArgs),
    /// Change an unexecuted queued Cook's placement after explicit confirmation.
    PlacementUpdate(PlacementUpdateArgs),
    /// Submit a fresh durable run from an existing run's plan.
    ///
    /// A stopped Cook is recovered with `agent-task recover`, which also owns
    /// the Cook's next-attempt provider-route overrides. `retry <cook-id>
    /// --run` performs the Cook's recovery step through `recover`; any other
    /// retry of a Cook ID is refused toward it.
    Retry(RetryArgs),
    /// Perform the one correct next recovery step for a stopped Cook.
    ///
    /// Accepts a Cook ID, attempt ID or batch-child run ID and performs the
    /// step its durable state names: continue, continue with the canonical
    /// patch artifact, rearm a failed continuation, retry, fork a replacement,
    /// or reconcile a held claim. `--preflight` reports the step without
    /// effects. `agent-task status` "Next:" lines name this command.
    Recover(RecoverArgs),
    /// Cook a batch of independent issues as one workflow instance.
    ///
    /// `fanout cook-batch` compiles to a `homeboy/workflow/v1` fan-out: each
    /// child is an ordinary `agent-task cook` that finalizes its own pull
    /// request when its deterministic gates pass.
    Fanout(AgentTaskFanoutArgs),
    /// Plan a cook-batch: resolve its base once, name each child, and prepare
    /// the shared Lab workspace bundle. The `prepare` step of `fanout cook-batch`.
    BatchPrepare(AgentTaskBatchPrepareArgs),
    /// Build a durable aggregate review envelope from run state, logs, artifacts,
    /// and promotion hints.
    Review(ReviewArgs),
    /// Promote a completed generic patch artifact into a managed worktree.
    Promote(Box<PromoteArgs>),
    /// Adopt an immutable commit candidate through a tracked cook's normal gates and finalization.
    Adopt(AdoptArgs),
    #[command(hide = true)]
    PromotionProvider(PromotionProviderArgs),
    /// Internal: one daemon-supervised terminal Cook continuation pass.
    #[command(hide = true)]
    TerminalContinuationWorker {
        #[arg(long)]
        request: String,
        /// File the worker publishes its outcome to before exiting.
        #[arg(long)]
        outcome: std::path::PathBuf,
    },
    /// Internal admitted runner gate execution boundary.
    #[command(hide = true)]
    GateExecute {
        #[arg(long)]
        request: String,
        #[arg(long)]
        resources: Option<String>,
        /// Private terminal artifact location; stdout carries only its digest.
        #[arg(long)]
        receipt_file: Option<String>,
    },
    /// Finalize a green manual-finalization candidate into a pull request.
    ///
    /// This is the core-owned publication boundary for external runtimes. A
    /// stopped Cook publishes through `agent-task recover`.
    FinalizePr(Box<FinalizePrArgs>),
    /// Record an independent, durable acceptance verdict for a candidate.
    Accept(AcceptArgs),
    /// Convert deterministic gate results into a cook retry or stop decision.
    GateFeedback(GateFeedbackArgs),
    /// List Pi provider routes and optional secret/model readiness.
    ///
    /// `--backend X` filters the presentation to X so output stays within caller
    /// display limits; pass `--catalog` for the full multi-backend catalog.
    /// `--set-default` recovers a lost or empty `agent_task` config by writing
    /// a live-verified `default_backend`/`rotation` (#13634).
    Providers(ProvidersArgs),
    /// Report live capacity and reset times for every provider route in the
    /// configured rotation, without running provider inference.
    ///
    /// Walks `agent_task.default_backend` plus every `agent_task.rotation`
    /// entry, probes each route's readiness invocation in capacity-only mode
    /// (`mode: "capacity"`), groups routes that report the same account-pool
    /// `scope`, and returns `homeboy/agent-task-capacity/v1` with one entry per
    /// pool and `next_reset` naming the soonest exhausted-account reset.
    /// Capacity support is a runtime opt-in declared by the runtime
    /// extension; a route whose runtime publishes no capacity is
    /// reported as unknown with its diagnostic, never probed with inference.
    Capacity(AgentTaskCapacityArgs),
    /// Manage markdown prompts in Homeboy-owned storage.
    ///
    /// Prompts are stored under Homeboy's data directory, not the current
    /// repo/worktree, and are referenced as `prompt:<id>` wherever a prompt
    /// string is accepted.
    Prompts(AgentTaskPromptsArgs),
    /// Export Homeboy's machine-readable agent-task core contract metadata.
    Contract(ContractArgs),
    /// Configure and inspect provider authentication secrets.
    Auth(AgentTaskAuthArgs),
    #[command(hide = true)]
    Tool(AgentTaskToolArgs),
}

#[derive(Args, Debug)]
pub struct AcceptArgs {
    /// Durable run ID whose candidate receives the verdict.
    pub run_id: String,
    /// Acceptance decision: `accepted` permits finalization; `rejected` records a failure.
    #[arg(long, value_parser = ["accepted", "rejected"])]
    pub verdict: String,
    /// Opaque credential consumed by the configured acceptance verifier.
    #[arg(long)]
    pub token: String,
    /// Durable evidence reference supporting the verdict. Repeatable.
    #[arg(long = "evidence-ref")]
    pub evidence_refs: Vec<String>,
    /// Bounded reviewer remediation feedback retained with a rejected Cook
    /// candidate for its one authorized repair attempt.
    #[arg(long, value_name = "TEXT")]
    pub feedback: Option<String>,
}

#[derive(Args, Debug)]
pub struct CookFeedbackArgs {
    /// Durable Cook ID receiving the review.
    pub cook_id: String,
    /// Reviewed commit, artifact, or other durable candidate identity (defaults to the Cook's unique selected candidate).
    #[arg(long, value_name = "IDENTITY")]
    pub candidate: Option<String>,
    /// Inline Markdown feedback.
    #[arg(long, conflicts_with_all = ["file", "stdin"])]
    pub text: Option<String>,
    /// Markdown file containing feedback.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["text", "stdin"])]
    pub file: Option<String>,
    /// Read Markdown feedback from stdin.
    #[arg(long, conflicts_with_all = ["text", "file"])]
    pub stdin: bool,
    /// Reviewer identity retained with the finding.
    #[arg(long, default_value = "operator")]
    pub author: String,
    /// Source label retained with the finding.
    #[arg(long, default_value = "review")]
    pub source: String,
    /// Stable retry key. Defaults to a deterministic key derived from the candidate and normalized feedback.
    #[arg(long)]
    pub idempotency_key: Option<String>,
    /// Read the durable feedback ledger instead of submitting a finding.
    #[arg(long)]
    pub status: bool,
}

#[cfg(test)]
mod command_parse_tests {
    use clap::Parser;

    use crate::{
        cli_surface::{Cli, Commands},
        commands::agent_task::AgentTaskCommand,
    };

    #[test]
    fn execution_step_parses_only_the_exact_intent_id_and_is_hidden() {
        use clap::CommandFactory;
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "execution-step",
            "--intent-id",
            "agent-task-exec:run-a:1:0",
        ])
        .unwrap();
        let Commands::AgentTask(args) = cli.command else {
            panic!("agent-task command")
        };
        let AgentTaskCommand::ExecutionStep { intent_id } = args.command else {
            panic!("execution-step command")
        };
        assert_eq!(intent_id, "agent-task-exec:run-a:1:0");
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("agent-task")
            .unwrap()
            .render_help()
            .to_string();
        assert!(!help.contains("execution-step"));
        assert!(Cli::try_parse_from(["homeboy", "agent-task", "execution-step"]).is_err());
    }

    #[test]
    fn feedback_accepts_inline_file_or_stdin_contract_and_status() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook-feedback",
            "cook-1",
            "--candidate",
            "candidate-a",
            "--text",
            "fix this",
            "--idempotency-key",
            "review-1",
        ])
        .expect("inline feedback parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::CookFeedback(args) = agent_task.command else {
            panic!("expected cook-feedback command");
        };
        assert_eq!(args.candidate.as_deref(), Some("candidate-a"));
        assert_eq!(args.text.as_deref(), Some("fix this"));
        assert!(!args.status);
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook-feedback",
            "cook-1",
            "--candidate",
            "candidate-a",
            "--status",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook-feedback",
            "cook-1",
            "--candidate",
            "candidate-a",
            "--text",
            "one",
            "--stdin",
        ])
        .is_err());
    }
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Maximum matching durable runs to return (effective maximum: 100; output
    /// reports both the requested and effective limits).
    #[arg(
        long = "limit",
        value_name = "N",
        value_parser = positive_discovery_limit,
        conflicts_with = "full"
    )]
    pub limit: Option<usize>,
    /// Opaque continuation from the preceding page. Reuse every filter from the prior page.
    #[arg(long, value_name = "CURSOR", conflicts_with = "full")]
    pub cursor: Option<String>,
    /// Restrict results to this repository identity.
    #[arg(long)]
    pub repo: Option<String>,
    /// Restrict results to this workspace handle or path.
    #[arg(long)]
    pub worktree: Option<String>,
    /// Restrict results to this task URL.
    #[arg(long = "task-url")]
    pub task_url: Option<String>,
    /// RFC3339 submission timestamp; excludes older records.
    #[arg(long = "submitted-after", value_name = "RFC3339")]
    pub submitted_after: Option<String>,
    /// Restrict results to this durable lifecycle state.
    #[arg(long, value_parser = ["queued", "running", "succeeded", "failed", "cancelled"])]
    pub state: Option<String>,
    /// Filter by recorded execution placement, not the global routing policy.
    #[arg(long = "run-placement", value_parser = ["local", "remote", "runner"])]
    pub run_placement: Option<String>,
    /// Restrict results to records owned by this parent run or group.
    #[arg(long = "parent-id")]
    pub parent_id: Option<String>,
    /// Restrict results to the recorded worktree branch.
    #[arg(long)]
    pub branch: Option<String>,
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
    /// Cap active discovery to a positive page size (effective maximum: 100;
    /// output reports both the requested and effective limits). Cannot be
    /// combined with `--full` or fleet-wide `--reconcile`.
    #[arg(
        long = "limit",
        value_name = "N",
        value_parser = positive_discovery_limit,
        conflicts_with_all = ["full", "reconcile"]
    )]
    pub limit: Option<usize>,
    /// Opaque continuation from the prior active page. Cannot be
    /// combined with `--full` or fleet-wide `--reconcile`.
    #[arg(long, value_name = "CURSOR", conflicts_with_all = ["full", "reconcile"])]
    pub cursor: Option<String>,
    /// Restrict active runs to the recorded worktree branch.
    #[arg(long, conflicts_with = "reconcile")]
    pub branch: Option<String>,
    /// Return every matching record. This is intentionally explicit because
    /// discovery defaults to a finite agent-facing page and cannot scope
    /// fleet-wide `--reconcile`.
    #[arg(long, conflicts_with = "reconcile")]
    pub full: bool,
    /// Preview reconciliation for every active record in the selected scope.
    #[arg(long = "reconcile")]
    pub reconcile: bool,
    /// Explicitly retain preview-only reconciliation mode.
    #[arg(long = "dry-run", requires = "reconcile", conflicts_with = "apply")]
    pub dry_run: bool,
    /// Apply reconciliation to every selected active record.
    #[arg(long = "apply", requires = "reconcile", conflicts_with = "dry_run")]
    pub apply: bool,
}
#[derive(Args, Debug)]
pub struct ReconcileArgs {
    /// Durable run ID or Cook group ID to reconcile.
    pub run_id: String,
    /// Preview the selected durable run/group without persisted mutation. This
    /// is the default when `--apply` is omitted.
    #[arg(long = "dry-run", conflicts_with = "apply")]
    pub dry_run: bool,
    /// Apply the reviewed reconciliation to the selected durable run/group.
    #[arg(long = "apply", conflicts_with = "dry_run")]
    pub apply: bool,
    /// Stable caller key for safely replaying an applied reconciliation.
    #[arg(long, value_name = "KEY", requires = "apply")]
    pub idempotency_key: Option<String>,
}
impl From<ListArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ListArgs) -> Self {
        Self {
            limit: (!args.full).then(|| args.limit.unwrap_or(DEFAULT_DISCOVERY_LIMIT)),
            cursor: 0,
            repo: args.repo,
            workspace: args.worktree,
            task_url: args.task_url,
            submitted_after: args.submitted_after,
            state: args.state,
            placement: args.run_placement,
            parent_id: args.parent_id,
            branch: args.branch,
        }
    }
}
impl From<ActiveArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ActiveArgs) -> Self {
        Self {
            limit: (!args.full).then(|| args.limit.unwrap_or(DEFAULT_DISCOVERY_LIMIT)),
            cursor: 0,
            branch: args.branch,
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
pub struct RetainedArtifactsArgs {
    #[command(subcommand)]
    pub command: RetainedArtifactsCommand,
}

#[derive(Subcommand, Debug)]
pub enum RetainedArtifactsCommand {
    /// Resolve the retained workspace and print bounded, run-ID-only attach guidance.
    Discover {
        /// Terminal Lab Cook run ID whose retained workspace is discovered.
        run_id: String,
    },
    /// Attach one repository-relative file or directory from the retained workspace.
    Attach {
        /// Terminal Lab Cook run ID that owns the retained workspace.
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
    /// Restrict results to this executor backend.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    /// Restrict results to this backend-specific provider selector.
    #[arg(long = "selector", value_name = "PROVIDER_ID")]
    pub selector: Option<String>,
    /// Validate or report this exact model selection using Cook's provider route.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
    /// Restrict results to `default`, `available`, or `unavailable` providers.
    ///
    /// `unavailable` means declared but not dispatchable — a provider whose
    /// declared credentials do not resolve here. Its `reason` names the missing
    /// credential (#11479).
    #[arg(long = "status", value_name = "STATUS")]
    pub status: Option<String>,
    /// Declare a secret environment variable available for readiness checks. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    /// Live-probe matching providers and report their readiness.
    #[arg(long = "validate-readiness")]
    pub validate_readiness: bool,
    /// Refresh cached provider discovery before listing results.
    #[arg(long = "refresh")]
    pub refresh: bool,
    /// Return the full multi-backend catalog even when `--backend` is set.
    /// Without this, `--backend X` filters the presentation to X so the output
    /// stays within caller display limits (#9654).
    #[arg(long = "catalog", visible_alias = "all")]
    pub catalog: bool,
    /// Return the complete provider declarations.
    #[arg(long)]
    pub full: bool,
    /// Emit the unfiltered provider DTO catalog for machine admission clients.
    #[arg(long, hide = true)]
    pub machine_catalog: bool,
    /// Live-probe every declared backend and, if at least one is ready right
    /// now, persist it as `agent_task.default_backend` with the remaining
    /// ready backends recorded as an `agent_task.rotation` fallback chain.
    ///
    /// Recovers a lost or empty `agent_task` config (`{}`) — which otherwise
    /// silently disables `agent-task cook` with no default and no documented
    /// shape to restore one — without reading source or an old backup
    /// (#13634). Implies `--validate-readiness`. Writes nothing and fails if
    /// no declared backend is currently ready; run `agent-task providers
    /// --validate-readiness` to see why each one failed.
    ///
    /// Written `agent_task.rotation` shape:
    /// `{"entries": [{"backend": "NAME"}, ...], "max_attempts": N,
    /// "liveness_timeout_ms": N}` — each entry may also set `selector`,
    /// `model`, and `provider_config`, and both `max_attempts` and
    /// `liveness_timeout_ms` are optional. Edit it directly with `homeboy
    /// config set /agent_task/rotation <json> --json`.
    #[arg(long = "set-default")]
    pub set_default: bool,
}
#[derive(Args, Debug)]
pub struct AgentTaskCapacityArgs {
    /// Narrow the rotation walk to this executor backend.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    /// Narrow the rotation walk to this backend-specific provider selector.
    #[arg(long = "selector", value_name = "PROVIDER_ID")]
    pub selector: Option<String>,
    /// Narrow the rotation walk to this model selection.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
}
#[derive(Args, Debug)]
pub struct AgentTaskDoctorArgs {
    /// Runner to diagnose and optionally repair.
    #[arg(long, value_name = "RUNNER")]
    pub runner: String,
    /// Restrict diagnostics to this executor backend.
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,
    /// Restrict diagnostics to this backend-specific provider selector.
    #[arg(long, value_name = "PROVIDER_ID")]
    pub selector: Option<String>,
    /// Add this directory to the runner diagnostic path search.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
    /// Require this extension to be available on the runner. Repeatable.
    #[arg(long = "extension", value_name = "EXTENSION")]
    pub extensions: Vec<String>,
    /// Require this executable to be available on the runner. Repeatable.
    #[arg(long = "require-tool", value_name = "TOOL")]
    pub required_tools: Vec<String>,
    /// Declare a secret environment variable available for readiness checks. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    /// Repair remediable runner readiness failures.
    #[arg(long)]
    pub repair: bool,
}
#[derive(Args, Debug)]
pub struct ContractArgs {
    /// Serialization format for the exported contract metadata.
    #[arg(long, default_value = "json", value_enum)]
    pub format: ContractFormat,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ContractFormat {
    Json,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn cook_help_recommends_review_test() {
        let help = crate::cli_surface::Cli::command()
            .find_subcommand("agent-task")
            .expect("agent-task command")
            .find_subcommand("cook")
            .expect("cook command")
            .clone()
            .render_long_help()
            .to_string();

        assert!(
            help.contains("--verify 'homeboy review test homeboy'"),
            "{help}"
        );
        assert!(!help.contains("--verify 'cargo test'"), "{help}");
    }
}
