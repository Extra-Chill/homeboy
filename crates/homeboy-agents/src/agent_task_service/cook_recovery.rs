//! The one Cook recovery decision and its executor (#16049 slice 4, #15925).
//!
//! A stopped Cook used to leave its operator choosing between overlapping
//! verbs (`resume`, the retired `cook-continue [--rearm] [--artifact-id]`,
//! `retry --run`, the retired `retry --backend/--selector/--model` Cook route
//! rearm, `reconcile`, ...). [`cook_recovery_step`] is the single typed
//! decision over
//! the same inputs the legal-action classifier reads, and the classifier
//! derives its mutating action from it, so `status` "Next:" and
//! `agent-task recover` can never disagree.
//!
//! [`recover_cook`] resolves a Cook, attempt or batch-child ID to its Cook,
//! computes the step, and either reports it (`preflight`) or performs it by
//! calling the handler the step names through a [`CookRecoveryPerformer`]. It
//! adds no delivery logic of its own: continuation re-enters the Cook spine
//! through the CLI's Cook continuation handler.

use homeboy_core::cook_status::CookStatus;
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_lifecycle;

/// Schema of the report [`recover_cook`] returns.
pub const COOK_RECOVERY_SCHEMA: &str = "homeboy/agent-task-cook-recovery/v1";

/// The single correct next recovery step for a stopped Cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CookRecoveryStep {
    /// Continue the Cook from its durable recipe. This also
    /// materializes a recipe-only attempt whose lifecycle record is absent.
    Continue,
    /// Re-run the recorded gates on the existing candidate, then publish.
    Regate,
    /// Rearm the attempt's failed terminal continuation.
    Rearm,
    /// Reserve and run a fresh successor attempt (`retry --run`).
    Retry,
    /// The exact checkpoint no longer matches its destination; fork a
    /// replacement attempt that preserves the evidence (`retry --run`).
    ForkReplacement,
    /// A promotion claim is still held; reconcile it before anything else
    /// (`reconcile --apply`).
    Reconcile,
    /// The already-applied candidate failed, deferred or never completed its
    /// gates. Re-verify that exact candidate with corrected gates, or record
    /// candidate-bound external proof, without re-applying the patch or
    /// replaying provider work. Both need explicit operator authorization
    /// (`recover --authorize-external-proof ...`), so only those inputs select
    /// this step; without them the planner re-gates the candidate.
    ReplaceGates,
    /// The verified candidate is unpublished; publish it from the durable Cook
    /// record.
    Publish,
    /// The Cook produced no candidate patch, but the destination worktree's
    /// branch holds commits ahead of its recorded base: the operator's verified
    /// work is the candidate. Adopt that exact revision into the Cook
    /// (`agent-task recover <cook> --candidate-ref <head-sha>`) instead of
    /// replaying provider work that would end `no_changes` again (#16181).
    Adopt { candidate_ref: String },
    /// The Cook failed before materializing, so it has no recipe or attempt
    /// to continue. Admit the same Cook request again from its recorded
    /// admission inputs under a fresh run id, linking the two runs.
    Relaunch,
    /// The previous `recover` already performed `repeated` from this exact
    /// durable state and left it unchanged. Performing it again would loop, so
    /// recovery names the `blocker` instead (#17650).
    Blocked { repeated: String, blocker: String },
    /// Nothing `recover` can perform; `reason` names what is required.
    None { reason: String },
}

/// A rendered next command; `show` marks a read-only status action.
pub struct CookRecoveryNextAction {
    pub label: String,
    pub command: String,
    pub show: bool,
}

impl CookRecoveryStep {
    /// Render the next command with the runner-aware `prefix`.
    pub fn next_action(&self, prefix: &str, run_id: &str) -> CookRecoveryNextAction {
        let command = |args: &[&str]| super::cook_recovery_command_with_prefix(prefix, args);
        let (label, command) = match self {
            Self::Blocked { blocker, .. } => (
                format!("recovery made no progress: {blocker}; cancel this Cook"),
                command(&["cancel", run_id]),
            ),
            Self::None { reason } => (reason.clone(), command(&["status", run_id])),
            Self::Adopt { candidate_ref } => (
                "adopt the preserved candidate".to_string(),
                command(&["recover", run_id, "--candidate-ref", candidate_ref]),
            ),
            Self::Regate => (
                "re-run the gates on the existing candidate; add `--regate --verify '<COMMAND>'` to replace its declared verify commands, or `--regate --gate-timeout-seconds <SECONDS> --gate-no-progress-timeout-seconds <SECONDS>` to change its gate budgets".to_string(),
                command(&["recover", run_id]),
            ),
            _ => (
                "perform the Cook's next recovery step".to_string(),
                command(&["recover", run_id]),
            ),
        };
        CookRecoveryNextAction {
            label,
            command,
            show: matches!(self, Self::None { .. }),
        }
    }

    /// The legal-action name status and notifications advertise for this
    /// step. `None` advertises no mutating action.
    pub fn action_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::Continue => "resume",
            Self::Regate => "regate",
            Self::Rearm => "rearm",
            Self::Retry => "retry",
            Self::ForkReplacement => "fork_replacement",
            Self::Reconcile => "reconcile",
            Self::ReplaceGates => "replace_gates",
            Self::Publish => "publish",
            Self::Adopt { .. } => "adopt",
            Self::Relaunch => "relaunch",
            Self::Blocked { .. } | Self::None { .. } => return None,
        })
    }

    /// No mutating step: nothing is legal, or the only one is blocked.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None { .. } | Self::Blocked { .. })
    }

    pub(crate) fn none(reason: impl Into<String>) -> Self {
        Self::None {
            reason: reason.into(),
        }
    }

    /// The mutating legal action for this step, rendered by [`Self::next_action`].
    pub(crate) fn recovery_actions(
        &self,
        prefix: &str,
        run_id: &str,
    ) -> Vec<super::AgentTaskCookRecoveryAction> {
        let command = self.next_action(prefix, run_id).command;
        self.action_name()
            .map(|action| super::AgentTaskCookRecoveryAction {
                action: action.to_string(),
                command,
            })
            .into_iter()
            .collect()
    }
}

/// Everything the step decision reads. These are exactly the facts the
/// legal-action classifier has always used, so both share one answer.
#[derive(Debug, Clone, Default)]
pub(crate) struct CookRecoveryInputs<'a> {
    pub status: &'a str,
    /// The attempt's one classified run state. Whether recovery may act at
    /// all is its legal-action projection, never a re-reading of `status`.
    pub run_state: Option<agent_task_lifecycle::CookRunState>,
    /// A durable lifecycle record exists for the attempt.
    pub recovery_legal: bool,
    pub disconnected_runner: bool,
    /// Another controller holds the Cook's kernel driver lock right now.
    pub live_driver: bool,
    /// A promotion operation claim is still `running`.
    pub blocking_claim: bool,
    /// Retry (or continuation) admission accepted this attempt.
    pub retry_admitted: bool,
    /// Why retry admission refused this attempt, when it did.
    pub retry_refusal: Option<&'a str>,
    /// The attempt was cancelled without an operator request (a runner-side
    /// cancel, a wait budget) and retry admission accepts a
    /// fresh attempt from its plan. Continuation admission does not apply:
    /// the cancelled attempt has no terminal result to continue from.
    pub involuntary_cancellation_retry_admitted: bool,
    /// The checkpoint's typed recovery action is `fork_replacement`.
    pub exact_checkpoint_candidate_mismatch: bool,
    /// The attempt's terminal continuation failed and must be rearmed.
    pub continuation_rearm_required: bool,
    /// A completed terminal continuation cannot be claimed again.
    pub continuation_completed: bool,
    /// Why continuation's candidate admission refuses this attempt; recovery
    /// then has no continuation to perform.
    pub continuation_refusal: Option<&'a str>,
    /// Authoritative observation has no provider outcome to continue.
    pub provider_outcome_missing: bool,
    /// The attempt whose recorded review form is consulted below.
    pub run_id: &'a str,
    /// `incomplete_reason` of a recorded review form that declares the task
    /// incomplete. Continuation re-reads that same terminal verdict, so it
    /// can never publish the candidate.
    pub agent_incomplete_reason: Option<&'a str>,
    /// The attempt's applied candidate failed (or never completed) its
    /// deterministic gates and holds no replacement proof yet.
    pub applied_gate_failure: bool,
    pub regate_ready: bool,
    /// Applied, verified candidate with no publication receipt. Gates are
    /// never replayed for it (#16441).
    pub verified_unpublished: bool,
    /// HEAD of the destination worktree when its branch holds commits ahead
    /// of the recorded base (#16181). A terminal `no_changes` attempt in that
    /// state is adoptable: the operator's committed work is the candidate the
    /// provider could not produce.
    pub adoptable_candidate_ref: Option<&'a str>,
    /// The previous `recover` performed a step from this exact attempt and
    /// status without changing either (#17650).
    pub no_progress: Option<&'a CookRecoveryNoProgress>,
}

/// The complete operator override for an agent-declared incomplete candidate.
pub(crate) fn incomplete_review_recovery_command(id: &str) -> String {
    format!(
        "homeboy agent-task recover {} --review-form @FILE --review-form-tool <tool> --review-form-model <model> --review-form-author <author>",
        homeboy_core::engine::shell::quote_arg(id)
    )
}

/// The command that re-gates the existing candidate, optionally replacing its
/// declared verify commands (#17802).
pub(crate) fn regate_recovery_command(id: &str) -> String {
    format!(
        "homeboy agent-task recover {} --regate [--verify '<COMMAND>'] [--gate-timeout-seconds <SECONDS>] [--gate-no-progress-timeout-seconds <SECONDS>]",
        homeboy_core::engine::shell::quote_arg(id)
    )
}

/// Decide the single next recovery step from durable Cook evidence. This is
/// the one planner: `status` "Next:", `diagnose`, `recover --preflight` and
/// `recover` all read their step from it, so none can name a step another
/// would not perform.
pub(crate) fn cook_recovery_step(inputs: &CookRecoveryInputs<'_>) -> CookRecoveryStep {
    let step = decide_cook_recovery_step(inputs);
    match (inputs.no_progress, step.action_name()) {
        (Some(previous), Some(action)) if previous.step == action => CookRecoveryStep::Blocked {
            repeated: action.to_string(),
            blocker: previous.blocker.clone(),
        },
        _ => step,
    }
}

fn decide_cook_recovery_step(inputs: &CookRecoveryInputs<'_>) -> CookRecoveryStep {
    if !inputs.recovery_legal {
        return CookRecoveryStep::none(
            "No recovery command is legal because the durable recipe has no lifecycle record. Start a fresh Cook after preserving the recipe reference for investigation.",
        );
    }
    // A live driver owns every transition of its Cook; any step recovery
    // took would race it, and `diagnose` reports the same Cook as running.
    if inputs.live_driver {
        return CookRecoveryStep::none(
format!(
            "A live Cook driver is running this Cook; watch it with `homeboy agent-task status {} --watch` instead.",
            inputs.run_id
        ));
    }
    if removed_runner_attempt(inputs.run_id) {
        return CookRecoveryStep::Reconcile;
    }
    if inputs.disconnected_runner {
        return CookRecoveryStep::Continue;
    }
    let status = CookStatus::from_status(inputs.status);
    if inputs.run_state.as_ref().is_some_and(|state| {
        !state.allows(homeboy_control_plane_contract::ControlPlaneAction::Recover)
    }) {
        return CookRecoveryStep::none(
format!(
            "The Cook is still in flight; watch it with `homeboy agent-task status {} --watch` instead.",
            inputs.run_id
        ));
    }
    if inputs.blocking_claim {
        return CookRecoveryStep::Reconcile;
    }
    // A publishable candidate whose agent declared the task incomplete stops
    // here whatever the gate status: rearming would replay provider work only
    // to re-read the same verdict (#16772).
    let publishable = inputs.verified_unpublished && !status_is_settled(&status);
    let recoverable = inputs.run_state.as_ref().map_or_else(
        || agent_task_lifecycle::CookRunPhase::status_is_recoverable(&status),
        |state| state.recoverable,
    );
    if recoverable || publishable {
        // An incomplete verdict on a candidate whose gates never passed is not
        // the last word: the candidate was never judged (or failed a gate the
        // operator may correct), so re-gating it is the step that makes
        // progress. The operator can still publish with a complete review
        // form once the gates pass (#17802).
        if inputs.agent_incomplete_reason.is_some()
            && inputs.applied_gate_failure
            && !publishable
            && !status_is_settled(&status)
        {
            return CookRecoveryStep::Regate;
        }
        if let Some(step) = agent_incomplete_stop(inputs) {
            return step;
        }
    }
    // A gate-green candidate publishes; without a usable review form it
    // publishes as a draft (#17671).
    if publishable {
        return CookRecoveryStep::Publish;
    }
    if inputs.regate_ready && !status_is_settled(&status) {
        return CookRecoveryStep::Regate;
    }
    // The applied candidate failed its gates. When no continuation can re-verify
    // it, re-gating that exact candidate is the step `recover` performs on its
    // own; the operator can change the gate budgets or commands on that regate
    // (#18556). Replacement proof stays an explicit operator input.
    let regate_failed_candidate = inputs.applied_gate_failure && !status_is_settled(&status);
    // A stale destination checkpoint cannot authorize replacing a verified
    // candidate. Publication validates and uses its immutable candidate commit.
    if inputs.exact_checkpoint_candidate_mismatch && inputs.retry_admitted {
        return CookRecoveryStep::ForkReplacement;
    }
    if recoverable && inputs.continuation_completed {
        if regate_failed_candidate {
            return CookRecoveryStep::Regate;
        }
        return CookRecoveryStep::none(
format!(
            "The candidate's terminal continuation is already completed; inspect it with `homeboy agent-task diagnose {run}`, then publish it with `homeboy agent-task recover {run} --review-form @FORM.json --review-form-tool <TOOL> --review-form-model <MODEL> --review-form-author <IDENTITY>`.",
            run = inputs.run_id
        ));
    }
    if status == CookStatus::NoChanges {
        if let Some(candidate_ref) = inputs.adoptable_candidate_ref {
            return CookRecoveryStep::Adopt {
                candidate_ref: candidate_ref.to_string(),
            };
        }
    }
    if status == CookStatus::PreExecutionFailure
        && inputs.continuation_rearm_required
        && inputs.continuation_refusal.is_none()
    {
        // Older Lab reports could label a successful provider result as a
        // pre-execution failure. Candidate admission, not that stale label,
        // authorizes replaying the failed terminal continuation.
        return continuation_step(inputs);
    }
    if status == CookStatus::PreExecutionFailure && inputs.applied_gate_failure {
        // Gate infrastructure can fail before a gate executes, after the
        // provider candidate was applied. Continue that candidate, not provider
        // execution; the recipe retains its admitted gate runner.
        return continuation_step(inputs);
    }
    if status == CookStatus::PreExecutionFailure {
        if inputs.retry_admitted {
            return CookRecoveryStep::Retry;
        }
        if let Some(refusal) = inputs.retry_refusal {
            return CookRecoveryStep::none(format!(
                "Retry admission refused this `pre_execution_failure` attempt: {refusal}. Inspect it with `homeboy agent-task diagnose {}`.",
                inputs.run_id
            ));
        }
    }
    // Only an operator's `agent-task cancel` settles a Cook. Any other cancel
    // left it without a result, and `agent-task retry` already admits a fresh
    // attempt, so `recover` performs that retry instead of reporting no step.
    if status == CookStatus::Cancelled && inputs.involuntary_cancellation_retry_admitted {
        return CookRecoveryStep::Retry;
    }
    let continuation_eligible = match &status {
        CookStatus::ReviewReady
        | CookStatus::GreenNoFinalize
        | CookStatus::IntentionalNoChange
        | CookStatus::NoCandidate
        | CookStatus::Cancelled
        | CookStatus::ExecutionBudgetExhausted
        | CookStatus::RetriesExhausted
        | CookStatus::PreExecutionFailure => false,
        // Infrastructure deferral consumes neither a provider retry nor budget:
        // continuation re-verifies the already materialized candidate.
        CookStatus::GateEnvironmentUnavailable => true,
        status if status.is_gate_failure() => inputs.retry_admitted,
        _ => true,
    };
    if !continuation_eligible {
        if status.is_gate_failure() && regate_failed_candidate {
            return CookRecoveryStep::Regate;
        }
        return CookRecoveryStep::none(format!(
            "Cook status `{}` admits no recovery step; inspect it with `homeboy agent-task diagnose {}`.",
            inputs.status, inputs.run_id
        ));
    }
    if regate_failed_candidate
        && inputs.continuation_refusal.is_some()
        && !inputs.provider_outcome_missing
        && inputs.agent_incomplete_reason.is_none()
    {
        return CookRecoveryStep::Regate;
    }
    continuation_step(inputs)
}

