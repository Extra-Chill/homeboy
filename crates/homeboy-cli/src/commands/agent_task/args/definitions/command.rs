use super::super::super::prompts::AgentTaskPromptsArgs;
use super::super::super::tool::AgentTaskToolArgs;
use super::cook::{AgentTaskCookArgs, PromotionProviderArgs};
use super::fanout::{AgentTaskBatchPrepareArgs, AgentTaskFanoutArgs};
use super::lifecycle::{
    AdoptArgs, CancelArgs, DiagnoseArgs, FinalizePrArgs, GateFeedbackArgs, PlacementUpdateArgs,
    PromoteArgs, QuarantineArgs, RearmArgs, RecoverArgs, ReplayProviderBoundaryArgs, ResumeArgs,
    RetryArgs, ReviewArgs, RunArgs, RunPlanArgs, StatusArgs, ValidatePlanArgs,
};
use clap::{Args, Subcommand, ValueEnum};

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
    /// Read the indexed active checkout ownership for an opaque caller context.
    ActiveScope(ActiveScopeArgs),
    /// Reconcile one durable agent-task run or explicit Cook group, or the whole
    /// active fleet with `--all`. This is a preview by default; use `--apply`
    /// only after reviewing the authoritative provider state. Success leaves
    /// every selected durable record reconciled to that state.
    Reconcile(ReconcileArgs),
    /// Discover or attach selected outputs retained in a terminal Lab Cook workspace.
    RetainedArtifacts(RetainedArtifactsArgs),
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
    /// Run a public gate from a Cook's recorded gate policy at the current tree.
    Gate {
        /// Cook recipe ID.
        cook_id: String,
        /// Public gate ID (gate-1, gate-2, ...).
        id: String,
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
pub struct ReconcileArgs {
    /// Durable run ID or Cook group ID to reconcile.
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub run_id: Option<String>,
    /// Reconcile the entire active fleet (preview unless --apply).
    #[arg(long, conflicts_with_all = ["run_id", "idempotency_key"])]
    pub all: bool,
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
