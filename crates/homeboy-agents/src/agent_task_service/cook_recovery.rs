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
    /// Continue promotion with the one canonical patch artifact the failed
    /// selection named.
    ContinueWithArtifact { artifact_id: String },
    /// Several distinct patch artifacts remain; the operator must name one
    /// (`recover --artifact-id <id>`).
    ChooseArtifact { artifact_ids: Vec<String> },
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
    /// Nothing `recover` can perform; `reason` names what is required.
    None { reason: String },
}

impl CookRecoveryStep {
    /// The legal-action name status and notifications advertise for this
    /// step. `None` advertises no mutating action.
    pub fn action_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::Continue => "resume",
            Self::ContinueWithArtifact { .. } | Self::ChooseArtifact { .. } => {
                "resume_with_artifact"
            }
            Self::Rearm => "rearm",
            Self::Retry => "retry",
            Self::ForkReplacement => "fork_replacement",
            Self::Reconcile => "reconcile",
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
            Self::ChooseArtifact { artifact_ids } => artifact_ids
                .iter()
                .map(|artifact_id| super::AgentTaskCookRecoveryAction {
                    action: action.to_string(),
                    command: command(&["recover", run_id, "--artifact-id", artifact_id]),
                })
                .collect(),
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
    /// Canonical patch artifacts when promotion failed on selection.
    pub ambiguous_artifact_ids: &'a [String],
    /// The attempt's terminal continuation failed and must be rearmed.
    pub continuation_rearm_required: bool,
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
    match inputs.ambiguous_artifact_ids {
        [] => {}
        [artifact_id] => {
            return CookRecoveryStep::ContinueWithArtifact {
                artifact_id: artifact_id.clone(),
            }
        }
        artifact_ids => {
            return CookRecoveryStep::ChooseArtifact {
                artifact_ids: artifact_ids.to_vec(),
            }
        }
    }
    if status == CookStatus::PreExecutionFailure && inputs.retry_admitted {
        return CookRecoveryStep::Retry;
    }
    let continuation_eligible = match status {
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

/// The handlers a recovery step performs. Implemented by the CLI with the
/// exact Cook continuation, retry and reconcile handlers, so recovery has no
/// orchestration of its own.
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
}

/// Status of a Cook attempt whose recipe is durable but whose lifecycle record
/// was never written; the same name `agent-task status` reports.
const RECIPE_ONLY_STATUS: &str = "recipe_only_recovery_required";

/// Resolve a Cook ID, attempt ID or batch-child run ID to its Cook and the
/// attempt continuation would select, then decide its recovery step.
pub fn plan_cook_recovery(id: &str) -> Result<CookRecoveryPlan> {
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
    plan_cook_recovery(id)
        .ok()
        .filter(|plan| !plan.step.is_none())
}

/// Perform (or, with `preflight`, only report) the one next recovery step.
///
/// `artifact_id` resolves a [`CookRecoveryStep::ChooseArtifact`] decision; it
/// is refused for every other step so it can never redirect recovery.
/// `overrides` are explicit operator inputs that reserve a successor attempt
/// with a changed parameter; they are refused where the step forbids a fresh
/// provider attempt (a held claim, an artifact choice, a replacement fork).
///
/// With `preflight`, a continuation step also runs the continuation admission
/// checks and reports them as `admission`; its exit code is the report's.
pub fn recover_cook(
    id: &str,
    preflight: bool,
    artifact_id: Option<&str>,
    overrides: &CookRecoveryOverrides,
    performer: &dyn CookRecoveryPerformer,
) -> Result<(CookRecoveryReport, i32)> {
    let plan = plan_cook_recovery(id)?;
    if !overrides.is_empty() {
        validate_overrides(&plan, preflight, artifact_id)?;
    }
    let selected_artifact = match (&plan.step, artifact_id, preflight) {
        // Preflight reports an unresolved artifact choice instead of failing.
        (CookRecoveryStep::ChooseArtifact { .. }, None, true) => None,
        _ => selected_artifact(&plan, artifact_id)?,
    };
    let mut report = CookRecoveryReport {
        schema: COOK_RECOVERY_SCHEMA,
        cook_id: plan.cook_id,
        run_id: plan.run_id,
        status: plan.status,
        step: plan.step,
        preflight,
        overrides: (!overrides.is_empty()).then(|| overrides.clone()),
        admission: None,
        result: None,
    };
    let run_id = report.run_id.clone();
    let run_id = run_id.as_str();
    if !overrides.is_empty() {
        let (result, exit_code) =
            performer.continue_cook(run_id, overrides.has_route_override(), None, overrides)?;
        report.result = Some(result);
        return Ok((report, exit_code));
    }
    if preflight {
        let continuation = match &report.step {
            CookRecoveryStep::Continue => Some((false, None)),
            CookRecoveryStep::ContinueWithArtifact { .. } => {
                Some((true, selected_artifact.as_deref()))
            }
            CookRecoveryStep::ChooseArtifact { .. } => selected_artifact
                .as_deref()
                .map(|artifact_id| (true, Some(artifact_id))),
            CookRecoveryStep::Rearm => Some((true, None)),
            _ => None,
        };
        // A recipe-only attempt has no lifecycle record to admit yet: the
        // continuation materializes it, which the plan already states.
        let continuation = continuation.filter(|_| report.status != RECIPE_ONLY_STATUS);
        let Some((rearm, artifact_id)) = continuation else {
            return Ok((report, 0));
        };
        let (admission, exit_code) =
            performer.preflight_continue_cook(run_id, rearm, artifact_id)?;
        report.admission = Some(admission);
        return Ok((report, exit_code));
    }
    let none = CookRecoveryOverrides::default();
    let (result, exit_code) = match &report.step {
        CookRecoveryStep::Continue => performer.continue_cook(run_id, false, None, &none)?,
        CookRecoveryStep::ContinueWithArtifact { .. } | CookRecoveryStep::ChooseArtifact { .. } => {
            performer.continue_cook(run_id, true, selected_artifact.as_deref(), &none)?
        }
        CookRecoveryStep::Rearm => performer.continue_cook(run_id, true, None, &none)?,
        CookRecoveryStep::Retry | CookRecoveryStep::ForkReplacement => performer.retry(run_id)?,
        CookRecoveryStep::Reconcile => performer.reconcile(run_id)?,
        CookRecoveryStep::None { .. } => {
            let exit_code = i32::from(!CookStatus::from_status(&report.status).is_success_exit());
            return Ok((report, exit_code));
        }
    };
    report.result = Some(result);
    Ok((report, exit_code))
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
        CookRecoveryStep::ContinueWithArtifact { .. } | CookRecoveryStep::ChooseArtifact { .. } => {
            refuse(
                "the recovery step promotes an existing patch artifact; it reserves no provider attempt to override",
            )
        }
        CookRecoveryStep::ForkReplacement => refuse(
            "the recovery step forks a replacement attempt that preserves the checkpoint; it accepts no overrides",
        ),
        _ => Ok(()),
    }
}

