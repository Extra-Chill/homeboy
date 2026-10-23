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
    /// The already-applied candidate failed (or never completed) its gates and
    /// no continuation is admitted. Re-verify that exact candidate with
    /// corrected gates, or record candidate-bound external proof, without
    /// re-applying the patch or replaying provider work. Both need explicit
    /// operator authorization (`recover --authorize-external-proof ...`).
    ReplaceGates,
    /// The verified candidate is unpublished; publish it from the durable Cook
    /// record or validated manual-finalization intent.
    Publish,
    /// The Cook produced no candidate patch, but the destination worktree's
    /// branch holds commits ahead of its recorded base: the operator's verified
    /// work is the candidate. Adopt that exact revision into the Cook
    /// (`agent-task adopt <cook> --candidate-ref <head-sha>`) instead of
    /// replaying provider work that would end `no_changes` again (#16181).
    Adopt { candidate_ref: String },
    /// Nothing `recover` can perform; `reason` names what is required.
    None { reason: String },
}

impl CookRecoveryStep {
    /// The legal-action name status and notifications advertise for this
    /// step. `None` advertises no mutating action.
    pub fn action_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::Continue => "resume",
            Self::Rearm => "rearm",
            Self::Retry => "retry",
            Self::ForkReplacement => "fork_replacement",
            Self::Reconcile => "reconcile",
            Self::ReplaceGates => "replace_gates",
            Self::Publish => "publish",
            Self::Adopt { .. } => "adopt",
            Self::None { .. } => return None,
        })
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None { .. })
    }

    pub(crate) fn none(reason: impl Into<String>) -> Self {
        Self::None {
            reason: reason.into(),
        }
    }

    /// The mutating legal actions for this step, each rendered as the one
    /// `recover` entry point. Only a choice between artifacts needs an
    /// argument; every other step is performed by the bare command.
    pub(crate) fn recovery_actions(
        &self,
        prefix: &str,
        run_id: &str,
    ) -> Vec<super::AgentTaskCookRecoveryAction> {
        let Some(action) = self.action_name() else {
            return Vec::new();
        };
        let command = |args: &[&str]| super::cook_recovery_command_with_prefix(prefix, args);
        match self {
            Self::Adopt { candidate_ref } => vec![super::AgentTaskCookRecoveryAction {
                action: action.to_string(),
                command: command(&["adopt", run_id, "--candidate-ref", candidate_ref]),
            }],
            _ => vec![super::AgentTaskCookRecoveryAction {
                action: action.to_string(),
                command: command(&["recover", run_id]),
            }],
        }
    }
}

/// Everything the step decision reads. These are exactly the facts the
/// legal-action classifier has always used, so both share one answer.
#[derive(Debug, Clone, Default)]
pub(crate) struct CookRecoveryInputs<'a> {
    pub status: &'a str,
    /// A durable lifecycle record exists for the attempt.
    pub recovery_legal: bool,
    /// A promotion operation claim is still `running`.
    pub blocking_claim: bool,
    /// Retry (or continuation) admission accepted this attempt.
    pub retry_admitted: bool,
    /// The checkpoint's typed recovery action is `fork_replacement`.
    pub exact_checkpoint_candidate_mismatch: bool,
    /// The attempt's terminal continuation failed and must be rearmed.
    pub continuation_rearm_required: bool,
    /// A completed terminal continuation cannot be claimed again.
    pub continuation_completed: bool,
    /// The attempt whose recorded review form is consulted below.
    pub run_id: &'a str,
    /// `incomplete_reason` of a recorded review form that declares the task
    /// incomplete. Continuation re-reads that same terminal verdict, so it
    /// can never publish the candidate.
    pub agent_incomplete_reason: Option<&'a str>,
    /// The attempt's applied candidate failed (or never completed) its
    /// deterministic gates and holds no replacement proof yet.
    pub applied_gate_failure: bool,
    /// The attempt's applied candidate is verified, carries a publishable
    /// review form, and has no publication receipt.
    pub publish_ready: bool,
    /// Applied, verified candidate with no publication receipt, independently
    /// of whether its outcome contains a publishable review form.
    pub verified_unpublished: bool,
    /// HEAD of the destination worktree when its branch holds commits ahead
    /// of the recorded base (#16181). A terminal `no_changes` attempt in that
    /// state is adoptable: the operator's committed work is the candidate the
    /// provider could not produce.
    pub adoptable_candidate_ref: Option<&'a str>,
}