/// Read the existing liveness classification without projecting or settling
/// runner state. Only explicit recovery applies the single-run reconciliation.
fn removed_runner_attempt(run_id: &str) -> bool {
    let Ok(store) = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment() else {
        return false;
    };
    agent_task_lifecycle::exact_record(run_id).is_ok_and(|record| {
        !record.state().is_terminal()
            && agent_task_lifecycle::observe_runner_job_in_store(&store, &record).is_some_and(
                |evidence| {
                    evidence.observation
                        == agent_task_lifecycle::RunnerJobObservation::RunnerRemoved
                },
            )
            && super::discovery::discover_reconcile_run(record)
                .liveness
                .is_some_and(super::discovery::AgentTaskLiveness::is_reconcilable)
    })
}

/// The agent declared the task incomplete: no step that re-reads its verdict
/// can publish the candidate (#16772).
fn agent_incomplete_stop(inputs: &CookRecoveryInputs<'_>) -> Option<CookRecoveryStep> {
    inputs.agent_incomplete_reason.map(|reason| {
        CookRecoveryStep::none(format!(
            "The agent's review form declares the task incomplete: {reason}. Continuing would re-read the same verdict, so `recover` cannot publish this candidate on its own. If the task is in fact done, publish it with a complete review form via `{command}`, otherwise start a fresh Cook.",
            reason = reason.trim_end_matches('.'),
            command = incomplete_review_recovery_command(inputs.run_id)
        ))
    })
}

/// Continue the Cook, rearming its failed terminal continuation first.
fn continuation_step(inputs: &CookRecoveryInputs<'_>) -> CookRecoveryStep {
    // Continuation replays the same attempt and re-reads its agent's
    // verdict, so an incomplete review form stops it whatever the status.
    if let Some(step) = agent_incomplete_stop(inputs) {
        return step;
    }
    if let Some(refusal) = inputs.continuation_refusal {
        if inputs.provider_outcome_missing {
            return if inputs.retry_admitted {
                CookRecoveryStep::Retry
            } else {
                CookRecoveryStep::none(format!(
                    "No provider candidate exists; retry admission refused this attempt: {}. Inspect it with `homeboy agent-task diagnose {}`.",
                    inputs.retry_refusal.unwrap_or("retry is not admitted"),
                    inputs.run_id
                ))
            };
        }
        return CookRecoveryStep::none(format!(
            "Continuation cannot admit this attempt's candidate: {refusal}. Inspect it with `homeboy agent-task diagnose {}`.",
            inputs.run_id
        ));
    }
    if inputs.continuation_rearm_required {
        CookRecoveryStep::Rearm
    } else {
        CookRecoveryStep::Continue
    }
}

/// Stop reason for a Cook whose candidate and immutable baseline failed the
/// same required gate without proof that would let it publish. It names the
/// commands that make progress: re-run the gates once the shared environment
/// is repaired, or re-verify the candidate with corrected gates.
pub(crate) fn baseline_red_refusal(cook_id: &str) -> String {
    format!(
        "candidate and immutable baseline failed the same required gate; repair the inherited infrastructure or gate environment, then run `homeboy agent-task recover {cook_id} --regate`, or re-verify the candidate with corrected gates via `homeboy agent-task recover {cook_id} --authorize-external-proof <AUTHORIZATION> --verify '<CANDIDATE-BOUND COMMAND>'`"
    )
}

/// Statuses whose candidate is already delivered or deliberately left
/// unpublished; a green candidate there is never republished by `recover`.
fn status_is_settled(status: &CookStatus) -> bool {
    matches!(
        status,
        CookStatus::ReviewReady
            | CookStatus::DraftPublished
            | CookStatus::GreenNoFinalize
            | CookStatus::IntentionalNoChange
            | CookStatus::NoCandidate
            | CookStatus::Cancelled
    )
}

/// A resolved Cook and the step [`recover_cook`] would perform for it.
#[derive(Debug, Clone, Serialize)]
pub struct CookRecoveryPlan {
    pub cook_id: String,
    pub run_id: String,
    pub status: String,
    pub step: CookRecoveryStep,
}

/// Explicit operator inputs `recover` accepts in addition to the step. Each
/// reserves a fresh provider attempt with a changed parameter, so `recover`
/// never applies one on its own: the operator names it, and the continuation
/// handler validates it against the attempt's terminal state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookRecoveryOverrides {
    /// Larger provider timeout for the successor attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Provider route for the successor attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub acknowledge_model_override: bool,
}

impl CookRecoveryOverrides {
    pub fn is_empty(&self) -> bool {
        self.timeout_ms.is_none() && !self.has_route_override()
    }

    /// A provider route override is a rearm of the terminal attempt.
    pub fn has_route_override(&self) -> bool {
        self.backend.is_some() || self.selector.is_some() || self.model.is_some()
    }
}

/// Corrected replacement verification for the already-applied candidate of a
/// [`CookRecoveryStep::ReplaceGates`] step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum CookReplacementProof {
    /// Execute these corrected gates against the applied candidate.
    Gates(crate::agent_task_gate::VerifyGateOptions),
    /// Record this externally produced, candidate-bound promotion report.
    External {
        promotion: Box<crate::agent_task_promotion::AgentTaskPromotionReport>,
    },
}

/// Operator inputs that feed [`CookRecoveryStep::ReplaceGates`]. Replacement
/// proof always requires an explicit authorization; `recover` never supplies
/// one on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CookReplaceGatesRequest {
    pub proof: CookReplacementProof,
    /// Explicit operator authorization for the recorded replacement proof.
    pub authorize_external_proof: String,
    /// Explicit authorization to rerun gates after an interrupted replacement
    /// execution. Only meaningful with [`CookReplacementProof::Gates`].
    pub authorize_interrupted_rerun: Option<String>,
}

/// Operator inputs for `recover --regate` (#17802). Empty inputs re-run the
/// recorded gates unchanged. Replacement commands replace only the recorded
/// deterministic commands; the recorded gate environment, runner placement
/// and supervision are kept. The replacement is recorded as the authorization
/// of the candidate's replacement gate proof, so the published evidence names
/// the commands that passed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CookRegateInputs {
    /// Replacement verification commands (`--verify`, `--verify-file`,
    /// `--private-verify`, `--private-verify-file`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gates: Option<crate::agent_task_gate::VerifyGateOptions>,
    /// Replacement wall-clock limit for each gate (`--gate-timeout-seconds`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_timeout_seconds: Option<u64>,
    /// Replacement no-progress window for each gate
    /// (`--gate-no-progress-timeout-seconds`); `0` disables it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_no_progress_timeout_seconds: Option<u64>,
}

impl CookRegateInputs {
    pub fn is_empty(&self) -> bool {
        self.gates.is_none()
            && self.gate_timeout_seconds.is_none()
            && self.gate_no_progress_timeout_seconds.is_none()
    }

    /// The gates a regate executes: the recorded policy with these operator
    /// replacements applied.
    pub fn apply(
        &self,
        mut recorded: crate::agent_task_gate::VerifyGateOptions,
    ) -> crate::agent_task_gate::VerifyGateOptions {
        if let Some(replacement) = &self.gates {
            recorded.verify = replacement.verify.clone();
            recorded.private_verify = replacement.private_verify.clone();
            recorded.input_sources = replacement.input_sources.clone();
            recorded.private_gate_reveal = replacement.private_gate_reveal;
            recorded.execution_policy = replacement.execution_policy;
            // Explicit commands replace every recorded command source.
            recorded.repro = Vec::new();
            recorded.test_execution_plan = None;
        }
        if let Some(seconds) = self.gate_timeout_seconds {
            recorded.gate_timeout_seconds = seconds;
        }
        if let Some(seconds) = self.gate_no_progress_timeout_seconds {
            recorded.gate_no_progress_timeout_seconds = seconds;
        }
        recorded
    }