fn selected_artifact(plan: &CookRecoveryPlan, artifact_id: Option<&str>) -> Result<Option<String>> {
    let refuse = |message: String| {
        Err(Error::validation_invalid_argument(
            "artifact-id",
            message,
            Some(plan.run_id.clone()),
            None,
        ))
    };
    match (&plan.step, artifact_id) {
        (CookRecoveryStep::ContinueWithArtifact { artifact_id: only }, None) => {
            Ok(Some(only.clone()))
        }
        (CookRecoveryStep::ContinueWithArtifact { artifact_id: only }, Some(requested)) => {
            if requested == only {
                Ok(Some(only.clone()))
            } else {
                refuse(format!(
                    "artifact `{requested}` is not the canonical patch candidate `{only}`"
                ))
            }
        }
        (CookRecoveryStep::ChooseArtifact { artifact_ids }, Some(requested)) => {
            if artifact_ids.iter().any(|id| id == requested) {
                Ok(Some(requested.to_string()))
            } else {
                refuse(format!(
                    "artifact `{requested}` is not one of the canonical patch candidates: {}",
                    artifact_ids.join(", ")
                ))
            }
        }
        (CookRecoveryStep::ChooseArtifact { artifact_ids }, None) => refuse(format!(
            "several distinct patch candidates remain; rerun with --artifact-id <one of: {}>",
            artifact_ids.join(", ")
        )),
        (_, Some(_)) => refuse(
            "--artifact-id only selects among patch candidates when the recovery step requires a choice"
                .to_string(),
        ),
        (_, None) => Ok(None),
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
    match record.state {
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
    match record.state {
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
        let one = vec!["canonical-patch".to_string()];
        assert_eq!(
            step("promotion_failed", |inputs| inputs.ambiguous_artifact_ids =
                &one),
            CookRecoveryStep::ContinueWithArtifact {
                artifact_id: "canonical-patch".to_string()
            }
        );
        let many = vec!["first-patch".to_string(), "second-patch".to_string()];
        assert_eq!(
            step("promotion_failed", |inputs| inputs.ambiguous_artifact_ids =
                &many),
            CookRecoveryStep::ChooseArtifact {
                artifact_ids: many.clone()
            }
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
        assert_eq!(
            actions(CookRecoveryStep::ChooseArtifact {
                artifact_ids: vec!["a".to_string(), "b".to_string()]
            }),
            vec![
                (
                    "resume_with_artifact".to_string(),
                    "homeboy agent-task recover cook-attempt-1 --artifact-id a".to_string()
                ),
                (
                    "resume_with_artifact".to_string(),
                    "homeboy agent-task recover cook-attempt-1 --artifact-id b".to_string()
                ),
            ]
        );
        assert!(actions(CookRecoveryStep::none("done")).is_empty());
    }

    #[test]
    fn artifact_selection_is_refused_outside_an_artifact_choice() {
        let plan = |step| CookRecoveryPlan {
            cook_id: "cook".to_string(),
            run_id: "cook-attempt-1".to_string(),
            status: "promotion_failed".to_string(),
            step,
        };
        let choose = plan(CookRecoveryStep::ChooseArtifact {
            artifact_ids: vec!["a".to_string(), "b".to_string()],
        });
        assert_eq!(
            selected_artifact(&choose, Some("b")).unwrap().as_deref(),
            Some("b")
        );
        assert!(selected_artifact(&choose, None).is_err());
        assert!(selected_artifact(&choose, Some("c")).is_err());
        assert!(selected_artifact(&plan(CookRecoveryStep::Continue), Some("a")).is_err());
        assert_eq!(
            selected_artifact(
                &plan(CookRecoveryStep::ContinueWithArtifact {
                    artifact_id: "a".to_string()
                }),
                None
            )
            .unwrap()
            .as_deref(),
            Some("a")
        );
    }
}