/// Decide the single next recovery step from durable Cook evidence.
pub(crate) fn cook_recovery_step(inputs: &CookRecoveryInputs<'_>) -> CookRecoveryStep {
    if !inputs.recovery_legal {
        return CookRecoveryStep::none(
            "No recovery command is legal because the durable recipe has no lifecycle record. Start a fresh Cook after preserving the recipe reference for investigation.",
        );
    }
    let status = CookStatus::from_status(inputs.status);
    if status.is_in_flight() {
        return CookRecoveryStep::none(
            "The Cook is still in flight; watch it with `agent-task status` instead.",
        );
    }
    if inputs.blocking_claim {
        return CookRecoveryStep::Reconcile;
    }
    if inputs.exact_checkpoint_candidate_mismatch && inputs.retry_admitted {
        return CookRecoveryStep::ForkReplacement;
    }
    if status == CookStatus::CandidateRecoverable {
        if let Some(reason) = inputs.agent_incomplete_reason {
            return CookRecoveryStep::none(format!(
                "The agent's review form declares the task incomplete: {reason}. Continuing would re-read the same verdict, so `recover` cannot publish this candidate on its own. Inspect it with `agent-task diagnose {run}`; if the task is in fact done, publish it with a complete review form via `agent-task recover {run} --review-form @FORM.json --review-form-tool <TOOL> --review-form-model <MODEL> --review-form-author <IDENTITY>`, otherwise start a fresh Cook.",
                reason = reason.trim_end_matches('.'),
                run = inputs.run_id
            ));
        }
    }
    if inputs.publish_ready && !status_is_settled(&status) {
        return CookRecoveryStep::Publish;
    }
    if status == CookStatus::CandidateRecoverable {
        if inputs.verified_unpublished {
            return CookRecoveryStep::none(format!(
                "The verified candidate has no publishable review form. Inspect it with `agent-task diagnose {run}`; if the task is done, publish it with a complete review form via `agent-task recover {run} --review-form @FORM.json --review-form-tool <TOOL> --review-form-model <MODEL> --review-form-author <IDENTITY>`.",
                run = inputs.run_id
            ));
        }
        if inputs.continuation_completed {
            return CookRecoveryStep::none(
                "The candidate's terminal continuation is already completed; inspect it with `agent-task diagnose` before supplying publication inputs.",
            );
        }
    }
    if status == CookStatus::NoChanges {
        if let Some(candidate_ref) = inputs.adoptable_candidate_ref {
            return CookRecoveryStep::Adopt {
                candidate_ref: candidate_ref.to_string(),
            };
        }
    }
    if status == CookStatus::PreExecutionFailure && inputs.retry_admitted {
        return CookRecoveryStep::Retry;
    }
    let continuation_eligible = match &status {
        CookStatus::Completed
        | CookStatus::ReviewReady
        | CookStatus::GreenNoFinalize
        | CookStatus::IntentionalNoChange
        | CookStatus::NoCandidate
        | CookStatus::Cancelled
        | CookStatus::ExecutionBudgetExhausted
        | CookStatus::RetriesExhausted
        | CookStatus::PreExecutionFailure => false,
        status if status.is_gate_failure() => inputs.retry_admitted,
        _ => true,
    };
    if !continuation_eligible {
        if status.is_gate_failure() && inputs.applied_gate_failure {
            return CookRecoveryStep::ReplaceGates;
        }
        return CookRecoveryStep::none(format!(
            "Cook status `{}` admits no recovery step; inspect it with `agent-task status` or `agent-task diagnose`.",
            inputs.status
        ));
    }
    if inputs.continuation_rearm_required {
        CookRecoveryStep::Rearm
    } else {
        CookRecoveryStep::Continue
    }
}