    /// The authorization recorded with the replacement gate proof. It names
    /// any replacement so reviewers can see which gates the candidate passed.
    pub fn authorization(&self) -> String {
        let mut authorization = "recover regate recorded deterministic gates".to_string();
        if let Some(gates) = &self.gates {
            authorization = format!(
                "recover --regate replaced the declared verify commands with: {}",
                gates
                    .verify
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .chain(
                        (!gates.private_verify.is_empty())
                            .then(|| format!("{} private command(s)", gates.private_verify.len()))
                    )
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        // Budget overrides are operator authority: the published evidence
        // names the limits the candidate's gates ran under (#18556).
        let budgets = [
            self.gate_timeout_seconds
                .map(|seconds| format!("gate timeout {seconds}s")),
            self.gate_no_progress_timeout_seconds
                .map(|seconds| match seconds {
                    0 => "gate no-progress limit disabled".to_string(),
                    seconds => format!("gate no-progress limit {seconds}s"),
                }),
        ];
        let budgets = budgets.into_iter().flatten().collect::<Vec<_>>();
        if !budgets.is_empty() {
            authorization.push_str(&format!(" (operator-authorized {})", budgets.join(", ")));
        }
        authorization
    }
}

/// Operator inputs that feed [`CookRecoveryStep::Publish`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CookPublishRequest {
    /// Complete review form for a candidate whose provider outcome recorded
    /// none (or declared the task incomplete).
    pub review_form: Option<super::AgentTaskSuppliedReviewForm>,
    /// Fallback component identity for a durable record that recorded none.
    pub component: Option<String>,
    /// Explicit reviewer overrides applied to the hydrated dossier.
    pub review_overrides: Vec<crate::agent_task_review_dossier::AgentTaskReviewOverride>,
}

impl CookPublishRequest {
    pub fn is_empty(&self) -> bool {
        self.review_form.is_none() && self.component.is_none() && self.review_overrides.is_empty()
    }
}

/// Everything one `agent-task recover` invocation supplies beyond its ID.
#[derive(Debug, Clone, Default)]
pub struct CookRecoveryRequest {
    /// Report (and validate read-only) the step instead of performing it.
    pub preflight: bool,
    /// Re-verify the newest candidate-bearing attempt without provider work.
    pub regate: bool,
    /// Replacement commands or gate timeout for `regate`.
    pub regate_inputs: CookRegateInputs,
    /// Perform a step the previous recovery reported `blocked`, after the
    /// operator resolved its named blocker.
    pub unblock: bool,
    /// Explicit successor-attempt overrides.
    pub overrides: CookRecoveryOverrides,
    /// Inputs for [`CookRecoveryStep::ReplaceGates`]. Supplying them selects
    /// that step for an applied candidate whose gates failed.
    pub replace_gates: Option<CookReplaceGatesRequest>,
    /// Correct the recipe gates before retrying a pre-provider failure.
    pub corrected_gates: Option<crate::agent_task_gate::VerifyGateOptions>,
    /// Inputs for [`CookRecoveryStep::Publish`]. Supplying them selects
    /// publication when no other mutating step must come first.
    pub publish: CookPublishRequest,
}

/// The handlers a recovery step performs. Implemented by the CLI with the
/// exact Cook continuation, retry and reconcile handlers, so recovery has no
/// orchestration of its own. Replacement gates and publication default to the
/// service handlers the retired `verify-replacement`,
/// `record-replacement-gate-proof` and `finalize-pr --recover` verbs called.
pub trait CookRecoveryPerformer {
    /// Admit slow continuation work through the durable execution outbox.
    /// Workers return `None` to execute the already admitted step.
    fn admit_step(
        &self,
        _plan: &CookRecoveryReport,
        _overrides: &CookRecoveryOverrides,
    ) -> Result<Option<Value>> {
        Ok(None)
    }
    /// Continue the Cook from `run_id`, optionally rearming its failed
    /// terminal continuation, promoting one named patch artifact, or reserving
    /// a successor attempt with explicit operator overrides.
    fn continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        overrides: &CookRecoveryOverrides,
    ) -> Result<(Value, i32)>;
    /// Validate continuation admission for `run_id` without dispatching a
    /// provider or mutating lifecycle state.
    fn preflight_continue_cook(&self, run_id: &str, rearm: bool) -> Result<(Value, i32)>;
    /// `retry <run_id> --run`.
    fn retry(&self, run_id: &str) -> Result<(Value, i32)>;
    /// `reconcile <run_id> --apply`.
    fn reconcile(&self, run_id: &str) -> Result<(Value, i32)>;
    /// Adopt the operator's committed candidate revision into the Cook for a
    /// [`CookRecoveryStep::Adopt`] step (#16181).
    fn adopt(&self, run_id: &str, candidate_ref: &str) -> Result<(Value, i32)>;
    /// Record replacement proof for the exact applied candidate of `run_id`.
    fn replace_gates(
        &self,
        run_id: &str,
        request: CookReplaceGatesRequest,
    ) -> Result<(Value, i32)> {
        let report = match request.proof {
            CookReplacementProof::Gates(gates) => super::verify_replacement_gates(
                run_id,
                gates,
                request.authorize_external_proof,
                request.authorize_interrupted_rerun,
            )?,
            CookReplacementProof::External { promotion } => super::record_replacement_gate_proof(
                run_id,
                *promotion,
                Some(request.authorize_external_proof),
            )?,
        };
        Ok((
            serde_json::to_value(report)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
            0,
        ))
    }
    /// Re-run the durable deterministic gates without a provider attempt,
    /// with the operator's replacement commands when `inputs` carries them.
    /// Every regate executes fresh gate jobs; it never replays a recorded gate
    /// receipt (#18315).
    fn regate(&self, run_id: &str, inputs: &CookRegateInputs) -> Result<(Value, i32)> {
        if super::cook_promotion::persisted_promotion_for_attempt(run_id)?
            .is_some_and(|promotion| promotion.applied_with_passing_verdict())
        {
            // Replacement commands for an already-green candidate would only
            // replay its recorded proof, never execute them (#18315).
            if !inputs.is_empty() {
                return Err(Error::validation_invalid_argument(
                    "regate",
                    "the candidate already passed its gates; replacement regate gates would not execute",
                    Some(run_id.to_string()),
                    Some(vec![format!("homeboy agent-task recover {run_id}")]),
                ));
            }
            return self.publish(run_id, &CookPublishRequest::default(), false);
        }
        let recipe = super::load_recipe_for_attempt(run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "regate",
                "candidate has no recipe",
                Some(run_id.into()),
                None,
            )
        })?;
        let gates = inputs.apply(
            serde_json::from_value(recipe.gate_policy)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
        );
        // A failing or unavailable regate is persisted as the latest gate run
        // and returned here, so the report names the fresh gates (#18315).
        let report =
            super::cook_promotion::regate_candidate_gates(run_id, gates, inputs.authorization())?;
        let mut value = serde_json::to_value(&report)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        if !report.finalization_eligible() {
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "failure".to_string(),
                    serde_json::json!({
                        "code": "agent_task.regate_gates_not_passing",
                        "message": format!(
                            "regate ran fresh gates on the candidate and they did not pass (status: {}); the new gate run is now the latest promotion",
                            serde_json::to_value(report.status)
                                .ok()
                                .and_then(|status| status.as_str().map(str::to_string))
                                .unwrap_or_default()
                        ),
                        "next_command": format!("homeboy agent-task status {run_id}"),
                    }),
                );
            }
            return Ok((value, 1));
        }
        // The agent declared the task incomplete: green gates cannot overrule
        // that verdict (#16051), so the verified candidate is handed to the
        // operator's review form instead of being published here.
        if let Ok(store) = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment() {
            if let Some(reason) =
                super::cook_promotion::attempt_agent_incomplete_reason(&store, run_id)
            {
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "handoff".to_string(),
                        serde_json::json!({
                            "reason": format!("The candidate passed its gates, but the agent's review form declares the task incomplete: {reason}"),
                            "next_command": incomplete_review_recovery_command(run_id),
                        }),
                    );
                }
                return Ok((value, 0));
            }
        }
        self.publish(run_id, &CookPublishRequest::default(), false)
    }
    /// Publish (or with `preflight`, validate publication of) the verified
    /// candidate of `run_id`.
    fn publish(
        &self,
        run_id: &str,
        request: &CookPublishRequest,
        preflight: bool,
    ) -> Result<(Value, i32)> {
        let value = super::recover_cook_pr_with_review_form(
            run_id,
            request.review_form.clone(),
            request.component.as_deref(),
            request.review_overrides.clone(),
            preflight,
        )?;
        let success = value
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(status, "review_ready" | "draft_published" | "validated")
            });
        Ok((value, i32::from(!success)))
    }
}

/// `agent-task recover` output.
#[derive(Debug, Clone, Serialize)]
pub struct CookRecoveryReport {
    pub schema: &'static str,
    pub cook_id: String,
    pub run_id: String,
    pub status: String,
    pub step: CookRecoveryStep,
    pub preflight: bool,
    /// Explicit operator overrides this recovery applied instead of the step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<CookRecoveryOverrides>,
    /// Operator replacements a regate step executes with (#17802).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regate: Option<CookRegateInputs>,
    /// Continuation admission `--preflight` validated for a continuation step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission: Option<Value>,
    pub result: Option<Value>,
    /// The declared cause of a refused recovery step or a state admitting none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Value>,
}

/// Exit code for a report whose step is [`CookRecoveryStep::None`], declaring
/// the step's reason as the failure cause when the exit is nonzero.
fn finish_without_step(mut report: CookRecoveryReport) -> (CookRecoveryReport, i32) {
    if let CookRecoveryStep::Blocked { repeated, blocker } = &report.step {
        report.failure = Some(serde_json::json!({
            "code": "agent_task.recover_blocked",
            "message": format!(
                "`recover` already performed `{repeated}` from this exact durable state and environment and it made no progress: {blocker}. Resolve that blocker, then run `homeboy agent-task recover {run} --unblock`, or supply explicit recovery inputs. Upgrading the controller or refreshing or reconnecting a runner retries it without `--unblock`.",
                blocker = blocker.trim_end_matches('.'),
                run = report.cook_id,
            ),
            "details": {
                "status": report.status,
                "run_id": report.run_id,
                "repeated_step": repeated,
                "blocker": blocker,
            },
        }));
        return (report, 1);
    }
    let exit_code = i32::from(!CookStatus::from_status(&report.status).is_success_exit());
    if exit_code != 0 {
        if let CookRecoveryStep::None { reason } = &report.step {
            report.failure = Some(serde_json::json!({
                "code": "agent_task.recover_no_step",
                "message": reason,
                "details": {
                    "status": report.status,
                    "run_id": report.run_id,
                },
            }));
        }
    }
    (report, exit_code)
}

/// Status of a Cook attempt whose recipe is durable but whose lifecycle record
/// was never written; the same name `agent-task status` reports.
const RECIPE_ONLY_STATUS: &str = "recipe_only_recovery_required";

/// Whether `recover <id>` resumes `id` as a plain durable run instead of
/// planning a Cook step: a lifecycle record exists and nothing ties it to a
/// Cook (no recipe, no owning attempt recipe, no terminal unmaterialized Cook
/// admission, no delegated Cook plan). An unknown ID stays on the Cook path so
/// the planner reports it.
pub fn recovers_as_run(id: &str) -> Result<bool> {
    if super::recipe_exists(id)? || super::load_recipe_for_attempt(id)?.is_some() {
        return Ok(false);
    }
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let Ok(record) = store.read_record(id) else {
        return Ok(false);
    };
    let terminal_admission = record.state().is_terminal()
        && record.metadata["unmaterialized_cook_admission"].is_object();
    Ok(!terminal_admission && cook_recovery_delegation(id).is_none())
}

pub fn plan_cook_recovery(id: &str) -> Result<CookRecoveryPlan> {
    plan_cook_recovery_with_probe_policy(id, true)
}

/// Plan diagnosis from recorded evidence without inspecting live worktrees or capacity.
pub fn plan_cook_recovery_recorded(id: &str) -> Result<CookRecoveryPlan> {
    plan_cook_recovery_with_probe_policy(id, false)
}

fn plan_cook_recovery_with_probe_policy(id: &str, live_probes: bool) -> Result<CookRecoveryPlan> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    if let Ok(record) = store.read_record(id) {
        if agent_task_lifecycle::CookRunState::of(&record).fenced {
            return Ok(CookRecoveryPlan {
                cook_id: record.metadata["cook_id"]
                    .as_str()
                    .unwrap_or(id)
                    .to_string(),
                run_id: record.run_id,
                status: "cancelled".into(),
                step: CookRecoveryStep::none(
                    "The Cook mission was fenced; its candidate is retired.",
                ),
            });
        }
        if let Some(plan) = unmaterialized_cook_recovery_plan(&record) {
            return Ok(plan);
        }
    }
    if let Ok(index) = store.read_cook_index(id) {
        if index.cancellation_fence.is_some() {
            return Ok(CookRecoveryPlan {
                cook_id: id.to_string(),
                run_id: index.latest_run_id.clone(),
                status: "cancelled".into(),
                step: CookRecoveryStep::none(
                    "The Cook mission was fenced; its candidate is retired.",
                ),
            });
        }
    }
    plan_cook_recovery_for_cook(id, live_probes)
}

/// A Cook that failed before materializing (its admission failed) has no
/// durable recipe and no execution owner. With recorded admission inputs its
/// step relaunches it from them; once relaunched, the plan describes that
/// relaunch, so replaying `recover` converges on it. Without inputs there is
/// no step, only the guidance to re-run the original `cook` command.
fn unmaterialized_cook_recovery_plan(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<CookRecoveryPlan> {
    if record.state() != homeboy_lifecycle_contract::RunExecutionState::Failed
        || !agent_task_lifecycle::is_unmaterialized_cook_admission(record)
        || super::load_recipe(&record.run_id).is_ok()
    {
        return None;
    }
    if agent_task_lifecycle::unmaterialized_cook_admission_inputs(record).is_none() {
        return Some(CookRecoveryPlan {
            cook_id: record.run_id.clone(),
            run_id: record.run_id.clone(),
            status: agent_task_lifecycle::CookRunState::of(record).status,
            step: CookRecoveryStep::none(
                agent_task_lifecycle::unmaterialized_cook_relaunch_guidance(record),
            ),
        });
    }
    let relaunch = record.metadata["recovered_by"]["run_id"]
        .as_str()
        .and_then(|run_id| agent_task_lifecycle::exact_record(run_id).ok());
    let (run_id, status) = relaunch.map_or_else(
        || {
            (
                record.run_id.clone(),
                agent_task_lifecycle::CookRunState::of(record).status,
            )
        },
        |relaunch| {
            (
                relaunch.run_id.clone(),
                agent_task_lifecycle::CookRunState::of(&relaunch).status,
            )
        },
    );
    Some(CookRecoveryPlan {
        cook_id: record.run_id.clone(),
        run_id,
        status,
        step: CookRecoveryStep::Relaunch,
    })
}

/// Perform a [`CookRecoveryStep::Relaunch`] for the unmaterialized source Cook.
fn relaunch_unmaterialized_cook(source_id: &str) -> Result<(Value, i32)> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let (relaunch, replayed) =
        agent_task_lifecycle::relaunch_unmaterialized_cook_in_store(&store, source_id)?;
    let blocker = if replayed {
        None
    } else {
        super::ensure_scheduling_daemon()
    };
    let mut value = serde_json::json!({
        "schema": "homeboy/agent-task-cook-relaunch/v1",
        "run_id": relaunch.run_id,
        "cook_id": relaunch.run_id,
        "status": agent_task_lifecycle::CookRunState::of(&relaunch).status,
        "recovered_from": source_id,
        "replayed": replayed,
        "status_command": format!("homeboy agent-task status {} --watch", relaunch.run_id),
    });
    if let Some(blocker) = blocker {
        value["blocker"] = blocker;
    }
    Ok((value, 0))
}

