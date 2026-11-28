use clap::Args;

use super::super::super::review;
use super::cook::VerifyGateArgs;

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// Agent-task plan as a JSON spec: inline JSON, `@FILE` to read a file, or
    /// `-` to read stdin. A bare path is NOT accepted — use `@/path/plan.json`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub plan: String,
    /// Durable run ID to record for this planned lifecycle.
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
    /// Maximum execution time in milliseconds.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Exact durable run id to execute. Use this to bypass older queued work.
    pub run_id: String,
    /// Maximum execution time in milliseconds.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}
#[derive(Args, Debug)]
pub struct ValidatePlanArgs {
    /// Agent-task plan as inline JSON, `@FILE`, or `-`. Validation creates no lifecycle record.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub plan: String,
}
#[derive(Args, Debug)]
pub struct PlacementUpdateArgs {
    /// Durable queued Cook ID whose unexecuted placement will be changed.
    pub run_id: String,
    /// Record the operator's confirmation of this execution-route change.
    #[arg(long)]
    pub confirm: bool,
    /// Stable key used to replay this placement update without changing it twice.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}

#[cfg_attr(test, derive(Default))]
#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Durable run or Cook ID whose status to inspect.
    pub run_id: String,
    /// Include the complete lifecycle and eligibility report.
    #[arg(long)]
    pub full: bool,
    /// Inspect this exact lifecycle record instead of resolving a Cook ID to its current attempt.
    #[arg(long)]
    pub exact: bool,
    /// Exit nonzero when the inspected Cook needs follow-up action.
    ///
    /// Normal status reads report their own success independently from the
    /// subject lifecycle state; this flag lets scripts gate on an actionable Cook.
    #[arg(long)]
    pub strict_subject_exit: bool,
    /// Follow this durable status until it reaches a terminal state or the timeout expires.
    #[arg(long)]
    pub watch: bool,
    /// Delay between status reads while following. Accepts ms, s, m, h, or d.
    #[arg(
        long,
        default_value = "5s",
        value_name = "DURATION",
        requires = "watch"
    )]
    pub interval: String,
    /// Total time to follow before returning the latest partial status. Accepts ms, s, m, h, or d.
    #[arg(
        long,
        default_value = "30m",
        value_name = "DURATION",
        requires = "watch"
    )]
    pub timeout: String,
}