/// Statuses whose candidate is already delivered or deliberately left
/// unpublished; a green candidate there is never republished by `recover`.
fn status_is_settled(status: &CookStatus) -> bool {
    matches!(
        status,
        CookStatus::Completed
            | CookStatus::ReviewReady
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CookRecoveryOverrides {
    /// Larger provider timeout for the successor attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Larger review-form deadline for a timed-out review-form attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_form_timeout_ms: Option<u64>,
    /// Provider route for the successor attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub acknowledge_model_override: bool,
}

impl CookRecoveryOverrides {
    pub fn is_empty(&self) -> bool {
        self.timeout_ms.is_none()
            && self.review_form_timeout_ms.is_none()
            && !self.has_route_override()
    }

    /// A provider route override is a rearm of the terminal attempt.
    pub fn has_route_override(&self) -> bool {
        self.backend.is_some() || self.selector.is_some() || self.model.is_some()
    }
}

/// Corrected replacement verification for the already-applied candidate of a
/// [`CookRecoveryStep::ReplaceGates`] step.
#[derive(Debug, Clone)]
pub enum CookReplacementProof {
    /// Execute these corrected gates against the applied candidate.
    Gates(crate::agent_task_gate::VerifyGateOptions),
    /// Record this externally produced, candidate-bound promotion report.
    External {
        promotion: Box<crate::agent_task_promotion::AgentTaskPromotionReport>,
        accept_inherited_failures: bool,
    },
}

/// Operator inputs that feed [`CookRecoveryStep::ReplaceGates`]. Replacement
/// proof always requires an explicit authorization; `recover` never supplies
/// one on its own.
#[derive(Debug, Clone)]
pub struct CookReplaceGatesRequest {
    pub proof: CookReplacementProof,
    /// Explicit operator authorization for the recorded replacement proof.
    pub authorize_external_proof: String,
    /// Explicit authorization to rerun gates after an interrupted replacement
    /// execution. Only meaningful with [`CookReplacementProof::Gates`].
    pub authorize_interrupted_rerun: Option<String>,
}

/// Operator inputs that feed [`CookRecoveryStep::Publish`].
#[derive(Debug, Clone, Default)]
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
    /// Explicit successor-attempt overrides.
    pub overrides: CookRecoveryOverrides,
    /// Inputs for [`CookRecoveryStep::ReplaceGates`]. Supplying them selects
    /// that step for an applied candidate whose gates failed.
    pub replace_gates: Option<CookReplaceGatesRequest>,
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
    /// Continue the Cook from `run_id`, optionally rearming its failed
    /// terminal continuation, promoting one named patch artifact, or reserving
    /// a successor attempt with explicit operator overrides.
    fn continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        artifact_id: Option<&str>,
        overrides: &CookRecoveryOverrides,
    ) -> Result<(Value, i32)>;
    /// Validate continuation admission for `run_id` without dispatching a
    /// provider or mutating lifecycle state.
    fn preflight_continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        artifact_id: Option<&str>,
    ) -> Result<(Value, i32)>;
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
            CookReplacementProof::External {
                promotion,
                accept_inherited_failures,
            } => super::record_replacement_gate_proof(
                run_id,
                *promotion,
                Some(request.authorize_external_proof),
                accept_inherited_failures,
            )?,
        };
        Ok((
            serde_json::to_value(report)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
            0,
        ))
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

/// Resolve a Cook ID, attempt ID, batch-child run ID or manual-finalization
/// run ID to the attempt recovery operates on, then decide its recovery step.
///
/// A Cook ID resolves through the same latest-attempt selection continuation
/// uses, so every step (including replacement gates and publication) acts on
/// the attempt `agent-task status` reports.
pub fn plan_cook_recovery(id: &str) -> Result<CookRecoveryPlan> {
    let mut plan = match plan_cook_recovery_for_cook(id) {
        Ok(plan) => plan,
        Err(error) => return manual_finalization_plan(id).ok_or(error),
    };
    // A validated manual-finalization intent is the operator's explicit
    // publication decision for this attempt; only a held claim precedes it.
    if plan.step != CookRecoveryStep::Reconcile
        && super::manual_finalization_publication_pending(&plan.run_id)
    {
        plan.step = CookRecoveryStep::Publish;
    }
    Ok(plan)
}

/// A durable manual-finalization record outside any readable Cook recipe:
/// its only recovery is publishing the validated intent, which replays the
/// recorded receipt once published.
fn manual_finalization_plan(id: &str) -> Option<CookRecoveryPlan> {
    if !super::manual_finalization_intent_recorded(id) {
        return None;
    }
    let record = agent_task_lifecycle::exact_record(id).ok()?;
    Some(CookRecoveryPlan {
        cook_id: record
            .metadata
            .get("cook_id")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string(),
        run_id: id.to_string(),
        status: cook_recovery_status(&record),
        step: CookRecoveryStep::Publish,
    })
}