fn plan_cook_recovery_for_cook(id: &str, live_probes: bool) -> Result<CookRecoveryPlan> {
    let key = resolve_cook_key(id)?;
    let recipe = super::load_recipe(&key)
        .or_else(|cook_error| super::load_recipe_for_attempt(&key)?.ok_or(cook_error))?;
    let run_id = super::resolve_cook_continuation_run_id(&key)?;
    if !agent_task_lifecycle::run_record_exists_readonly(&run_id)? {
        // The immutable recipe is durable but its lifecycle record is absent:
        // continuing the exact attempt materializes it before provider work,
        // which is what `agent-task status` names for this state.
        return Ok(CookRecoveryPlan {
            cook_id: recipe.cook_id,
            run_id,
            status: RECIPE_ONLY_STATUS.to_string(),
            step: CookRecoveryStep::Continue,
        });
    }
    let record = agent_task_lifecycle::exact_record(&run_id)?;
    let status = agent_task_lifecycle::CookRunState::of(&record).status;
    let step = if let Some(pr) = published_pull_request(&record) {
        // A recorded publication is the Cook's delivery, whatever gate or
        // continuation status the attempt last recorded: re-running gates or
        // continuing would only redo delivered work.
        CookRecoveryStep::none(format!("The Cook is published as {pr}; review it there."))
    } else if super::cook_recipe::abandoned_cook_continuation(&record) {
        CookRecoveryStep::Continue
    } else {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
        let recipe_store = super::cook_recipe::CookRecipeStore::for_lifecycle_store(&store);
        super::cook_promotion::cook_failure_context_with_probe_policy(
            &recipe_store,
            &store,
            &recipe.cook_id,
            Some(&run_id),
            &status,
            live_probes,
        )
        .and_then(|context| context.recovery_step)
        .unwrap_or_else(|| {
            CookRecoveryStep::none("The Cook recipe or its recovery context could not be read.")
        })
    };
    Ok(CookRecoveryPlan {
        cook_id: recipe.cook_id,
        run_id,
        status,
        step,
    })
}

/// The pull request a publication receipt proves the attempt is published as
/// (ready for review or as a draft).
fn published_pull_request(record: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<String> {
    let delivery = super::CookDelivery::from_run_metadata(&record.metadata)?;
    let publication = delivery.publication.as_ref().filter(|p| p.published)?;
    publication
        .pr_url
        .clone()
        .or_else(|| publication.pr_identity())
}

/// The recovery plan a verb that also serves non-Cook runs must hand to
/// [`recover_cook`] instead of deciding a Cook's next step itself.
///
/// `Some` only when `id` is a durable Cook ID (the Cook alias, not one exact
/// attempt) whose durable state names a step `recover` performs. `None` keeps
/// the calling verb's own behavior: a non-Cook run, an exact attempt ID, an
/// in-flight Cook (for example a queued admission `resume` re-arms), a Cook
/// whose state admits no step, or a Cook whose recovery plan cannot be read.
pub fn cook_recovery_delegation(id: &str) -> Option<CookRecoveryPlan> {
    if super::load_recipe(id).is_err() {
        return None;
    }
    // Replacement gates need explicit operator authorization that the
    // delegating verb never carries, so only `recover` performs them.
    plan_cook_recovery(id)
        .ok()
        .filter(|plan| !plan.step.is_none() && plan.step != CookRecoveryStep::ReplaceGates)
}

/// Perform (or, with `preflight`, only report) the one next recovery step.
///
/// Operator inputs select a replacement gate or publication
/// decision; it is refused for every other step so it can never redirect
/// recovery. `request.overrides` are explicit operator inputs that reserve a
/// successor attempt with a changed parameter; they are refused where the
/// step forbids a fresh provider attempt (a held claim, an artifact choice, a
/// replacement fork). `request.replace_gates` and `request.publish` feed the
/// replacement-gate and publication steps; supplying them selects that step
/// when the Cook's state admits it, and they never combine with each other or
/// with the continuation inputs.
///
/// With `preflight`, a continuation step also runs the continuation admission
/// checks and a publication step validates the hydrated publication, both
/// reported as `admission`; the exit code is the report's.
///
/// Recovery is bounded: observation-store lock contention re-plans and
/// retries with backoff, then reports `observation_store.busy`. A step that
/// left its durable state unchanged is recorded, so the next `recover` from
/// that state reports [`CookRecoveryStep::Blocked`] instead of looping.
pub fn prepare_admitted_recovery_continuation(run_id: &str) -> Result<()> {
    let plan = plan_cook_recovery(run_id)?;
    super::CookRecipeStore::from_current_data_root()?
        .schedule_recovery_continuation(&plan.cook_id, run_id)
}

pub fn recover_cook(
    id: &str,
    request: CookRecoveryRequest,
    performer: &dyn CookRecoveryPerformer,
) -> Result<(CookRecoveryReport, i32)> {
    with_store_contention_backoff(
        "agent-task recover",
        |outcome: &Result<(CookRecoveryReport, i32)>| match outcome {
            Ok((report, _)) => report
                .failure
                .as_ref()
                .is_some_and(|failure| failure["code"] == STORE_BUSY_CODE),
            Err(error) => is_store_contention(error),
        },
        || recover_cook_once(id, request.clone(), performer),
    )
}

fn recover_cook_once(
    id: &str,
    request: CookRecoveryRequest,
    performer: &dyn CookRecoveryPerformer,
) -> Result<(CookRecoveryReport, i32)> {
    let CookRecoveryRequest {
        preflight,
        regate,
        regate_inputs,
        unblock,
        overrides,
        mut replace_gates,
        mut corrected_gates,
        publish,
    } = request;
    // Explicit operator inputs change what the step does, so they are never
    // a repeat of the previous recovery's step.
    let explicit_inputs = regate
        || !overrides.is_empty()
        || replace_gates.is_some()
        || corrected_gates.is_some()
        || !publish.is_empty();
    if !regate && !regate_inputs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "regate",
            "replacement regate gates and gate budgets require --regate",
            Some(id.to_string()),
            Some(vec![format!(
                "homeboy agent-task recover {id} --regate --verify '<COMMAND>'"
            )]),
        ));
    }
    if unblock && !preflight {
        clear_no_progress_for(id);
    }
    if !preflight {
        if let Ok(run_id) = super::resolve_cook_continuation_run_id(id) {
            if let Ok(record) = agent_task_lifecycle::exact_record(&run_id) {
                if agent_task_lifecycle::recorded_store_contention(&record)
                    || (record.state() == homeboy_lifecycle_contract::RunExecutionState::Running
                        && !removed_runner_attempt(&run_id))
                {
                    agent_task_lifecycle::reconcile_status(&run_id)?;
                }
            }
        }
    }
    if let Some(ci) = super::cook_ci::cook_ci_status(id)? {
        if corrected_gates.is_some() {
            return Err(Error::validation_invalid_argument(
                "verify",
                "CI recovery does not accept recipe gate corrections",
                Some(id.to_string()),
                None,
            ));
        }
        let ci = if preflight {
            ci
        } else {
            super::cook_ci::repoll_cook_ci(id)?.unwrap_or(ci)
        };
        let status = ci["status"].as_str().unwrap_or("awaiting_ci").to_owned();
        let exit = i32::from(matches!(
            status.as_str(),
            "ci_gate_failed" | "ci_infrastructure"
        ));
        return Ok((
            CookRecoveryReport {
                schema: COOK_RECOVERY_SCHEMA,
                cook_id: ci["cook_id"].as_str().unwrap_or(id).to_owned(),
                run_id: id.to_owned(),
                status,
                step: CookRecoveryStep::None {
                    reason: "CI verification poll; automatic retry dispatch is not yet supported"
                        .into(),
                },
                preflight,
                overrides: None,
                regate: None,
                admission: None,
                result: Some(ci),
                failure: None,
            },
            exit,
        ));
    }
    let mut plan = plan_cook_recovery(id)?;
    if !preflight
        && plan.step == CookRecoveryStep::Reconcile
        && removed_runner_attempt(&plan.run_id)
    {
        // Reuse the scoped repair, including its authoritative refresh and
        // postcondition. Then plan from the durable runner.job_lost result so
        // the existing candidate/recipe recovery runs in this same invocation.
        let reconciliation = super::reconcile_run(&plan.run_id, false)?;
        if let Some(failure) = reconciliation.failure_summary() {
            return Err(Error::internal_unexpected(failure));
        }
        plan = plan_cook_recovery(id)?;
    }
    if regate {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
        let index_fenced = store
            .read_cook_index(&plan.cook_id)
            .is_ok_and(|index| index.cancellation_fence.is_some());
        // Regate is admitted exactly when the run state's legal actions name
        // it, the same projection `status` reads (#17976).
        let regate_legal = match agent_task_lifecycle::exact_record(&plan.run_id) {
            Ok(record) => agent_task_lifecycle::CookRunState::of(&record)
                .with_cook_index_fence(index_fenced)
                .allows_regate(),
            Err(_) => !index_fenced,
        };
        if !regate_legal || plan.step == CookRecoveryStep::Reconcile {
            return Err(Error::validation_invalid_argument(
                "regate",
                "Cook is fenced, in flight, or has a held claim",
                Some(id.into()),
                None,
            ));
        }
        if !overrides.is_empty()
            || replace_gates.is_some()
            || corrected_gates.is_some()
            || !publish.is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "regate",
                "regate cannot be combined with other recovery inputs",
                Some(id.into()),
                None,
            ));
        }
        let recipe = if super::recipe_exists(id)? {
            super::load_recipe(id)?
        } else if let Some(recipe) = super::load_recipe_for_attempt(id)? {
            recipe
        } else {
            // A Cook whose admission never materialized has no recipe: no
            // provider ran, so there is no candidate to re-gate.
            let next = match &plan.step {
                CookRecoveryStep::None { reason } => reason.clone(),
                _ => format!(
                    "recover it without --regate: `homeboy agent-task recover {}`",
                    plan.cook_id
                ),
            };
            return Err(Error::validation_invalid_argument(
                "regate",
                format!(
                    "Cook has no durable recipe because its admission never materialized, so no provider ran and there is no candidate to re-gate. {next}"
                ),
                Some(id.into()),
                None,
            ));
        };
        let candidate = recipe
            .attempts
            .iter()
            .rev()
            .find(|attempt| {
                if id != recipe.cook_id && id != attempt.run_id {
                    return false;
                }
                super::cook_promotion::persisted_promotion_for_attempt(&attempt.run_id)
                    .ok()
                    .flatten()
                    .is_some_and(|promotion| promotion.candidate.is_some())
            })
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "regate",
                    "no candidate-bearing attempt",
                    Some(id.into()),
                    None,
                )
            })?;
        plan.run_id = candidate.run_id.clone();
        let record = agent_task_lifecycle::exact_record(&plan.run_id)?;
        let candidate_state =
            agent_task_lifecycle::CookRunState::of(&record).with_cook_index_fence(index_fenced);
        if !candidate_state.allows_regate() {
            return Err(Error::validation_invalid_argument(
                "regate",
                "candidate is retired, in flight, or fenced",
                Some(plan.run_id),
                None,
            ));
        }
        plan.status = candidate_state.status;
        plan.step = CookRecoveryStep::Regate;
    }
    // Ownership wins over every operator override.
    // Report the durable claim and progress without touching admission or gates.
    let live_record = agent_task_lifecycle::exact_record(&plan.run_id)
        .ok()
        .filter(|record| {
            agent_task_lifecycle::CookRunState::of(record).phase
                == agent_task_lifecycle::CookRunPhase::Continuing
        });
    if let Some((record, owner)) = live_record
        .as_ref()
        .and_then(|record| super::live_cook_continuation(record).map(|owner| (record, owner)))
    {
        return Ok((
            CookRecoveryReport {
                schema: COOK_RECOVERY_SCHEMA,
                cook_id: plan.cook_id.clone(),
                run_id: plan.run_id,
                status: "in_flight".into(),
                step: CookRecoveryStep::none(format!(
                    "Continuation owner pid {} is in flight; homeboy agent-task status {} --watch",
                    owner["owner_pid"], plan.cook_id
                )),
                preflight,
                overrides: None,
                regate: None,
                admission: None,
                result: Some(serde_json::json!({
                    "continuation": owner,
                    "progress": record.metadata.get("cook_progress"),
                    "next_action": format!("homeboy agent-task status {} --watch", plan.cook_id),
                })),
                failure: None,
            },
            0,
        ));
    }
    // Executing corrected commands on the authenticated candidate is not an
    // import of external proof. Keep recipe corrections for pre-provider runs,
    // but route applied candidates through the single replacement predicate.
    if corrected_gates.is_some() && super::applied_candidate_awaits_verification(&plan.run_id) {
        replace_gates = Some(CookReplaceGatesRequest {
            proof: CookReplacementProof::Gates(corrected_gates.take().unwrap()),
            authorize_external_proof: "operator supplied replacement gates via recover".into(),
            authorize_interrupted_rerun: None,
        });
    }
    validate_step_inputs(&mut plan, &overrides, replace_gates.is_some(), &publish)?;
    if !overrides.is_empty() {
        validate_overrides(&plan, preflight)?;
    }
    // Keep the normal retry contract: only a terminal pre-execution failure
    // may change its recipe, and retry still reserves the successor.
    if let Some(gates) = corrected_gates {
        let record = agent_task_lifecycle::exact_record(&plan.run_id)?;
        if !overrides.is_empty()
            || replace_gates.is_some()
            || !publish.is_empty()
            || !record.state().is_terminal()
            || agent_task_lifecycle::CookRunState::of(&record).status != "pre_execution_failure"
            || plan.step != CookRecoveryStep::Retry
        {
            return Err(Error::validation_invalid_argument(
                "verify",
                "recipe gate correction requires a pre-provider failure and cannot be combined with other recovery inputs",
                Some(plan.run_id.clone()),
                Some(vec![format!("homeboy agent-task status {}", plan.run_id)]),
            ));
        }
        let store = super::CookRecipeStore::from_current_data_root()?;
        let _driver = store
            .try_acquire_cook_driver(&plan.cook_id)?
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "verify",
                    "another controller is driving this Cook",
                    Some(plan.cook_id.clone()),
                    None,
                )
            })?;
        store.correct_pre_provider_gates(&plan.cook_id, gates, preflight)?;
    }
    let report = CookRecoveryReport {
        schema: COOK_RECOVERY_SCHEMA,
        cook_id: plan.cook_id,
        run_id: plan.run_id,
        status: plan.status,
        step: plan.step,
        preflight,
        overrides: (!overrides.is_empty()).then(|| overrides.clone()),
        regate: (!regate_inputs.is_empty()).then(|| regate_inputs.clone()),
        admission: None,
        result: None,
        failure: None,
    };
    let run_id = report.run_id.clone();
    let run_id = run_id.as_str();
    if !preflight
        && (!overrides.is_empty()
            || matches!(
                report.step,
                CookRecoveryStep::Continue | CookRecoveryStep::Regate | CookRecoveryStep::Rearm
            ))
    {
        if let Some(admission) = performer.admit_step(&report, &overrides)? {
            let mut report = report;
            report.status = admission["status"].as_str().unwrap_or("admitted").into();
            report.admission = Some(admission);
            return Ok((report, 0));
        }
    }
    if !overrides.is_empty() {
        let result = performer.continue_cook(run_id, overrides.has_route_override(), &overrides);
        return Ok(finish_performed_step(report, result));
    }
    if preflight {
        if report.step == CookRecoveryStep::Publish {
            let result = performer.publish(run_id, &publish, true);
            return Ok(finish_performed_step(report, result));
        }
        if report.step == CookRecoveryStep::ReplaceGates {
            // Replacement gates execute operator shell programs; preflight
            // only proves the applied candidate admits replacement proof.
            if replace_gates.is_some() {
                super::ensure_replacement_gate_candidate(run_id)?;
            }
            return Ok((report, 0));
        }
        let continuation = match &report.step {
            CookRecoveryStep::Continue => Some(false),
            CookRecoveryStep::Rearm => Some(true),
            _ => None,
        };
        // A recipe-only attempt has no lifecycle record to admit yet: the
        // continuation materializes it, which the plan already states.
        let continuation = continuation.filter(|_| report.status != RECIPE_ONLY_STATUS);
        let Some(rearm) = continuation else {
            return Ok((report, 0));
        };
        let result = performer.preflight_continue_cook(run_id, rearm);
        return Ok(finish_performed_step(report, result));
    }
    if report.step.is_none() {
        return Ok(finish_without_step(report));
    }
    let before = (
        report.run_id.clone(),
        report.status.clone(),
        report.step.clone(),
    );
    let none = CookRecoveryOverrides::default();
    let result = (|| {
        Ok(match &report.step {
            CookRecoveryStep::Continue => {
                // The step names remaining work, so its continuation must be
                // claimable before the handler runs; otherwise the handler
                // finds nothing to consume and reports a no-op (#17494).
                super::CookRecipeStore::from_current_data_root()?
                    .schedule_recovery_continuation(&report.cook_id, run_id)?;
                performer.continue_cook(run_id, false, &none)?
            }
            CookRecoveryStep::Regate => performer.regate(run_id, &regate_inputs)?,
            CookRecoveryStep::Rearm => performer.continue_cook(run_id, true, &none)?,
            CookRecoveryStep::Retry | CookRecoveryStep::ForkReplacement => {
                // A fresh provider attempt never starts from a pre-patched
                // destination (#16292).
                super::cook_attempt_workspace::prepare_fresh_attempt_workspace_for_cook(
                    &report.cook_id,
                    run_id,
                )?;
                performer.retry(run_id)?
            }
            CookRecoveryStep::Reconcile => performer.reconcile(run_id)?,
            CookRecoveryStep::Adopt { candidate_ref } => performer.adopt(run_id, candidate_ref)?,
            CookRecoveryStep::Relaunch => relaunch_unmaterialized_cook(&report.cook_id)?,
            CookRecoveryStep::ReplaceGates => {
                let Some(replace_gates) = replace_gates else {
                    return Err(replace_gates_inputs_required(run_id));
                };
                let (mut result, exit_code) = performer.replace_gates(run_id, replace_gates)?;
                // Authorized proof that makes the candidate publishable completes
                // the Cook here, as `regate` does, instead of handing the operator
                // a second `recover` for the publish it already authorized (#17691).
                // The planner's own verdict decides, so this never publishes
                // what a fresh `recover` would not.
                if exit_code == 0 && plan_cook_recovery(id)?.step == CookRecoveryStep::Publish {
                    let (mut published, exit_code) = performer.publish(run_id, &publish, false)?;
                    if let Some(object) = published.as_object_mut() {
                        object.insert("replacement_gates".to_string(), result);
                    }
                    return Ok((published, exit_code));
                }
                if let Some(object) = result.as_object_mut() {
                    object.insert(
                    "handoff".to_string(),
                    serde_json::json!({
                        "next_command": super::cook_recover_command(None, run_id),
                        "status_command": super::cook_recovery_command(run_id, &["status", run_id]),
                    }),
                );
                }
                (result, exit_code)
            }
            CookRecoveryStep::Publish => performer.publish(run_id, &publish, false)?,
            CookRecoveryStep::Blocked { .. } | CookRecoveryStep::None { .. } => {
                unreachable!("no-step handled before performing")
            }
        })
    })();
    let (report, exit_code) = finish_performed_step(report, result);
    if !explicit_inputs {
        record_recovery_progress(id, before, &report);
    }
    Ok((report, exit_code))
}