#[derive(Args, Debug)]
pub struct DiagnoseArgs {
    /// Durable run or Cook ID to diagnose.
    pub run_id: String,
    /// Hydrate every available evidence summary rather than the bounded preview.
    #[arg(long)]
    pub full: bool,
}
#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::{
        cli_surface::{Cli, Commands},
        commands::agent_task::AgentTaskCommand,
    };

    #[test]
    fn artifacts_can_show_a_selected_artifact_and_tail_its_lines() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "runs",
            "artifacts",
            "run-a",
            "--show",
            "transcript",
            "--tail",
            "12",
        ])
        .expect("artifact show and tail parse");
        assert!(matches!(cli.command, Commands::Runs(_)));
        assert!(
            Cli::try_parse_from(["homeboy", "runs", "artifacts", "run-a", "--tail", "12"]).is_err()
        );
    }

    #[test]
    fn status_watch_uses_bounded_duration_flags() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--watch",
            "--interval",
            "250ms",
            "--timeout",
            "2m",
        ])
        .expect("watch status parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Status(args) = agent_task.command else {
            panic!("expected status command");
        };
        assert!(args.watch);
        assert_eq!(args.interval, "250ms");
        assert_eq!(args.timeout, "2m");
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--interval",
            "1s",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--timeout",
            "1m",
        ])
        .is_err());
    }
}
#[derive(Args, Debug, Default)]
pub struct RecoverArgs {
    /// Publish a green incomplete candidate unchanged and file remaining scope on its tracker.
    #[arg(long, conflicts_with_all = ["review_form", "regate", "candidate_ref", "review_overrides"])]
    pub publish_partial: bool,
    /// Publish as complete after operator verification of the remaining acceptance.
    #[arg(long, requires = "evidence", conflicts_with_all = ["publish_partial", "review_form", "review_overrides", "regate", "candidate_ref"])]
    pub publish: bool,
    /// Operator-verified acceptance evidence: literal text or @path to a UTF-8 file.
    #[arg(long, requires = "publish", value_name = "TEXT|@FILE")]
    pub evidence: Option<String>,
    /// Explanation for this operator intervention
    #[arg(long)]
    pub reason: Option<String>,
    /// Adopt this immutable commit through the recorded Cook recipe.
    #[arg(long, value_name = "SHA", conflicts_with_all = ["preflight", "regate", "timeout_ms", "backend", "selector", "model", "acknowledge_model_override"])]
    pub candidate_ref: Option<String>,
    /// Select the candidate's owning durable attempt.
    #[arg(long, value_name = "N", requires = "candidate_ref")]
    pub attempt: Option<u32>,
    /// Concrete model that prepared the supplied candidate (not a retry route override).
    #[arg(
        long = "adoption-model",
        value_name = "MODEL",
        requires = "candidate_ref",
        conflicts_with = "model"
    )]
    pub adoption_model: Option<String>,
    /// Replace a stale interrupted adoption, retaining its evidence.
    #[arg(long, requires = "candidate_ref")]
    pub replace_interrupted: bool,
    /// Stable key for resuming a non-Cook run without executing it twice.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
    /// Follow the admitted daemon step through status/watch.
    #[arg(long, conflicts_with = "preflight")]
    pub wait: bool,
    /// Maximum wait; timing out never cancels daemon-owned work.
    #[arg(long, default_value = "30m", requires = "wait")]
    pub timeout: String,
    /// Longest this client waits for Lab admission or runner capacity before
    /// a gate it runs starts. Past it, queued gate work is withdrawn, no
    /// start fence is kept, and recover exits non-zero. SIGINT/SIGTERM stop
    /// the same way at any time.
    #[arg(long, value_name = "SECONDS", conflicts_with = "preflight")]
    pub wait_timeout_seconds: Option<u64>,
    /// Re-run deterministic gates on the newest candidate's existing SHA,
    /// without a provider attempt. Each regate executes fresh gate jobs. With
    /// `--verify`/`--verify-file`/`--private-verify`, those commands replace
    /// the declared verify commands for this regate, and the replacement is
    /// recorded in the candidate's gate evidence. `--gate-timeout-seconds` and
    /// `--gate-no-progress-timeout-seconds` (`0` disables it) replace the
    /// recorded per-gate budgets whenever passed; the override is recorded as
    /// operator authority in the gate evidence.
    #[arg(
        long,
        conflicts_with_all = ["authorize_external_proof", "authorize_interrupted_rerun", "promotion"]
    )]
    pub regate: bool,
    /// Durable run ID, Cook ID, Cook attempt ID or batch-child run ID.
    pub id: String,
    /// Report the step recovery would perform without performing it. A
    /// continuation step also validates continuation admission read-only.
    #[arg(long)]
    pub preflight: bool,
    /// Perform a step the previous `recover` reported `blocked` because it made
    /// no progress, after resolving the blocker it named.
    #[arg(long, conflicts_with = "preflight")]
    pub unblock: bool,

    /// Explicitly increase the provider timeout for a new retry attempt of a
    /// terminal provider timeout. The override and its operator authority are
    /// retained in the Cook recipe.
    #[arg(
        long = "timeout-ms",
        value_name = "MS",
        conflicts_with_all = ["preflight"]
    )]
    pub timeout_ms: Option<u64>,
    /// Backend for a rearmed retry of a terminal attempt. The original Cook
    /// recipe remains authoritative for prompt, gates, worktree, notification
    /// and disclosure.
    #[arg(long, value_name = "BACKEND", conflicts_with_all = ["preflight", "timeout_ms"])]
    pub backend: Option<String>,
    /// Provider-specific selector for a rearmed retry of a terminal attempt.
    #[arg(long, value_name = "SELECTOR", conflicts_with_all = ["preflight", "timeout_ms"])]
    pub selector: Option<String>,
    /// Model for a rearmed retry of a terminal attempt. Confirm the pinned
    /// route with --acknowledge-model-override.
    #[arg(long, value_name = "MODEL", conflicts_with_all = ["preflight", "timeout_ms"])]
    pub model: Option<String>,
    /// Confirm an explicit model override that pins this retry.
    #[arg(long, requires = "model")]
    pub acknowledge_model_override: bool,
    /// With `--wait`, report the complete lifecycle status rather than the bounded summary.
    #[arg(long)]
    pub full: bool,
    /// Replace-gates step: explicit operator authorization for the replacement
    /// proof recorded for an applied candidate whose gates failed. Required
    /// with corrected gates for an applied candidate or with `--promotion`.
    /// Pre-provider recipe correction needs no external-proof authorization.
    #[arg(long, value_name = "TEXT")]
    pub authorize_external_proof: Option<String>,
    /// Replace-gates step: explicit authorization to rerun corrected gates
    /// after an interrupted replacement execution. Homeboy still refuses while
    /// the original operation lease is live.
    #[arg(long, value_name = "TEXT", requires = "authorize_external_proof")]
    pub authorize_interrupted_rerun: Option<String>,
    /// Replace-gates step: complete typed promotion report produced by an
    /// external, candidate-bound gate executor: inline JSON, `@FILE`, or `-`.
    #[arg(
        long,
        value_name = "JSON|@FILE|-",
        requires = "authorize_external_proof",
        conflicts_with = "authorize_interrupted_rerun"
    )]
    pub promotion: Option<String>,
    /// Corrected verification gates: with --regate, replace the declared
    /// verify commands for that regate; otherwise update a pre-provider
    /// failure's recipe, or, with --authorize-external-proof, verify the
    /// already-applied candidate.
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Publish step: complete typed AI review form for a candidate whose
    /// provider outcome recorded none (or declared the task incomplete):
    /// inline JSON, `@FILE`, or `-`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub review_form: Option<String>,
    /// Tool that authored the supplied review form.
    #[arg(long, value_name = "TOOL", requires = "review_form")]
    pub review_form_tool: Option<String>,
    /// Concrete model that authored the supplied review form.
    #[arg(long, value_name = "MODEL", requires = "review_form")]
    pub review_form_model: Option<String>,
    /// Operator or agent identity submitting the supplied review form.
    #[arg(long, value_name = "IDENTITY", requires = "review_form")]
    pub review_form_author: Option<String>,
    /// Publish step: explicit reviewer override in `TARGET=VALUE@PROVENANCE`
    /// form. Overrides require a recorded or supplied review form.
    #[arg(long = "review-override", value_name = "TARGET=VALUE@PROVENANCE")]
    pub review_overrides: Vec<String>,
}