fn plan_cook_recovery_for_cook(id: &str) -> Result<CookRecoveryPlan> {
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
    let status = cook_recovery_status(&record);
    let step = super::cook_failure_context(&recipe.cook_id, Some(&run_id), &status)
        .and_then(|context| context.recovery_step)
        .unwrap_or_else(|| {
            CookRecoveryStep::none("The Cook recipe or its recovery context could not be read.")
        });
    Ok(CookRecoveryPlan {
        cook_id: recipe.cook_id,
        run_id,
        status,
        step,
    })
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
    super::load_recipe(id).ok()?;
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
pub fn recover_cook(
    id: &str,
    request: CookRecoveryRequest,
    performer: &dyn CookRecoveryPerformer,
) -> Result<(CookRecoveryReport, i32)> {
    let CookRecoveryRequest {
        preflight,
        overrides,
        replace_gates,
        publish,
    } = request;
    let artifact_id = None;
    let mut plan = plan_cook_recovery(id)?;
    validate_step_inputs(
        &mut plan,
        artifact_id,
        &overrides,
        replace_gates.is_some(),
        &publish,
    )?;
    if !overrides.is_empty() {
        validate_overrides(&plan, preflight, artifact_id)?;
    }
    let report = CookRecoveryReport {
        schema: COOK_RECOVERY_SCHEMA,
        cook_id: plan.cook_id,
        run_id: plan.run_id,
        status: plan.status,
        step: plan.step,
        preflight,
        overrides: (!overrides.is_empty()).then(|| overrides.clone()),
        admission: None,
        result: None,
        failure: None,
    };
    let run_id = report.run_id.clone();
    let run_id = run_id.as_str();
    if !overrides.is_empty() {
        let result =
            performer.continue_cook(run_id, overrides.has_route_override(), None, &overrides);
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
            CookRecoveryStep::Continue => Some((false, None)),
            CookRecoveryStep::Rearm => Some((true, None)),
            _ => None,
        };
        // A recipe-only attempt has no lifecycle record to admit yet: the
        // continuation materializes it, which the plan already states.
        let continuation = continuation.filter(|_| report.status != RECIPE_ONLY_STATUS);
        let Some((rearm, artifact_id)) = continuation else {
            return Ok((report, 0));
        };
        let result = performer.preflight_continue_cook(run_id, rearm, artifact_id);
        return Ok(finish_performed_step(report, result));
    }
    if report.step.is_none() {
        return Ok(finish_without_step(report));
    }
    let none = CookRecoveryOverrides::default();
    let result = (|| {
        Ok(match &report.step {
            CookRecoveryStep::Continue => performer.continue_cook(run_id, false, None, &none)?,
            CookRecoveryStep::Rearm => performer.continue_cook(run_id, true, None, &none)?,
            CookRecoveryStep::Retry | CookRecoveryStep::ForkReplacement => {
                performer.retry(run_id)?
            }
            CookRecoveryStep::Reconcile => performer.reconcile(run_id)?,
            CookRecoveryStep::Adopt { candidate_ref } => performer.adopt(run_id, candidate_ref)?,
            CookRecoveryStep::ReplaceGates => {
                let Some(replace_gates) = replace_gates else {
                    return Err(replace_gates_inputs_required(run_id));
                };
                let (mut result, exit_code) = performer.replace_gates(run_id, replace_gates)?;
                if let Some(object) = result.as_object_mut() {
                    object.insert(
                    "handoff".to_string(),
                    serde_json::json!({
                        "next_command": super::cook_recover_command(None, run_id, None),
                        "status_command": super::cook_recovery_command(run_id, &["status", run_id]),
                    }),
                );
                }
                (result, exit_code)
            }
            CookRecoveryStep::Publish => performer.publish(run_id, &publish, false)?,
            CookRecoveryStep::None { .. } => unreachable!("no-step handled before performing"),
        })
    })();
    Ok(finish_performed_step(report, result))
}

fn finish_performed_step(
    mut report: CookRecoveryReport,
    result: Result<(Value, i32)>,
) -> (CookRecoveryReport, i32) {
    match result {
        Ok((value, exit_code)) => {
            if report.preflight {
                report.admission = Some(value);
            } else {
                report.result = Some(value);
            }
            (report, exit_code)
        }
        Err(error) => {
            report.failure = Some(serde_json::json!({
                "code": error.code.as_str(),
                "message": error.message,
                "details": error.details,
            }));
            (report, 1)
        }
    }
}