fn finish_performed_step(
    mut report: CookRecoveryReport,
    result: Result<(Value, i32)>,
) -> (CookRecoveryReport, i32) {
    match result {
        Ok((value, exit_code)) => {
            if exit_code != 0 {
                report.failure = performed_step_failure(&value).or_else(|| {
                    Some(serde_json::json!({
                        "code": "agent_task.recover_incomplete",
                        "message": format!("Recovery did not complete (status: {}). Inspect the persisted run before recovering again.", report.status),
                        "next_command": format!("homeboy agent-task status {}", report.run_id),
                    }))
                });
            }
            if report.preflight {
                report.admission = Some(value);
            } else {
                // Continuations and retries may start or adopt a successor. The
                // report describes that attempt, not the recovery's source.
                if let Some(run_id) = value
                    .get("latest_run_id")
                    .or_else(|| value.get("run_id"))
                    .or_else(|| value.pointer("/run/id"))
                    .and_then(Value::as_str)
                {
                    report.run_id = run_id.to_string();
                }
                if let Some(status) = value
                    .get("status")
                    .or_else(|| value.get("state"))
                    .or_else(|| value.pointer("/run/state"))
                    .and_then(Value::as_str)
                {
                    report.status = status.to_string();
                }
                report.result = Some(value);
            }
            (report, exit_code)
        }
        Err(error) => {
            let code = if is_store_contention(&error) {
                STORE_BUSY_CODE
            } else {
                error.code.as_str()
            };
            report.failure = Some(serde_json::json!({
                "code": code,
                "message": error.message,
                "details": error.details,
            }));
            (report, 1)
        }
    }
}

/// Preserve the concrete cause returned by retries as well as continuations.
/// Retry waits return a status-watch envelope, not a phase report.
fn performed_step_failure(value: &Value) -> Option<Value> {
    if let Some(failure) = blocked_phase_failure(value) {
        return Some(failure);
    }
    for pointer in [
        "/failure",
        "/terminal_summary/blocker",
        "/latest/blocker",
        "/blocker",
    ] {
        if let Some(failure) = value.pointer(pointer).filter(|failure| {
            failure["message"]
                .as_str()
                .is_some_and(|message| !message.trim().is_empty())
        }) {
            return Some(failure.clone());
        }
    }
    value["stop_reason"]
        .as_str()
        .filter(|reason| !reason.trim().is_empty())
        .map(|reason| {
            serde_json::json!({
                "code": "agent_task.recover_failed",
                "message": reason,
                "details": { "status": value["status"] },
            })
        })
}

/// A refused step reports its blocking phase deep in its payload. Lift that
/// phase's message as the recovery's declared cause, so the command summary
/// states why recovery stopped.
fn blocked_phase_failure(value: &Value) -> Option<Value> {
    let phase = value["phases"]
        .as_array()?
        .iter()
        .rev()
        .find(|phase| phase["status"] == "blocked")?;
    let message = phase["message"]
        .as_str()
        .filter(|message| !message.trim().is_empty())?;
    let name = phase["phase"].as_str().unwrap_or("admission");
    Some(serde_json::json!({
        "code": "agent_task.recover_blocked",
        "message": format!("recover blocked at `{name}`: {message}"),
        "details": { "phase": name, "reason": phase["reason"] },
    }))
}

fn replace_gates_inputs_required(run_id: &str) -> Error {
    Error::validation_invalid_argument(
        "replace-gates",
        "the candidate failed its recorded gates: re-run the same gates with --regate (for example after a flaky test), or replace them with explicit operator inputs: --authorize-external-proof <TEXT> with corrected gates (--verify/--verify-file) or candidate-bound proof (--promotion @FILE)",
        Some(run_id.to_string()),
        Some(vec![
            format!("homeboy agent-task recover {run_id} --regate"),
            format!(
                "homeboy agent-task recover {run_id} --authorize-external-proof <AUTHORIZATION> --verify '<CANDIDATE-BOUND COMMAND>'"
            ),
            format!(
                "homeboy agent-task recover {run_id} --authorize-external-proof <AUTHORIZATION> --promotion @replacement.json"
            ),
        ]),
    )
}

/// Replacement-gate and publication inputs select the step they feed when the
/// decided step is a plain continuation or no step at all. Any step that must
/// come first (a held claim, a fork, an artifact choice, a retry) refuses them,
/// and they never combine with each other or with continuation inputs.
fn validate_step_inputs(
    plan: &mut CookRecoveryPlan,
    overrides: &CookRecoveryOverrides,
    replace_gates: bool,
    publish: &CookPublishRequest,
) -> Result<()> {
    let publish = !publish.is_empty();
    if !replace_gates && !publish {
        return Ok(());
    }
    let refuse = |field: &str, message: String| {
        Err(Error::validation_invalid_argument(
            field,
            message,
            Some(plan.run_id.clone()),
            Some(vec![format!("homeboy agent-task recover {}", plan.run_id)]),
        ))
    };
    if replace_gates && publish {
        return refuse(
            "recovery-step",
            "replacement-gate inputs and publication inputs select different steps; supply one set"
                .to_string(),
        );
    }
    if !overrides.is_empty() {
        return refuse(
            "recovery-step",
            "replacement-gate and publication inputs cannot be combined with continuation overrides".to_string(),
        );
    }
    let (field, selected) = if replace_gates {
        ("replace-gates", CookRecoveryStep::ReplaceGates)
    } else {
        ("publish", CookRecoveryStep::Publish)
    };
    if agent_task_lifecycle::exact_record(&plan.run_id).is_ok_and(|record| {
        !agent_task_lifecycle::CookRunState::of(&record)
            .allows(homeboy_control_plane_contract::ControlPlaneAction::Recover)
    }) {
        return refuse(
            field,
            format!(
                "the Cook is still in flight; watch it with `homeboy agent-task status {} --watch` instead",
                plan.run_id
            ),
        );
    }
    match &plan.step {
        step if *step == selected => Ok(()),
        CookRecoveryStep::Continue
        | CookRecoveryStep::Rearm
        | CookRecoveryStep::Regate
        | CookRecoveryStep::Blocked { .. }
        | CookRecoveryStep::None { .. } => {
            plan.step = selected;
            Ok(())
        }
        other => refuse(
            field,
            format!(
                "the Cook's recovery step is `{}`; run `agent-task recover {}` without these inputs first",
                other.action_name().unwrap_or("none"),
                plan.run_id
            ),
        ),
    }
}

/// Overrides reserve a fresh provider attempt. They never combine with a
/// preflight or an artifact selection, and a step that forbids a fresh attempt
/// refuses them rather than silently ignoring them.
fn validate_overrides(plan: &CookRecoveryPlan, preflight: bool) -> Result<()> {
    let refuse = |message: &str| {
        Err(Error::validation_invalid_argument(
            "recovery-override",
            message.to_string(),
            Some(plan.run_id.clone()),
            Some(vec![format!("homeboy agent-task recover {}", plan.run_id)]),
        ))
    };
    if preflight {
        return refuse("timeout and provider-route overrides cannot be combined with --preflight");
    }
    match &plan.step {
        CookRecoveryStep::Regate => refuse("regate reserves no provider attempt to override"),
        CookRecoveryStep::Reconcile => refuse(
            "a promotion claim is still held; run `agent-task recover` without overrides to reconcile it first",
        ),
        CookRecoveryStep::Adopt { .. } => refuse(
            "the recovery step adopts the worktree's committed candidate; it reserves no provider attempt to override",
        ),
        CookRecoveryStep::ForkReplacement => refuse(
            "the recovery step forks a replacement attempt that preserves the checkpoint; it accepts no overrides",
        ),
        CookRecoveryStep::Relaunch => refuse(
            "the recovery step relaunches the Cook from its recorded admission inputs; it accepts no overrides",
        ),
        _ => Ok(()),
    }
}

/// The previous `recover` performed `step` from this attempt and status and
/// left both unchanged (#17650). The marker lives beside the Cook recipe; the
/// planner reports [`CookRecoveryStep::Blocked`] while it still matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CookRecoveryNoProgress {
    pub run_id: String,
    pub status: String,
    pub step: String,
    pub blocker: String,
    /// The environment the step ran in, from [`recovery_environment`].
    #[serde(default)]
    pub environment: String,
}

/// The execution environment a recovery step depends on beyond the Cook's own
/// durable state: this controller's build and each runner session it holds
/// (the runner's build and when it connected). Upgrading the controller, or
/// refreshing or reconnecting a runner, changes it, so a step blocked by the
/// old environment is performed again without `--unblock` (#17873).
fn recovery_environment() -> String {
    let mut facts = vec![format!(
        "controller {}",
        homeboy_core::build_identity::current().display
    )];
    let sessions = homeboy_core::paths::runner_sessions_dir()
        .ok()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|runner| std::fs::read_dir(runner.path()).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("controller-") && name.ends_with(".json"))
        });
    let mut runners: Vec<String> = sessions
        .filter_map(|path| {
            let session: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            Some(format!(
                "{} {} connected {}",
                session["runner_id"].as_str()?,
                session["homeboy_build_identity"]
                    .as_str()
                    .unwrap_or("unknown build"),
                session["connected_at"].as_str().unwrap_or("unknown"),
            ))
        })
        .collect();
    runners.sort();
    facts.extend(runners);
    facts.join("; ")
}