impl RecoverArgs {
    /// The explicit operator overrides this invocation supplies.
    pub fn overrides(&self) -> homeboy::agents::agent_task_service::CookRecoveryOverrides {
        homeboy::agents::agent_task_service::CookRecoveryOverrides {
            timeout_ms: self.timeout_ms,
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            model: self.model.clone(),
            acknowledge_model_override: self.acknowledge_model_override,
        }
    }
}
#[derive(Args, Debug, Clone)]
pub struct RetryArgs {
    /// Durable run ID to retry. A stopped Cook is recovered with
    /// `agent-task recover <cook-or-attempt-id>`, which also takes explicit
    /// provider-route overrides for its next attempt.
    pub run_id: String,
    /// Durable ID to assign to the new retry run.
    #[arg(long, value_name = "ID")]
    pub new_run_id: Option<String>,
    /// Execute the retry immediately after creating it.
    #[arg(long)]
    pub run: bool,
    /// Permit a new retry after every prior retry in this lineage is terminal.
    #[arg(long)]
    pub force: bool,
    /// Stable caller key for safely replaying this retry reservation.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Durable run or Cook ID to cancel.
    pub run_id: String,
    /// Include the complete, redacted lifecycle record instead of the compact
    /// cancellation acknowledgement.
    #[arg(long)]
    pub full: bool,
    /// Optional explanation recorded with the cancellation.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Stable caller key for safely replaying this cancellation request.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}
#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// Durable run or Cook ID to review.
    pub run_id: String,
    /// Include complete lifecycle, promotion, and gate evidence. The default
    /// keeps one actionable candidate and bounded gate findings.
    #[arg(long)]
    pub full: bool,
    /// Target managed worktree handle for the review candidate.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: Option<String>,
    /// Exact argv element for the promotion apply provider; repeat per element.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        long_help = "Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
    )]
    pub provider_argv: Vec<String>,
}
#[derive(Args, Debug)]
pub struct FinalizePrArgs {
    /// Include complete finalization and gate evidence.
    #[arg(long)]
    pub full: bool,
    /// Durable run ID for a manual finalization record.
    #[arg(long, value_name = "ID")]
    pub run_id: String,
    /// Worktree path containing the manual finalization candidate.
    #[arg(long, value_name = "PATH")]
    pub path: String,
    /// Registered component identity for disambiguating a shared-repository worktree.
    #[arg(long, value_name = "COMPONENT_ID")]
    pub component: Option<String>,
    /// Base branch for the manual finalization candidate.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,
    /// Immutable base commit SHA recorded before the declared verification gates ran.
    #[arg(long, value_name = "SHA")]
    pub verified_base_sha: Option<String>,
    /// Head branch for the manual finalization candidate.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,
    /// Pull request title for the manual finalization candidate.
    #[arg(long, value_name = "TEXT")]
    pub title: String,
    /// Commit message for the manual finalization candidate.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: String,
    /// Reviewer-facing evidence for the finalization dossier.
    #[command(flatten)]
    pub evidence: review::FinalizePrEvidenceArgs,
    /// Recorded result for a verification gate: `NAME=STATUS[:DETAIL]`.
    #[arg(long = "gate-result", value_name = "NAME=STATUS[:DETAIL]")]
    pub gate_results: Vec<String>,
    /// Execute a deterministic verification command against the committed manual candidate. Repeat for multiple gates.
    #[arg(long, value_name = "COMMAND")]
    pub verify: Vec<String>,
    /// Changed file path to include in the finalization dossier.
    #[arg(long = "changed-file", value_name = "PATH")]
    pub changed_files: Vec<String>,
    /// Branch that must not be updated by finalization; repeat for multiple branches.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,
    /// Summary of the finalization candidate.
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,
    /// User-visible change description; repeat for multiple entries.
    #[arg(long = "what-changed", value_name = "TEXT")]
    pub what_changed: Vec<String>,
    /// Reviewer test step. Strict shape: COMMAND=>EXPECTED.
    #[arg(long = "test-step", value_name = "COMMAND=>EXPECTED")]
    pub test_steps: Vec<String>,
    /// Closing issue reference: #NUMBER, OWNER/REPO#NUMBER, or a github.com issue URL.
    #[arg(long = "closes", value_name = "ISSUE_REF")]
    pub closes: Vec<String>,
    /// Related issue reference: #NUMBER, OWNER/REPO#NUMBER, or a github.com issue URL.
    #[arg(long = "relates-to", value_name = "ISSUE_REF")]
    pub relates_to: Vec<String>,
    /// Explicit reviewer override in `TARGET=VALUE@PROVENANCE` form.
    #[arg(long = "review-override", value_name = "TARGET=VALUE@PROVENANCE")]
    pub review_overrides: Vec<String>,
    /// Validate the complete hydrated dossier and candidate without publishing.
    #[arg(long)]
    pub preflight: bool,
    /// Publish corrected, independently verified work without a promotion lineage. The ID must identify a failed attempt (a Cook ID resolves to its newest attempt, which must be failed), or be unused so Homeboy can reserve a durable manual-finalization record for its intent and receipt.
    #[arg(long)]
    pub manual_finalization: bool,
}