fn replace_gates_inputs_required(run_id: &str) -> Error {
    Error::validation_invalid_argument(
        "replace-gates",
        "replacement gates need explicit operator inputs: --authorize-external-proof <TEXT> with corrected gates (--verify/--verify-file) or candidate-bound proof (--promotion @FILE)",
        Some(run_id.to_string()),
        Some(vec![
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
    artifact_id: Option<&str>,
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
            None,
        ))
    };
    if replace_gates && publish {
        return refuse(
            "recovery-step",
            "replacement-gate inputs and publication inputs select different steps; supply one set"
                .to_string(),
        );
    }
    if artifact_id.is_some() || !overrides.is_empty() {
        return refuse(
            "recovery-step",
            "replacement-gate and publication inputs cannot be combined with --artifact-id or continuation overrides".to_string(),
        );
    }
    let (field, selected) = if replace_gates {
        ("replace-gates", CookRecoveryStep::ReplaceGates)
    } else {
        ("publish", CookRecoveryStep::Publish)
    };
    if CookStatus::from_status(&plan.status).is_in_flight() {
        return refuse(
            field,
            "the Cook is still in flight; watch it with `agent-task status` instead".to_string(),
        );
    }
    match &plan.step {
        step if *step == selected => Ok(()),
        CookRecoveryStep::Continue | CookRecoveryStep::Rearm | CookRecoveryStep::None { .. } => {
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
fn validate_overrides(
    plan: &CookRecoveryPlan,
    preflight: bool,
    artifact_id: Option<&str>,
) -> Result<()> {
    let refuse = |message: &str| {
        Err(Error::validation_invalid_argument(
            "recovery-override",
            message.to_string(),
            Some(plan.run_id.clone()),
            None,
        ))
    };
    if preflight {
        return refuse("timeout and provider-route overrides cannot be combined with --preflight");
    }
    if artifact_id.is_some() {
        return refuse(
            "timeout and provider-route overrides cannot be combined with --artifact-id",
        );
    }
    match &plan.step {
        CookRecoveryStep::Reconcile => refuse(
            "a promotion claim is still held; run `agent-task recover` without overrides to reconcile it first",
        ),
        CookRecoveryStep::Adopt { .. } => refuse(
            "the recovery step adopts the worktree's committed candidate; it reserves no provider attempt to override",
        ),
        CookRecoveryStep::ForkReplacement => refuse(
            "the recovery step forks a replacement attempt that preserves the checkpoint; it accepts no overrides",
        ),
        _ => Ok(()),
    }
}

/// A batch-child run is recorded with the Cook it belongs to; every other
/// identifier already names a Cook or one of its attempts.
fn resolve_cook_key(id: &str) -> Result<String> {
    if super::load_recipe(id).is_ok() || super::load_recipe_for_attempt(id)?.is_some() {
        return Ok(id.to_string());
    }
    agent_task_lifecycle::exact_record(id)
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

/// The Cook status the recovery decision classifies. Delivery's typed read
/// model decides first; otherwise the attempt's own lifecycle evidence.
pub(crate) fn cook_recovery_status(record: &agent_task_lifecycle::AgentTaskRunRecord) -> String {
    use agent_task_lifecycle::AgentTaskRunState as State;
    match record.state() {
        State::Queued => return "queued".to_string(),
        State::Running => return "running".to_string(),
        _ => {}
    }
    if let Some(status) = super::CookDelivery::from_run_metadata(&record.metadata)
        .and_then(|delivery| delivery.lifecycle().cook_status)
    {
        return status.as_str().to_string();
    }
    if record
        .metadata
        .get("pre_execution_failure")
        .is_some_and(|failure| !failure.is_null())
    {
        return "pre_execution_failure".to_string();
    }
    if let Some(status) = record
        .metadata
        .pointer("/cook_progress/terminal_status")
        .and_then(Value::as_str)
    {
        return status.to_string();
    }
    match record.state() {
        State::Queued => "queued",
        State::Running => "running",
        State::Succeeded => "succeeded",
        State::CandidateRecoverable | State::PartialRecoverable => "candidate_recoverable",
        State::PartialFailure | State::Failed => "failed",
        State::Cancelled => "cancelled",
    }
    .to_string()
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
    fn agent_reported_incomplete_candidate_has_no_continuation_step() {
        let reason = "the PR could not be opened";
        let decided = step("candidate_recoverable", |inputs| {
            inputs.run_id = "cook-attempt-1";
            inputs.agent_incomplete_reason = Some(reason);
        });
        let CookRecoveryStep::None { reason: message } = decided else {
            panic!(
                "an agent-declared incomplete candidate must not advertise continuation: {decided:?}"
            );
        };
        assert!(message.contains(reason), "{message}");
        assert!(
            message.contains("agent-task recover cook-attempt-1 --review-form @FORM.json"),
            "{message}"
        );
        assert_eq!(
            step("candidate_recoverable", |_| {}),
            CookRecoveryStep::Continue,
            "other recoverable candidates still continue"
        );
    }

    #[test]
    fn verified_candidate_without_review_form_requires_operator_form() {
        let decided = step("candidate_recoverable", |inputs| {
            inputs.run_id = "run-1";
            inputs.verified_unpublished = true;
            inputs.continuation_completed = true;
        });
        let CookRecoveryStep::None { reason } = decided else {
            panic!("missing review form must not continue: {decided:?}");
        };
        assert!(reason.contains("agent-task recover run-1 --review-form @FORM.json --review-form-tool <TOOL> --review-form-model <MODEL> --review-form-author <IDENTITY>"));
        assert_eq!(
            step("candidate_recoverable", |inputs| {
                inputs.verified_unpublished = true;
                inputs.continuation_completed = true;
                inputs.publish_ready = true;
            }),
            CookRecoveryStep::Publish
        );
        assert!(step("candidate_recoverable", |inputs| {
            inputs.continuation_completed = true;
        })
        .is_none());

        let mut plan = CookRecoveryPlan {
            cook_id: "cook".to_string(),
            run_id: "run-1".to_string(),
            status: "candidate_recoverable".to_string(),
            step: CookRecoveryStep::none(reason),
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
            None,
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
            "completed",
            "cancelled",
            "execution_budget_exhausted",
            "pre_execution_failure",
            "gate_failed",
            "running",
        ] {
            assert!(step(status, |_| {}).is_none(), "{status}");
        }
        assert!(
            step("verification_pending", |inputs| inputs.recovery_legal =
                false)
            .is_none()
        );
    }

    #[test]
    fn applied_gate_failure_without_retry_admission_replaces_gates() {
        assert_eq!(
            step("gate_failed", |inputs| inputs.applied_gate_failure = true),
            CookRecoveryStep::ReplaceGates
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
            step("completed", |inputs| inputs.applied_gate_failure = true).is_none(),
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
                    inputs.publish_ready = true;
                    inputs.retry_admitted = true;
                }),
                CookRecoveryStep::Publish,
                "{status}"
            );
        }
        for status in [
            "completed",
            "review_ready",
            "cancelled",
            "green_no_finalize",
        ] {
            assert_ne!(
                step(status, |inputs| inputs.publish_ready = true),
                CookRecoveryStep::Publish,
                "{status} is already settled"
            );
        }
        assert_eq!(
            step("candidate_recoverable", |inputs| {
                inputs.publish_ready = true;
                inputs.agent_incomplete_reason = Some("not done");
            })
            .action_name(),
            None,
            "an agent-declared incomplete form is never published"
        );
    }

    #[test]
    fn mutating_actions_name_only_the_recover_entry_point() {
        let actions = |step: CookRecoveryStep| {
            step.recovery_actions("homeboy", "cook-attempt-1")
                .into_iter()
                .map(|action| (action.action, action.command))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            actions(CookRecoveryStep::Continue),
            vec![(
                "resume".to_string(),
                "homeboy agent-task recover cook-attempt-1".to_string()
            )]
        );
        for (step, action) in [
            (CookRecoveryStep::ReplaceGates, "replace_gates"),
            (CookRecoveryStep::Publish, "publish"),
        ] {
            assert_eq!(
                actions(step),
                vec![(
                    action.to_string(),
                    "homeboy agent-task recover cook-attempt-1".to_string()
                )]
            );
        }
        assert!(actions(CookRecoveryStep::none("done")).is_empty());
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
    fn no_step_on_a_successful_status_declares_no_failure() {
        let (report, exit_code) =
            finish_without_step(no_step_report("review_ready", "Nothing to recover."));

        assert_eq!(exit_code, 0);
        let value = serde_json::to_value(&report).expect("serialize report");
        assert!(value.get("failure").is_none());
    }
}