const NO_PROGRESS_MARKER: &str = "recover-no-progress.json";

/// The no-progress marker of `cook_id` when it still describes `record`.
pub(crate) fn matching_no_progress(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
) -> Option<CookRecoveryNoProgress> {
    let record = record?;
    let path = lifecycle_store.cook_dir(cook_id).join(NO_PROGRESS_MARKER);
    let marker: CookRecoveryNoProgress = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (marker.run_id == record.run_id
        && marker.status == agent_task_lifecycle::CookRunState::of(record).status
        && marker.environment == recovery_environment())
    .then_some(marker)
}

fn no_progress_path(id: &str) -> Option<std::path::PathBuf> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().ok()?;
    let key = resolve_cook_key(id).ok()?;
    let recipe = super::load_recipe(&key)
        .ok()
        .or_else(|| super::load_recipe_for_attempt(&key).ok().flatten())?;
    Some(store.cook_dir(&recipe.cook_id).join(NO_PROGRESS_MARKER))
}

fn clear_no_progress_for(id: &str) {
    if let Some(path) = no_progress_path(id) {
        let _ = std::fs::remove_file(path);
    }
}

/// Compare the Cook's plan after a performed step with the plan it was
/// performed from. An unchanged attempt, status and step is no progress, and
/// is recorded so the planner blocks the repeat; anything else clears it.
fn record_recovery_progress(
    id: &str,
    (run_id, status, step): (String, String, CookRecoveryStep),
    report: &CookRecoveryReport,
) {
    // Contention is transient, not evidence that the step cannot progress.
    if report
        .failure
        .as_ref()
        .is_some_and(|failure| failure["code"] == STORE_BUSY_CODE)
    {
        return;
    }
    let Some(path) = no_progress_path(id) else {
        return;
    };
    let Some(action) = step.action_name() else {
        return;
    };
    let after = plan_cook_recovery(id).ok();
    let repeated = after.as_ref().is_some_and(|after| {
        after.run_id == run_id && after.status == status && after.step.action_name() == Some(action)
    });
    if !repeated {
        let _ = std::fs::remove_file(path);
        return;
    }
    let blocker = report
        .failure
        .as_ref()
        .and_then(|failure| failure["message"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "`{action}` returned `{}` and left the Cook at `{status}`",
                report.status
            )
        });
    let marker = CookRecoveryNoProgress {
        run_id,
        status,
        step: action.to_string(),
        blocker,
        environment: recovery_environment(),
    };
    if let (Some(parent), Ok(bytes)) = (path.parent(), serde_json::to_vec_pretty(&marker)) {
        let staged = path.with_extension("json.tmp");
        let _ = std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::write(&staged, bytes))
            .and_then(|()| std::fs::rename(&staged, &path));
    }
}

/// Error code of the typed `store_busy` outcome of a bounded recovery or retry.
pub const STORE_BUSY_CODE: &str = "observation_store.busy";
const STORE_CONTENTION_ATTEMPTS: u32 = 5;
const STORE_CONTENTION_BASE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether `error` is observation-store lock contention: the typed busy
/// error, or a SQLite write whose contention budget ran out.
pub fn is_store_contention(error: &Error) -> bool {
    error.code == homeboy_core::ErrorCode::ObservationStoreBusy
        || is_store_contention_message(&error.message)
}

/// Whether a recorded failure message reports observation-store lock contention.
pub fn is_store_contention_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database is busy")
        || message.contains("writer contention budget exhausted")
}

/// Run `attempt` until `busy` no longer reports contention, sleeping with
/// exponential backoff between a bounded number of attempts. Contention that
/// outlasts the budget is returned as the typed `observation_store.busy`.
pub fn with_store_contention_backoff<T>(
    operation: &str,
    busy: impl Fn(&Result<T>) -> bool,
    mut attempt: impl FnMut() -> Result<T>,
) -> Result<T> {
    let started = std::time::Instant::now();
    let mut backoff = STORE_CONTENTION_BASE_BACKOFF;
    let mut attempts = 1;
    loop {
        let outcome = attempt();
        if attempts >= STORE_CONTENTION_ATTEMPTS || !busy(&outcome) {
            return match outcome {
                Err(error) if is_store_contention(&error) => {
                    let path = error.details["path"]
                        .as_str()
                        .unwrap_or("observation store")
                        .to_string();
                    Err(Error::observation_store_busy(
                        path,
                        operation,
                        started.elapsed().as_millis() as u64,
                    )
                    .with_hint(format!(
                        "`{operation}` gave up after {attempts} attempts behind another observation-store writer: {}",
                        error.message
                    )))
                }
                outcome => outcome,
            };
        }
        std::thread::sleep(backoff);
        backoff = backoff.saturating_mul(2);
        attempts += 1;
    }
}

/// A batch-child run is recorded with the Cook it belongs to; every other
/// identifier already names a Cook or one of its attempts.
fn resolve_cook_key(id: &str) -> Result<String> {
    if super::load_recipe(id).is_ok() || super::load_recipe_for_attempt(id)?.is_some() {
        return Ok(id.to_string());
    }
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let resolved = agent_task_lifecycle::resolve_cook_reader_run_id_in_store(&store, id)?;
    if let Some(recipe) = super::load_recipe_for_attempt(&resolved)? {
        return Ok(recipe.cook_id);
    }
    agent_task_lifecycle::exact_record(&resolved)
        .ok()
        .and_then(|record| {
            record
                .metadata
                .get("cook_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "id",
                "no durable Cook, Cook attempt or batch-child run has this ID",
                Some(id.to_string()),
                None,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step<'a>(
        status: &'a str,
        configure: impl FnOnce(&mut CookRecoveryInputs<'a>),
    ) -> CookRecoveryStep {
        let mut inputs = CookRecoveryInputs {
            status,
            recovery_legal: true,
            ..Default::default()
        };
        configure(&mut inputs);
        cook_recovery_step(&inputs)
    }

    #[test]
    fn failed_terminal_continuation_rearms_instead_of_retrying_provider() {
        for status in ["failed", "pre_execution_failure"] {
            assert_eq!(
                step(status, |inputs| {
                    inputs.continuation_rearm_required = true;
                    inputs.retry_refusal = Some("provider already succeeded");
                }),
                CookRecoveryStep::Rearm
            );
        }
    }

    #[test]
    fn refused_continuation_retries_only_without_a_provider_outcome() {
        for (missing, admitted, expected) in [
            (true, true, CookRecoveryStep::Retry),
            (true, false, CookRecoveryStep::none(
                "No provider candidate exists; retry admission refused this attempt: retry budget exhausted. Inspect it with `homeboy agent-task diagnose attempt`.",
            )),
            (false, true, CookRecoveryStep::none(
                "Continuation cannot admit this attempt's candidate: candidate admission refused. Inspect it with `homeboy agent-task diagnose attempt`.",
            )),
        ] {
            assert_eq!(step("failed", |inputs| {
                inputs.run_id = "attempt";
                inputs.provider_outcome_missing = missing;
                inputs.retry_admitted = admitted;
                inputs.retry_refusal = (!admitted).then_some("retry budget exhausted");
                inputs.continuation_refusal = Some("candidate admission refused");
            }), expected);
        }
    }

    #[test]
    fn observer_failure_can_continue_while_operator_cancel_has_no_recovery_step() {
        assert_eq!(step("failed", |_| {}), CookRecoveryStep::Continue);
        assert_eq!(
            step("candidate_recoverable", |_| {}),
            CookRecoveryStep::Continue
        );
        assert!(matches!(
            step("cancelled", |_| {}),
            CookRecoveryStep::None { .. }
        ));
    }

    #[test]
    fn recover_publication_failure_never_forks_a_green_applied_candidate() {
        assert_eq!(
            step("durable_failure", |inputs| {
                inputs.verified_unpublished = true;
                inputs.retry_admitted = true;
                inputs.exact_checkpoint_candidate_mismatch = true;
                inputs.continuation_rearm_required = true;
            }),
            CookRecoveryStep::Publish
        );
        assert_eq!(
            step("gate_failed", |inputs| {
                inputs.applied_gate_failure = true;
                inputs.retry_admitted = true;
            }),
            CookRecoveryStep::Continue,
            "gate remediation retries through the recorded Cook continuation"
        );
    }

    #[test]
    fn agent_reported_incomplete_candidate_has_no_continuation_step() {
        let reason = "the PR could not be opened";
        let decided = step("candidate_recoverable", |inputs| {
            inputs.run_id = "cook-attempt-1";
            inputs.agent_incomplete_reason = Some(reason);
        });
        let (report, exit) = finish_without_step(CookRecoveryReport {
            schema: COOK_RECOVERY_SCHEMA,
            cook_id: "cook".into(),
            run_id: "cook-attempt-1".into(),
            status: "candidate_recoverable".into(),
            step: decided.clone(),
            preflight: false,
            overrides: None,
            regate: None,
            admission: None,
            result: None,
            failure: None,
        });
        assert_eq!(exit, 1);
        let failure = report.failure.unwrap();
        assert!(failure["message"]
            .as_str()
            .unwrap()
            .contains(&incomplete_review_recovery_command("cook-attempt-1")));
        let CookRecoveryStep::None { reason: message } = decided else {
            panic!(
                "an agent-declared incomplete candidate must not advertise continuation: {decided:?}"
            );
        };
        assert!(message.contains(reason), "{message}");
        assert!(
            message.contains(&incomplete_review_recovery_command("cook-attempt-1")),
            "{message}"
        );
        assert_eq!(
            step("candidate_recoverable", |_| {}),
            CookRecoveryStep::Continue,
            "other recoverable candidates still continue"
        );
        // Every continuation re-reads the verdict; steps that replace the
        // gates or start a fresh attempt do not.
        for (status, configure, expected) in [
            (
                "gate_environment_unavailable",
                (|i: &mut CookRecoveryInputs| i.continuation_rearm_required = true)
                    as fn(&mut CookRecoveryInputs),
                None,
            ),
            ("succeeded", |i| i.continuation_rearm_required = true, None),
            (
                "gate_failed",
                |i| i.applied_gate_failure = true,
                Some(CookRecoveryStep::Regate),
            ),
            (
                "pre_execution_failure",
                |i| i.retry_admitted = true,
                Some(CookRecoveryStep::Retry),
            ),
        ] {
            let decided = step(status, |inputs| {
                inputs.agent_incomplete_reason = Some(reason);
                configure(inputs);
            });
            match expected {
                Some(expected) => assert_eq!(decided, expected, "{status}"),
                None => assert!(decided.is_none(), "{status}: {decided:?}"),
            }
        }
    }

    #[test]
    fn incomplete_candidate_without_passing_gates_offers_regate() {
        // Unjudged or failed gates on a recoverable candidate: regate first,
        // even though the agent's review form declares the task incomplete.
        assert_eq!(
            step("candidate_recoverable", |inputs| {
                inputs.agent_incomplete_reason = Some("gates never ran");
                inputs.applied_gate_failure = true;
            }),
            CookRecoveryStep::Regate
        );
        // Once the gates pass, the incomplete verdict still stops publication.
        assert!(step("candidate_recoverable", |inputs| {
            inputs.agent_incomplete_reason = Some("gates never ran");
            inputs.verified_unpublished = true;
        })
        .is_none());
        let replacement = CookRegateInputs {
            gates: Some(crate::agent_task_gate::VerifyGateOptions {
                verify: vec!["test -f fixed.txt".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let executed = replacement.apply(crate::agent_task_gate::VerifyGateOptions {
            verify: vec!["cargo test missing".into()],
            gate_timeout_seconds: 42,
            gate_no_progress_timeout_seconds: 7,
            ..Default::default()
        });
        assert_eq!(executed.verify, vec!["test -f fixed.txt".to_string()]);
        assert_eq!(executed.gate_timeout_seconds, 42, "recorded timeout kept");
        assert_eq!(executed.gate_no_progress_timeout_seconds, 7);
        assert!(replacement.authorization().contains("`test -f fixed.txt`"));
        // Budget overrides replace the recorded limits and are named as
        // operator authority in the recorded evidence.
        let budgets = CookRegateInputs {
            gate_timeout_seconds: Some(1800),
            gate_no_progress_timeout_seconds: Some(0),
            ..Default::default()
        };
        let executed = budgets.apply(crate::agent_task_gate::VerifyGateOptions {
            gate_timeout_seconds: 42,
            gate_no_progress_timeout_seconds: 7,
            ..Default::default()
        });
        assert_eq!(executed.gate_timeout_seconds, 1800);
        assert_eq!(executed.gate_no_progress_timeout_seconds, 0);
        let authorization = budgets.authorization();
        assert!(
            authorization.contains("operator-authorized gate timeout 1800s"),
            "{authorization}"
        );
        assert!(
            authorization.contains("no-progress limit disabled"),
            "{authorization}"
        );
    }

    #[test]
    fn verified_candidate_without_review_form_publishes_with_operator_form() {
        // The review form is publication input: a verified candidate without
        // a usable one publishes, never rearms or replays provider work.
        for status in ["candidate_recoverable", "durable_failure"] {
            assert_eq!(
                step(status, |inputs| {
                    inputs.verified_unpublished = true;
                    inputs.continuation_completed = true;
                    inputs.continuation_rearm_required = true;
                }),
                CookRecoveryStep::Publish,
                "{status}"
            );
        }
        assert!(step("candidate_recoverable", |inputs| {
            inputs.continuation_completed = true;
        })
        .is_none());

        let mut plan = CookRecoveryPlan {
            cook_id: "cook".to_string(),
            run_id: "run-1".to_string(),
            status: "candidate_recoverable".to_string(),
            step: CookRecoveryStep::none("no step"),
        };
        let supplied = super::super::AgentTaskSuppliedReviewForm {
            form: serde_json::from_value(serde_json::json!({
                "pr_title": "Recover verified work",
                "summary": "The verified candidate completes the task.",
                "what_changed": ["Preserve verified work"],
                "compatibility": "Additive",
                "used_for": "Inspected the candidate",
                "completion": "complete"
            }))
            .unwrap(),
            tool: "pi".to_string(),
            model: "test-model".to_string(),
            operator: "operator".to_string(),
        };
        validate_step_inputs(
            &mut plan,
            &CookRecoveryOverrides::default(),
            false,
            &CookPublishRequest {
                review_form: Some(supplied),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.step, CookRecoveryStep::Publish);
    }

    #[test]
    fn recovery_publishes_gate_green_candidate_whether_or_not_its_review_form_exists() {
        // #17671: a missing review form publishes a draft rather than running
        // a review-only provider step; it never replays provider work.
        for status in [
            "candidate_recoverable",
            "durable_failure",
            "retries_exhausted",
        ] {
            let decided = step(status, |inputs| {
                inputs.verified_unpublished = true;
                inputs.retry_admitted = true;
                inputs.exact_checkpoint_candidate_mismatch = true;
            });
            assert_eq!(decided, CookRecoveryStep::Publish, "{status}");
        }
    }

    #[test]
    fn no_changes_with_commits_ahead_adopts_the_worktree_head() {
        assert_eq!(
            step("no_changes", |inputs| inputs.adoptable_candidate_ref =
                Some("abc123")),
            CookRecoveryStep::Adopt {
                candidate_ref: "abc123".to_string()
            }
        );
        assert_eq!(
            step("no_changes", |_| {}),
            CookRecoveryStep::Continue,
            "no_changes with nothing ahead of base keeps today's recovery"
        );
        assert_ne!(
            step("gate_failed", |inputs| {
                inputs.retry_admitted = true;
                inputs.adoptable_candidate_ref = Some("abc123");
            }),
            CookRecoveryStep::Adopt {
                candidate_ref: "abc123".to_string()
            },
            "only a no_changes attempt adopts"
        );
    }

    #[test]
    fn head_ahead_of_base_requires_a_strict_descendant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git");
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8(output.stdout)
                .expect("utf8")
                .trim()
                .to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        assert_eq!(
            super::super::cook_promotion::head_ahead_of_base(dir.path(), &base),
            None,
            "HEAD equal to base is not ahead"
        );
        git(&["commit", "-q", "--allow-empty", "-m", "fix"]);
        let head = git(&["rev-parse", "HEAD"]);
        assert_eq!(
            super::super::cook_promotion::head_ahead_of_base(dir.path(), &base),
            Some(head)
        );
        assert_eq!(
            super::super::cook_promotion::head_ahead_of_base(
                dir.path(),
                "0000000000000000000000000000000000000000"
            ),
            None,
            "an unknown base is not an ancestor"
        );
    }

    #[test]
    fn step_decision_covers_every_variant() {
        assert_eq!(
            step("gate_environment_unavailable", |inputs| {
                inputs.retry_admitted = false;
                inputs.continuation_rearm_required = true;
            }),
            CookRecoveryStep::Rearm,
            "infrastructure recovery does not require provider retry budget"
        );
        assert_eq!(
            step("verification_pending", |_| {}),
            CookRecoveryStep::Continue
        );
        assert_eq!(
            step("finalization_failed", |_| {}),
            CookRecoveryStep::Continue
        );
        assert_eq!(
            step("gate_failed", |inputs| inputs.retry_admitted = true),
            CookRecoveryStep::Continue
        );
        assert_eq!(
            step("verification_pending", |inputs| {
                inputs.continuation_rearm_required = true
            }),
            CookRecoveryStep::Rearm
        );
        assert_eq!(
            step("pre_execution_failure", |inputs| inputs.retry_admitted =
                true),
            CookRecoveryStep::Retry
        );
        assert_eq!(
            step("pre_execution_failure", |inputs| {
                inputs.retry_admitted = true;
                inputs.applied_gate_failure = true;
                inputs.continuation_rearm_required = true;
            }),
            CookRecoveryStep::Rearm
        );
        assert_eq!(
            step("verification_pending", |inputs| {
                inputs.retry_admitted = true;
                inputs.exact_checkpoint_candidate_mismatch = true;
            }),
            CookRecoveryStep::ForkReplacement
        );
        assert_eq!(
            step("verification_pending", |inputs| inputs.blocking_claim =
                true),
            CookRecoveryStep::Reconcile
        );
        for status in [
            "review_ready",
            "cancelled",
            "execution_budget_exhausted",
            "pre_execution_failure",
            "gate_failed",
        ] {
            assert!(step(status, |_| {}).is_none(), "{status}");
        }
        // In flight is the run state's verdict, not the label's.
        let continuing = lifecycle_record(
            homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable,
            serde_json::json!({
                "cook_continuation": { "state": "claimed", "owner_pid": std::process::id() },
            }),
        );
        assert!(step("in_flight", |inputs| {
            inputs.run_state = Some(agent_task_lifecycle::CookRunState::of(&continuing));
        })
        .is_none());
        assert!(
            step("verification_pending", |inputs| inputs.recovery_legal =
                false)
            .is_none()
        );
    }

    #[test]
    fn applied_gate_failure_without_continuation_regates() {
        // #18556: a gate-failed candidate is recoverable, so plain `recover`
        // re-gates it instead of naming no step or demanding replacement
        // proof authorization.
        assert_eq!(
            step("gate_failed", |inputs| inputs.applied_gate_failure = true),
            CookRecoveryStep::Regate
        );
        assert_eq!(
            step("candidate_recoverable", |inputs| {
                inputs.applied_gate_failure = true;
                inputs.continuation_completed = true;
            }),
            CookRecoveryStep::Regate,
            "a completed continuation cannot re-verify the candidate; regate can"
        );
        assert_eq!(
            step("candidate_recoverable", |inputs| {
                inputs.applied_gate_failure = true;
                inputs.continuation_refusal = Some("candidate fingerprint mismatch");
            }),
            CookRecoveryStep::Regate
        );
        assert_eq!(
            step("gate_failed", |inputs| {
                inputs.applied_gate_failure = true;
                inputs.retry_admitted = true;
            }),
            CookRecoveryStep::Continue,
            "an admitted retry still continues the Cook"
        );
        assert!(
            step("review_ready", |inputs| inputs.applied_gate_failure = true).is_none(),
            "only a gate failure takes replacement gates"
        );
        assert_eq!(
            step("gate_failed", |inputs| {
                inputs.applied_gate_failure = true;
                inputs.blocking_claim = true;
            }),
            CookRecoveryStep::Reconcile,
            "a running promotion claim is reconciled first"
        );
    }

    #[test]
    fn verified_unpublished_candidate_publishes_and_settled_cooks_do_not() {
        for status in [
            "verification_pending",
            "finalization_failed",
            "candidate_recoverable",
            "pre_execution_failure",
        ] {
            assert_eq!(
                step(status, |inputs| {
                    inputs.verified_unpublished = true;
                    inputs.retry_admitted = true;
                }),
                CookRecoveryStep::Publish,
                "{status}"
            );
        }
        for status in ["review_ready", "cancelled", "green_no_finalize"] {
            assert_ne!(
                step(status, |inputs| inputs.verified_unpublished = true),
                CookRecoveryStep::Publish,
                "{status} is already settled"
            );
        }
        // Replacement gates can leave a green candidate under a gate-failure
        // status whose continuation still needs rearming (#16772).
        for status in ["candidate_recoverable", "baseline_red"] {
            assert_eq!(
                step(status, |inputs| {
                    inputs.verified_unpublished = true;
                    inputs.continuation_rearm_required = true;
                    inputs.agent_incomplete_reason = Some("not done");
                })
                .action_name(),
                None,
                "{status}: an agent-declared incomplete form is neither published nor replayed"
            );
        }
    }

    fn no_step_report(status: &str, reason: &str) -> CookRecoveryReport {
        CookRecoveryReport {
            schema: COOK_RECOVERY_SCHEMA,
            cook_id: "cook-1".to_string(),
            run_id: "run-1".to_string(),
            status: status.to_string(),
            step: CookRecoveryStep::none(reason),
            preflight: false,
            overrides: None,
            regate: None,
            admission: None,
            result: None,
            failure: None,
        }
    }

    #[test]
    fn no_step_on_a_failing_status_declares_its_reason_as_the_failure() {
        let reason = "Cook status `gate_failed` admits no recovery step; inspect it with `agent-task status` or `agent-task diagnose`.";
        let (report, exit_code) = finish_without_step(no_step_report("gate_failed", reason));

        assert_eq!(exit_code, 1);
        let value = serde_json::to_value(&report).expect("serialize report");
        assert_eq!(value["failure"]["code"], "agent_task.recover_no_step");
        assert_eq!(value["failure"]["message"], reason);
        assert_eq!(value["failure"]["details"]["status"], "gate_failed");
        assert_eq!(value["failure"]["details"]["run_id"], "run-1");
    }

    #[test]
    fn refused_performed_step_reports_the_error_as_its_failure_cause() {
        let message = "cannot be claimed because its continuation is Completed";
        let mut report = no_step_report("candidate_recoverable", "");
        report.step = CookRecoveryStep::Continue;
        let error = Error::validation_invalid_argument("run_id", message, None, None);
        let (report, exit_code) = finish_performed_step(report, Err(error));
        assert_eq!(exit_code, 1);
        assert!(report.failure.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains(message));
        assert!(report.result.is_none());
    }

    #[test]
    fn retry_pre_execution_failure_preserves_concrete_cause() {
        let cause = "Cook cannot attribute uncommitted destination changes: M source.rs; stash changes before retrying";
        for preflight in [false, true] {
            for payload in [
                serde_json::json!({
                    "status": "pre_execution_failure",
                    "stop_reason": cause,
                }),
                serde_json::json!({
                    "terminal_summary": {
                        "state": "pre_execution_failure",
                        "blocker": { "code": "invalid_input", "message": cause },
                    },
                }),
            ] {
                let mut report = no_step_report("pre_execution_failure", "");
                report.step = CookRecoveryStep::Retry;
                report.preflight = preflight;
                let (report, exit_code) = finish_performed_step(report, Ok((payload.clone(), 1)));
                assert_eq!(exit_code, 1);
                assert_eq!(report.failure.unwrap()["message"], cause);
                assert_eq!(
                    if preflight {
                        report.admission
                    } else {
                        report.result
                    },
                    Some(payload)
                );
            }
        }
    }

    #[test]
    fn recovery_reports_the_started_attempt() {
        let mut report = no_step_report("candidate_recoverable", "fixture");
        report.step = CookRecoveryStep::Continue;
        let (report, code) = finish_performed_step(
            report,
            Ok((
                serde_json::json!({
                    "latest_run_id": "cook-attempt-2", "status": "running"
                }),
                0,
            )),
        );
        assert_eq!(code, 0);
        assert_eq!(report.run_id, "cook-attempt-2");
        assert_eq!(report.status, "running");
        assert_eq!(report.step, CookRecoveryStep::Continue);
    }

    /// #17650: lock contention re-runs the operation with backoff a bounded
    /// number of times, then reports the typed `observation_store.busy`.
    #[test]
    fn cook_recovery_store_contention_is_bounded_and_typed() {
        let locked = || {
            Error::internal_unexpected(
                "SQLite observation store error: upsert imported run with canonical events (writer contention budget exhausted): database is locked",
            )
        };
        let busy =
            |outcome: &Result<u32>| matches!(outcome, Err(error) if is_store_contention(error));
        let started = std::time::Instant::now();
        let mut attempts = 0;
        let error = with_store_contention_backoff("agent-task recover", busy, || {
            attempts += 1;
            Err::<u32, _>(locked())
        })
        .unwrap_err();
        assert_eq!(attempts, STORE_CONTENTION_ATTEMPTS);
        assert_eq!(error.code.as_str(), STORE_BUSY_CODE);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));

        let mut attempts = 0;
        let value = with_store_contention_backoff("agent-task retry", busy, || {
            attempts += 1;
            if attempts < 3 {
                Err(locked())
            } else {
                Ok(attempts)
            }
        })
        .unwrap();
        assert_eq!(value, 3, "contention that clears completes the operation");

        let mut attempts = 0;
        let refused = with_store_contention_backoff("agent-task retry", busy, || {
            attempts += 1;
            Err::<u32, _>(Error::validation_invalid_argument(
                "run_id", "refused", None, None,
            ))
        })
        .unwrap_err();
        assert_eq!(attempts, 1, "other refusals are not retried");
        assert_ne!(refused.code.as_str(), STORE_BUSY_CODE);
    }

    /// Failed admissions have no recipe or execution owner for a retry.
    #[test]
    fn cook_recover_refuses_an_unmaterialized_cook_without_admission_inputs() {
        struct RetryPerformer(std::cell::RefCell<Vec<String>>);
        impl CookRecoveryPerformer for RetryPerformer {
            fn continue_cook(
                &self,
                _: &str,
                _: bool,
                _: &CookRecoveryOverrides,
            ) -> Result<(Value, i32)> {
                unreachable!("an unmaterialized Cook has no attempt to continue")
            }
            fn preflight_continue_cook(&self, _: &str, _: bool) -> Result<(Value, i32)> {
                unreachable!()
            }
            fn retry(&self, run_id: &str) -> Result<(Value, i32)> {
                self.0.borrow_mut().push(run_id.to_string());
                let retry = crate::agent_task_service::retry(run_id, None, false, false)?;
                Ok((serde_json::json!({ "run_id": retry.record.run_id }), 0))
            }
            fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
                unreachable!()
            }
            fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
                unreachable!()
            }
        }
        const GUIDANCE: &str =
            "re-run the original homeboy agent-task cook command with a fresh --run-id";
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-admission-failed";
            let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &store,
                cook_id,
                serde_json::json!({
                    "schema": "homeboy/unmaterialized-cook-binding/v1",
                    "request_ref": "sha256:request",
                    "worktree_ref": "repo@branch",
                }),
                "blocked_runner_unavailable",
                "selected runner requires admitted connected readiness evidence",
            )
            .unwrap();
            store
                .mutate_record(cook_id, |record| {
                    record.metadata["detached_cook_handoff"]["state"] =
                        serde_json::json!("exited_before_handoff");
                    record
                        .set_state_for_test(homeboy_lifecycle_contract::RunExecutionState::Failed);
                    true
                })
                .unwrap();

            // Planning and applying agree: without recorded admission inputs
            // there is no step, only the guidance to re-run the original Cook.
            let plan = plan_cook_recovery(cook_id).expect("unmaterialized Cook is planned");
            assert!(plan.step.is_none(), "{:?}", plan.step);
            assert!(cook_recovery_delegation(cook_id).is_none());

            let performer = RetryPerformer(Default::default());
            // With no recipe there is no candidate: `--regate` names the step
            // that recovers it instead of failing on the missing recipe file.
            let refused = recover_cook(
                cook_id,
                CookRecoveryRequest {
                    regate: true,
                    preflight: true,
                    ..Default::default()
                },
                &performer,
            )
            .unwrap_err();
            assert_eq!(refused.details["field"], "regate", "{refused:?}");
            assert!(refused.message.contains(GUIDANCE), "{}", refused.message);

            for preflight in [true, false] {
                let (report, exit_code) = recover_cook(
                    cook_id,
                    CookRecoveryRequest {
                        preflight,
                        ..Default::default()
                    },
                    &performer,
                )
                .unwrap();
                assert_eq!(exit_code, i32::from(!preflight), "{report:?}");
                let CookRecoveryStep::None { reason } = &report.step else {
                    panic!("{report:?}")
                };
                assert!(reason.contains(GUIDANCE), "{reason}");
            }
            assert!(performer.0.borrow().is_empty(), "no retry is attempted");
            assert!(
                store
                    .read_records()
                    .unwrap()
                    .iter()
                    .all(|record| record.run_id == cook_id),
                "no relaunch is admitted"
            );
        });
    }

    /// An unmaterialized Cook with recorded admission inputs is relaunched
    /// through the admission path under a fresh run id; replays converge.
    #[test]
    fn cook_recover_relaunches_an_unmaterialized_cook_from_its_admission_inputs() {
        struct NoPerformer;
        impl CookRecoveryPerformer for NoPerformer {
            fn continue_cook(
                &self,
                _: &str,
                _: bool,
                _: &CookRecoveryOverrides,
            ) -> Result<(Value, i32)> {
                unreachable!("an unmaterialized Cook has no attempt to continue")
            }
            fn preflight_continue_cook(&self, _: &str, _: bool) -> Result<(Value, i32)> {
                unreachable!()
            }
            fn retry(&self, _: &str) -> Result<(Value, i32)> {
                unreachable!("relaunch is not a retry")
            }
            fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
                unreachable!()
            }
            fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
                unreachable!()
            }
        }
        homeboy_core::test_support::with_isolated_home(|home| {
            let cook_id = "cook-admission-relaunch";
            let binding = super::super::execution_step::execution_step_fixtures::admission_binding(
                &home.path().join("inputs"),
                cook_id,
                Some("frozen prompt"),
            );
            let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &store,
                cook_id,
                binding,
                "blocked_runner_unavailable",
                "runner transport lost before materialization",
            )
            .unwrap();
            store
                .mutate_record(cook_id, |record| {
                    record
                        .set_state_for_test(homeboy_lifecycle_contract::RunExecutionState::Failed);
                    true
                })
                .unwrap();
            assert_eq!(
                plan_cook_recovery(cook_id).unwrap().step,
                CookRecoveryStep::Relaunch
            );

            let (report, exit_code) =
                recover_cook(cook_id, CookRecoveryRequest::default(), &NoPerformer).unwrap();
            assert_eq!(exit_code, 0, "{report:?}");
            assert_eq!(report.step, CookRecoveryStep::Relaunch);
            let relaunch_id = report.run_id.clone();
            assert_ne!(relaunch_id, cook_id);
            assert_eq!(report.result.as_ref().unwrap()["replayed"], false);

            let source = store.read_record(cook_id).unwrap();
            let relaunch = store.read_record(&relaunch_id).unwrap();
            assert_eq!(source.metadata["recovered_by"]["run_id"], relaunch_id);
            assert_eq!(relaunch.metadata["recovered_from"]["run_id"], cook_id);
            assert_eq!(
                relaunch.state(),
                homeboy_lifecycle_contract::RunExecutionState::Queued
            );
            let inputs = |record: &agent_task_lifecycle::AgentTaskRunRecord, id: &str| {
                let mut inputs = record.metadata["unmaterialized_cook_admission"]["binding"]
                    ["admission_inputs"]
                    .clone();
                inputs["cook_id"] = Value::Null;
                for argument in inputs["argv"].as_array_mut().unwrap() {
                    if argument == id {
                        *argument = Value::Null;
                    }
                }
                inputs
            };
            assert_eq!(inputs(&relaunch, &relaunch_id), inputs(&source, cook_id));
            assert_eq!(
                super::super::prepare_admission_arguments(
                    &relaunch.metadata["unmaterialized_cook_admission"]["binding"],
                    &relaunch_id,
                )
                .unwrap()
                .last()
                .unwrap(),
                "--prompt=frozen prompt"
            );

            let (replay, exit_code) =
                recover_cook(cook_id, CookRecoveryRequest::default(), &NoPerformer).unwrap();
            assert_eq!(exit_code, 0, "{replay:?}");
            assert_eq!(replay.run_id, relaunch_id);
            assert_eq!(replay.result.as_ref().unwrap()["replayed"], true);
            assert_eq!(store.read_records().unwrap().len(), 2, "one relaunch only");
        });
    }

    #[test]
    fn no_step_on_a_successful_status_declares_no_failure() {
        let (report, exit_code) =
            finish_without_step(no_step_report("review_ready", "Nothing to recover."));

        assert_eq!(exit_code, 0);
        let value = serde_json::to_value(&report).expect("serialize report");
        assert!(value.get("failure").is_none());
    }

    fn lifecycle_record(
        state: homeboy_lifecycle_contract::RunExecutionState,
        metadata: Value,
    ) -> agent_task_lifecycle::AgentTaskRunRecord {
        let mut record: agent_task_lifecycle::AgentTaskRunRecord =
            serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-run/v1", "run_id": "attempt", "plan_id": "plan",
                "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
            }))
            .expect("record");
        record.metadata = metadata;
        record
    }

    /// What `status` advertises for this record and what `recover` performs
    /// for it. `status` renders the record's lifecycle state; `recover` plans
    /// from its own label. Both must read the one run state.
    fn status_and_recover_steps(
        record: &agent_task_lifecycle::AgentTaskRunRecord,
        configure: impl Fn(&mut CookRecoveryInputs<'_>),
    ) -> (CookRecoveryStep, CookRecoveryStep) {
        let state = agent_task_lifecycle::CookRunState::of(record);
        let status_label = serde_json::to_value(record.state()).expect("state label");
        let status_label = status_label.as_str().expect("state label");
        let decide = |label: &str| {
            let mut inputs = CookRecoveryInputs {
                status: label,
                run_state: Some(state.clone()),
                recovery_legal: true,
                run_id: &record.run_id,
                ..Default::default()
            };
            configure(&mut inputs);
            cook_recovery_step(&inputs)
        };
        (decide(status_label), decide(&state.status))
    }

    fn eligibility(
        record: &agent_task_lifecycle::AgentTaskRunRecord,
        action: homeboy_control_plane_contract::ControlPlaneAction,
    ) -> homeboy_control_plane_contract::ControlPlaneActionEligibility {
        agent_task_lifecycle::lifecycle_action_eligibility(record, None)
            .actions
            .into_iter()
            .find(|entry| entry.action == action)
            .expect("action")
    }

    /// #17976: a Lab Cook whose provider hit its wall clock after producing a
    /// candidate. Its record is `candidate_recoverable`, but its last Cook
    /// exit had reported `in_flight`. `status` named `recover`, while
    /// `recover --preflight` and `recover --regate` refused it as in flight.
    #[test]
    fn regression_17976_status_recover_and_regate_agree_on_a_timed_out_candidate() {
        let record = lifecycle_record(
            homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable,
            serde_json::json!({
                "cook_id": "cook-17976",
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-17976",
                "cook_progress": { "phase": "terminal", "terminal_status": "in_flight" },
            }),
        );
        let state = agent_task_lifecycle::CookRunState::of(&record);
        assert_eq!(state.status, "candidate_recoverable");
        assert!(state.allows(homeboy_control_plane_contract::ControlPlaneAction::Recover));
        assert!(
            state.allows_regate(),
            "the regate guard reads this legal action"
        );
        let (status_step, recover_step) = status_and_recover_steps(&record, |_| {});
        assert_eq!(status_step, CookRecoveryStep::Continue);
        assert_eq!(recover_step, status_step);

        // A genuinely live continuation is in flight on every surface.
        let mut live = record.clone();
        live.metadata["cook_continuation"] =
            serde_json::json!({ "state": "claimed", "owner_pid": std::process::id() });
        let (status_step, recover_step) = status_and_recover_steps(&live, |_| {});
        assert!(status_step.is_none(), "{status_step:?}");
        assert_eq!(recover_step, status_step);
        assert!(!agent_task_lifecycle::CookRunState::of(&live).allows_regate());
    }

    #[test]
    fn recovery_uses_record_recoverability_not_a_conflicting_status_label() {
        let record = lifecycle_record(
            homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable,
            serde_json::json!({}),
        );
        let inputs = CookRecoveryInputs {
            status: "gate_failed",
            run_state: Some(agent_task_lifecycle::CookRunState::of(&record)),
            recovery_legal: true,
            continuation_completed: true,
            ..Default::default()
        };
        let step = cook_recovery_step(&inputs);
        assert!(step.is_none(), "{step:?}");
        assert!(format!("{step:?}").contains("already completed"));
    }

    /// #17384: after a transport retry, `diagnose` and `review` named
    /// `recover` for a terminal `candidate_recoverable` proxy attempt, and
    /// `recover` answered "still in flight" because the retry's last exit had
    /// reported `queued`.
    #[test]
    fn regression_17384_transport_retry_candidate_is_recoverable_everywhere() {
        let record = lifecycle_record(
            homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable,
            serde_json::json!({
                "cook_id": "cook-17384",
                "kind": "lab_offload_controller_proxy",
                "phase": "retry",
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-17384-transport-retry",
                "cook_progress": { "phase": "terminal", "terminal_status": "queued" },
            }),
        );
        use homeboy_control_plane_contract::{ControlPlaneAction, ControlPlaneActionAvailability};
        let resume = eligibility(&record, ControlPlaneAction::Recover);
        assert_eq!(
            resume.availability,
            ControlPlaneActionAvailability::Unavailable
        );
        assert!(resume
            .reason
            .contains("homeboy agent-task recover cook-17384"));
        let cancel = eligibility(&record, ControlPlaneAction::Cancel);
        assert_eq!(
            cancel.availability,
            ControlPlaneActionAvailability::Unavailable
        );
        assert!(!agent_task_lifecycle::CookRunState::of(&record)
            .allows(homeboy_control_plane_contract::ControlPlaneAction::Cancel));
        // The verb the review projection names accepts the run.
        let (status_step, recover_step) = status_and_recover_steps(&record, |_| {});
        assert_eq!(recover_step, CookRecoveryStep::Continue);
        assert_eq!(recover_step, status_step);
    }

    /// #17489: a `candidate_recoverable` Cook whose applied candidate only has
    /// deferred gates (its last exit handed verification to CI) could not be
    /// re-gated: `recover --regate` refused it as in flight.
    #[test]
    fn regression_17489_candidate_recoverable_cook_advances_through_regate() {
        let record = lifecycle_record(
            homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable,
            serde_json::json!({
                "cook_id": "cook-17489",
                "cook_progress": { "phase": "terminal", "terminal_status": "awaiting_ci" },
            }),
        );
        let state = agent_task_lifecycle::CookRunState::of(&record);
        assert_eq!(state.status, "candidate_recoverable");
        assert!(state.allows_regate());
        let (status_step, recover_step) = status_and_recover_steps(&record, |inputs| {
            inputs.applied_gate_failure = true;
            inputs.regate_ready = true;
        });
        assert_eq!(recover_step, CookRecoveryStep::Regate);
        assert_eq!(recover_step, status_step);
    }

    /// #17295: a provider-succeeded Cook whose continuation was never
    /// scheduled (the Lab was unreachable at the continuation boundary, so its
    /// last exit reported `queued`). `status` named `recover`, and `recover`'s
    /// guidance named a read-only command, so the operator looped. One
    /// `recover` must perform the continuation instead.
    #[test]
    fn regression_17295_recover_performs_the_continuation_instead_of_naming_a_no_op() {
        let record = lifecycle_record(
            homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable,
            serde_json::json!({
                "cook_id": "cook-17295",
                "provider_executions_consumed": 1,
                "cook_progress": { "phase": "terminal", "terminal_status": "queued" },
            }),
        );
        let (status_step, recover_step) = status_and_recover_steps(&record, |_| {});
        assert_eq!(recover_step, CookRecoveryStep::Continue);
        assert_eq!(recover_step, status_step);
        let next = recover_step.next_action("homeboy", "cook-17295");
        assert!(
            !next.show,
            "recover's guidance must not be a read-only no-op"
        );
        assert!(next.command.contains("recover"), "{}", next.command);
        assert_eq!(recover_step.action_name(), Some("resume"));
    }
}
