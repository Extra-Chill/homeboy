//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
    AgentTaskIntentionalNoChange,
};
use crate::agent_task_dispatch_plan::{build_dispatch_plan, validate_single_cook_prompt_source};
use crate::agent_task_dispatch_service::{self, AgentTaskDispatchCommand};
use crate::agent_task_gate::VerifyGateOptions;
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{AgentTaskPromotionReport, AgentTaskPromotionStatus};
use crate::agent_task_scheduler::{
    AgentTaskExecutionBudget, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use crate::agent_task_timeout::{capture_cook_deadline, expired_cook_deadline, CookDeadline};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::cook_status::{CookDisposition, CookStatus};
use homeboy_core::run_lifecycle_status::RunLifecycleStatus;
use homeboy_core::{Error, Result};

use super::cook_activity::{CookActivityProbe, CookProviderActivity};
use super::cook_baseline::{
    compare_gate_failures_to_verified_base, cook_attempt_harvest_context,
    materialize_follow_up_baseline, materialize_initial_candidate_baseline,
    re_materialize_follow_up_baseline, CookFollowUpBaseline, DerivedCookBaselineCapability,
};
use super::cook_budget::{
    budget_remaining, execution_budget_usage, reserve_remediation_budget,
    validate_effective_cook_budget, ExecutionBudgetUsage,
};
use super::cook_pre_execution::{
    materialize_cook_attempt, materialize_initial_cook_attempt, pre_execution_failure_details,
    pre_execution_failure_phase, pre_execution_failure_report, record_pre_execution_failure,
    retryable_pre_execution_failure, terminal_executor_matches, with_pre_execution_phase,
};
use super::cook_promotion::{
    attempt_needs_execution, cook_report, finalize_or_load_cook_pr,
    is_moving_base_finalization_error, moving_base_recovery_for_run,
    moving_base_recovery_from_promotion, moving_base_recovery_report, next_moving_base_recovery,
    persisted_promotion_for_attempt, promote_or_load_attempt, recover_moving_base_cook_candidate,
    refreshed_moving_base_recovery, retryable_provider_discovery_failure, CookReportInput,
    MovingBaseCookRecovery,
};
use super::cook_supervision::{resolve_supervision_policy, CookSupervisor};
use super::execution::run_loaded_plan_with_derived_cook_baseline;
use super::AgentTaskRunResult;

/// Lease window for a cook promotion operation claim. Long enough that a healthy
/// controller finishes promoting and records the result within it; a crashed
/// controller's lease elapses so a resumed pass can reconcile and continue.
const PROMOTION_CLAIM_LEASE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Durable operation key for the promotion of one cook attempt. Promotion is
/// one-per-`run_id`, so the run id is the stable operation identity (#8357).
fn promotion_operation_key(run_id: &str) -> String {
    format!("promote:{run_id}")
}

/// Lease window for a retry-dispatch operation claim. A detached dispatch may
/// take a while to be accepted by the runner; the lease is generous so a healthy
/// controller completes it, while a crashed controller's lease still elapses.
const RETRY_DISPATCH_CLAIM_LEASE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// A missing aggregate is a controller interruption, not provider output. Keep
/// its claim separate from retry dispatch because no next run exists yet.
const PRE_ARTIFACT_INTERRUPTION_CLAIM_LEASE: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// Durable operation key for the dispatch of one retry attempt. A retry is
/// one-per-generated-`run_id`, so the next run id is the stable identity (#8357).
fn retry_dispatch_operation_key(next_run_id: &str) -> String {
    format!("dispatch:{next_run_id}")
}

fn pre_artifact_interruption_operation_key(run_id: &str) -> String {
    format!("pre-artifact-interruption:{run_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreArtifactInterruptionPhase {
    BeforeProviderStart,
    DuringProviderExecution,
    AfterProviderReturn,
}

impl PreArtifactInterruptionPhase {
    fn name(self) -> &'static str {
        match self {
            Self::BeforeProviderStart => "before_provider_start",
            Self::DuringProviderExecution => "during_provider_execution",
            Self::AfterProviderReturn => "after_provider_return_before_aggregate_persistence",
        }
    }
}

/// Classify a terminal attempt without an aggregate from the durable provider
/// ledger. A reservation is proof that a provider started; absent reservations
/// consume nothing. This deliberately does not manufacture aggregate events.
fn pre_artifact_interruption_phase(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> PreArtifactInterruptionPhase {
    let executions = record.metadata["provider_executions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if executions.is_empty() {
        PreArtifactInterruptionPhase::BeforeProviderStart
    } else if executions
        .iter()
        .any(|execution| execution["state"] == "running")
    {
        PreArtifactInterruptionPhase::DuringProviderExecution
    } else {
        PreArtifactInterruptionPhase::AfterProviderReturn
    }
}

fn intentional_no_change_from_aggregate(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
) -> Option<AgentTaskIntentionalNoChange> {
    aggregate.outcomes.iter().find_map(|outcome| {
        let declaration = outcome
            .outputs
            .get("provider_run_result")?
            .get("intentional_no_change")?
            .clone();
        serde_json::from_value::<AgentTaskIntentionalNoChange>(declaration).ok()
    })
}

fn pre_artifact_execution_count(record: &agent_task_lifecycle::AgentTaskRunRecord) -> u32 {
    record.metadata["provider_executions"]
        .as_array()
        .map(|executions| executions.len().try_into().unwrap_or(u32::MAX))
        .unwrap_or_default()
}

fn pre_artifact_interruption_report(
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    run_id: &str,
    phase: PreArtifactInterruptionPhase,
    reason: String,
    exit_code: i32,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let mut report = cook_report(CookReportInput {
        cook_id,
        status: "pre_artifact_interruption",
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some(reason),
        exit_code,
        invocation_latest_run_id: Some(run_id),
    });
    report.value.terminal_phase = Some(phase.name().to_string());
    report.value.terminal_failure_classification = Some("pre_artifact_interruption".to_string());
    report
}

/// Terminalize a Cook that ran out of wall-clock budget.
///
/// Reported as [`CookStatus::TimedOut`] — the existing vocabulary entry for
/// "exceeded its wall-clock budget" — rather than a new status, so the batch
/// totals, exit-code classification, and terminality rules that already
/// understand `timed_out` need no teaching. Exit code is 1: the Cook did not
/// finish the work it was asked to do, and an operator has something to act on.
///
/// [`CookDisposition::Terminal`] is declared explicitly: nothing is left
/// carrying this Cook forward, so the orchestrator's completion is due now.
fn cook_deadline_report(
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    run_id: &str,
    deadline: &CookDeadline,
    boundary: &str,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let mut report = cook_report(CookReportInput {
        cook_id,
        // The existing vocabulary entry for "exceeded its wall-clock budget"
        // ([`CookStatus::TimedOut`]). Written as the literal every other exit
        // in this file uses; the linkage is pinned by test.
        status: "timed_out",
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some(deadline.stop_reason(boundary)),
        exit_code: 1,
        invocation_latest_run_id: Some(run_id),
    });
    report.value.terminal_phase = Some("cook_deadline".to_string());
    report.value.terminal_failure_classification = Some("timed_out".to_string());
    report
}

/// Claim exactly one recipe continuation for a terminal run that never
/// persisted an aggregate. The claim is on the interrupted run, so concurrent
/// controllers converge before they can append competing recipe attempts.
fn claim_pre_artifact_interruption_retry(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<Option<(u32, String)>> {
    let next_attempt = attempt.checked_add(1).ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "durable cook attempt sequence is exhausted",
            Some(cook_id.to_string()),
            None,
        )
    })?;
    let operation_key = pre_artifact_interruption_operation_key(run_id);
    let recipe_next_attempt = || {
        super::load_recipe(cook_id).map(|recipe| {
            recipe
                .attempts
                .iter()
                .find(|recorded| recorded.attempt == next_attempt && recorded.plan == *plan)
                .map(|recorded| recorded.run_id.clone())
        })
    };

    match agent_task_lifecycle::claim_cook_operation(
        run_id,
        &operation_key,
        PRE_ARTIFACT_INTERRUPTION_CLAIM_LEASE,
    )? {
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            let next_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, next_attempt);
            super::record_recipe_attempt(cook_id, next_attempt, &next_run_id, plan)?;
            agent_task_lifecycle::complete_cook_operation(
                run_id,
                &operation_key,
                serde_json::json!({
                    "next_attempt": next_attempt,
                    "next_run_id": next_run_id,
                }),
            )?;
            Ok(Some((next_attempt, next_run_id)))
        }
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            let recorded_attempt = result["next_attempt"].as_u64();
            let recorded_run_id = result["next_run_id"].as_str();
            if let Some(next_run_id) = recipe_next_attempt()? {
                if recorded_attempt == Some(u64::from(next_attempt))
                    && recorded_run_id == Some(next_run_id.as_str())
                {
                    return Ok(Some((next_attempt, next_run_id)));
                }
            }
            {
                Err(Error::internal_unexpected(
                    "pre-artifact interruption continuation claim conflicts with the durable cook recipe",
                ))
            }
        }
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            // A crash after recipe append but before claim completion is safe to
            // finish: the immutable next attempt is already fully identified.
            if let Some(next_run_id) = recipe_next_attempt()? {
                agent_task_lifecycle::complete_cook_operation(
                    run_id,
                    &operation_key,
                    serde_json::json!({
                        "next_attempt": next_attempt,
                        "next_run_id": next_run_id,
                    }),
                )?;
                Ok(Some((next_attempt, next_run_id)))
            } else {
                Ok(None)
            }
        }
    }
}

/// Lease window for a finalization operation claim. PR finalization (commit,
/// push, `gh pr create`) can take a while; the lease is generous so a healthy
/// controller completes it, while a crashed controller's lease still elapses.
const FINALIZATION_CLAIM_LEASE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Foreground liveness is deliberately bounded. Provider-native progress still
/// wins when available; this durable heartbeat covers quiet providers.
const COOK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// One Cook progress event, as delivered to a foreground observer.
///
/// This used to be three bare `&str`s — `(phase, cook_id, run_id)` — which is
/// why every heartbeat an operator saw was byte-identical and carried nothing
/// about the run it described. `report_cook_progress` already received the
/// attempt and a detail string and recorded them durably; both were thrown away
/// at the observer boundary. Passing a struct means adding a fact is an
/// additive change here rather than a signature change at every call site, so
/// the boundary stops being the place state goes to die (#11482).
#[derive(Clone, Copy, Debug)]
pub struct CookProgressEvent<'a> {
    pub phase: &'a str,
    pub cook_id: &'a str,
    pub run_id: &'a str,
    /// Which provider attempt this event belongs to.
    pub attempt: u32,
    /// Controller-owned description of the phase.
    pub detail: Option<&'a str>,
    /// What the provider is doing right now, when it is observable.
    pub activity: Option<&'a CookProviderActivity>,
}

impl CookProgressEvent<'_> {
    /// Bounded one-line rendering of the provider's current activity.
    pub fn activity_summary(&self) -> Option<String> {
        self.activity.and_then(CookProviderActivity::summary_line)
    }
}

pub type CookProgressObserver<'a> = dyn Fn(&CookProgressEvent<'_>) -> Result<()> + Send + Sync + 'a;

fn report_cook_progress(
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
) -> Result<()> {
    report_cook_progress_with_activity(observer, cook_id, run_id, phase, attempt, detail, None)
}

fn report_cook_progress_with_activity(
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
    activity: Option<&CookProviderActivity>,
) -> Result<()> {
    agent_task_lifecycle::record_cook_progress_with_activity(
        run_id,
        phase,
        attempt,
        detail,
        activity.and_then(CookProviderActivity::to_record_value),
    )?;
    let event = CookProgressEvent {
        phase,
        cook_id,
        run_id,
        attempt,
        detail,
        activity,
    };
    if let Some(observer) = observer {
        if let Err(error) = observer(&event) {
            // The observer is the submitting client's output channel. Once the
            // progress record exists, a broken client pipe is evidence about
            // observation, never authority to stop promotion or finalization.
            let _ = agent_task_lifecycle::record_cook_observer_event(
                run_id,
                phase,
                bounded_error_diagnostic(&error),
            );
        }
    }
    Ok(())
}

/// Durable operation key for finalizing one cook candidate. Keyed by the run id
/// plus the promoted candidate fingerprint (patch SHA), so re-finalizing the
/// same candidate is idempotent while a genuinely different candidate finalizes
/// on its own claim (#8357).
fn finalization_operation_key(run_id: &str, promotion: &AgentTaskPromotionReport) -> String {
    match promotion.patch_artifact.sha256.as_deref() {
        Some(sha) => format!("finalize:{run_id}:{sha}"),
        None => format!("finalize:{run_id}"),
    }
}

/// Finalize a cook candidate under a durable exactly-once operation claim.
///
/// PR finalization performs its external effects (commit, push, `gh pr create`)
/// and only then records the result. A controller crash after the PR is created
/// but before the result is durable would open a second PR on restart. The claim
/// closes it: reserve `finalize:<run_id>:<sha>` before the effect and complete it
/// with the finalization result after it is durable. A resumed pass revalidates
/// the existing PR idempotently, including its live Git and GitHub identities
/// (#8357).
fn finalize_with_operation_claim(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
    finalize: &mut dyn FnMut(
        &AgentTaskCookServiceOptions,
        &str,
        &AgentTaskPromotionReport,
    ) -> Result<Value>,
) -> Result<Value> {
    let operation_key = finalization_operation_key(run_id, promotion);
    match agent_task_lifecycle::claim_cook_operation(
        run_id,
        &operation_key,
        FINALIZATION_CLAIM_LEASE,
    )? {
        // A completed claim only proves an earlier publication. Re-run the
        // idempotent finalizer so a later force-push or PR-head mutation cannot
        // be returned as a still-valid publication.
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_) => {
            finalize(options, run_id, promotion)
        }
        // A concurrent pass owns a fresh lease. The load-or-finalize path still
        // returns an already-recorded finalization when present; do not mark the
        // claim completed from here — its owner does.
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => finalize(options, run_id, promotion),
        // This pass owns the operation. Finalize, then record the result as the
        // claim's immutable completion.
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            let finalization = match finalize(options, run_id, promotion) {
                Ok(finalization) => finalization,
                Err(error) => {
                    agent_task_lifecycle::fail_cook_operation(
                        run_id,
                        &operation_key,
                        bounded_error_diagnostic(&error),
                    )?;
                    return Err(error);
                }
            };
            agent_task_lifecycle::complete_cook_operation(
                run_id,
                &operation_key,
                finalization.clone(),
            )?;
            Ok(finalization)
        }
    }
}

/// Promote a cook attempt under a durable exactly-once operation claim.
///
/// `promote_or_load_attempt` already loads an already-persisted promotion, but
/// the fresh-promote path performs its external effect (`promote_attempt`) and
/// only then records the result. A controller crash in that window re-runs the
/// effect on restart. The claim closes it: reserve `promote:<run_id>` before the
/// effect, complete it after the result is durable, and on a resumed pass return
/// the persisted promotion instead of repeating the effect (#8357).
fn promote_with_operation_claim(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let operation_key = promotion_operation_key(run_id);
    match agent_task_lifecycle::claim_cook_operation(run_id, &operation_key, PROMOTION_CLAIM_LEASE)?
    {
        // A prior pass already promoted and recorded the result. Load the durable
        // promotion rather than repeating the external effect.
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_) => {
            promote_or_load_attempt(options, run_id)
        }
        // Another pass holds a still-fresh lease. The persisted-promotion read in
        // `promote_or_load_attempt` still resolves an already-produced promotion;
        // if none exists yet, promotion proceeds (content-addressed and idempotent
        // on disk). Do not mark the claim completed from here — its owner does.
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            let claim = agent_task_lifecycle::operation_claim(run_id, &operation_key)?;
            let mut error = Error::validation_invalid_argument(
                "promotion_operation",
                "operation_in_progress",
                Some(operation_key),
                Some(vec![format!("homeboy agent-task cook-continue {run_id}")]),
            );
            error.details["claim"] = serde_json::to_value(claim).unwrap_or(Value::Null);
            Err(error)
        }
        // This pass owns the operation. Promote, then record the result as the
        // claim's immutable completion.
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            let promotion = match promote_or_load_attempt(options, run_id) {
                Ok(promotion) => promotion,
                Err(error) => {
                    agent_task_lifecycle::fail_cook_operation(
                        run_id,
                        &operation_key,
                        bounded_error_diagnostic(&error),
                    )?;
                    return Err(error);
                }
            };
            agent_task_lifecycle::complete_cook_operation(
                run_id,
                &operation_key,
                serde_json::to_value(&promotion)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
            )?;
            Ok(promotion)
        }
    }
}

fn bounded_error_diagnostic(error: &Error) -> Value {
    let mut details = homeboy_core::redaction::redact_json(&error.details);
    bound_diagnostic_value(&mut details, 0);
    serde_json::json!({
        "status": "failed",
        "code": format!("{:?}", error.code),
        "message": truncate_diagnostic_text(&homeboy_core::redaction::redact_string(&error.message)),
        "details": details,
    })
}

fn bound_diagnostic_value(value: &mut Value, depth: usize) {
    if depth >= 4 {
        *value = Value::String("[omitted: diagnostic depth limit]".to_string());
        return;
    }
    match value {
        Value::String(text) => *text = truncate_diagnostic_text(text),
        Value::Array(items) => {
            items.truncate(8);
            for item in items {
                bound_diagnostic_value(item, depth + 1);
            }
        }
        Value::Object(entries) => {
            for item in entries.values_mut() {
                bound_diagnostic_value(item, depth + 1);
            }
        }
        _ => {}
    }
}

fn truncate_diagnostic_text(text: &str) -> String {
    const LIMIT: usize = 2048;
    if text.len() <= LIMIT {
        text.to_string()
    } else {
        let prefix: String = text.chars().take(LIMIT).collect();
        format!("{prefix}...[truncated]")
    }
}

/// The generic cook side-effect boundary the attempt loop drives its external
/// effects through: promotion, moving-base recovery, and PR finalization.
///
/// Routing every external effect through one injectable object (rather than a
/// mix of free-function calls and ad-hoc closures) gives durable exactly-once
/// operation claims a single wiring point, and lets deterministic tests inject
/// side effects without real Git/GitHub mutations (#8357). Promotion is wired
/// through the claim primitive here (`promote_with_operation_claim`); retry
/// dispatch and finalization follow as separate slices.
pub(crate) trait CookSideEffectService {
    /// Promote the successful candidate for `run_id`, or load the already-persisted
    /// promotion when this attempt was interrupted after promoting.
    fn promote(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
    ) -> Result<AgentTaskPromotionReport>;

    /// Rebase and re-verify a candidate whose base moved under it.
    fn recover_moving_base(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        recovery: &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport>;

    /// Commit, push, and open/update the PR for a green promoted candidate, or
    /// load the already-finalized PR when this attempt was interrupted after
    /// finalizing.
    fn finalize(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
        promotion: &AgentTaskPromotionReport,
    ) -> Result<Value>;
}

/// Production cook side-effect boundary. Each method delegates to the existing
/// promotion/finalization free functions, so behavior is identical to the prior
/// direct calls; the trait only relocates the call sites behind one seam.
pub(crate) struct DefaultCookSideEffects<F> {
    finalize: F,
}

impl<F> DefaultCookSideEffects<F>
where
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
{
    pub(crate) fn new(finalize: F) -> Self {
        Self { finalize }
    }
}

impl<F> CookSideEffectService for DefaultCookSideEffects<F>
where
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
{
    fn promote(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
    ) -> Result<AgentTaskPromotionReport> {
        promote_with_operation_claim(options, run_id)
    }

    fn recover_moving_base(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        recovery: &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport> {
        recover_moving_base_cook_candidate(options, recovery)
    }

    fn finalize(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
        promotion: &AgentTaskPromotionReport,
    ) -> Result<Value> {
        finalize_with_operation_claim(options, run_id, promotion, &mut self.finalize)
    }
}

/// Test cook side-effect boundary with injectable `finalize` and
/// `recover_moving_base` closures, so recovery/finalization control flow can be
/// exercised without real Git/GitHub mutations. `promote` delegates to the real
/// promotion path (tests that need to intercept promotion persist a promotion
/// first, exactly as before).
#[cfg(test)]
pub(crate) struct TestCookSideEffects<F, R> {
    finalize: F,
    recover: R,
}

#[cfg(test)]
impl<F, R> TestCookSideEffects<F, R>
where
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
    R: FnMut(
        &AgentTaskCookServiceOptions,
        &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport>,
{
    pub(crate) fn new(finalize: F, recover: R) -> Self {
        Self { finalize, recover }
    }
}

#[cfg(test)]
impl<F, R> CookSideEffectService for TestCookSideEffects<F, R>
where
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
    R: FnMut(
        &AgentTaskCookServiceOptions,
        &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport>,
{
    fn promote(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
    ) -> Result<AgentTaskPromotionReport> {
        promote_or_load_attempt(options, run_id)
    }

    fn recover_moving_base(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        recovery: &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport> {
        (self.recover)(options, recovery)
    }

    fn finalize(
        &mut self,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
        promotion: &AgentTaskPromotionReport,
    ) -> Result<Value> {
        (self.finalize)(options, run_id, promotion)
    }
}

/// The promotion checkpoint captures this before gates run, when it is the only
/// complete authorization for reusing the dirty managed destination.
pub(crate) fn gate_feedback_current_diff(promotion: &AgentTaskPromotionReport) -> String {
    promotion
        .provenance
        .pointer("/gate_feedback_baseline/current_diff")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Parse the AI-authored review form off the terminal attempt outcome.
///
/// The agent emits it under `outputs["review_form"]`; the terminal outcome is
/// the last one recorded in the aggregate. Returns `Ok(None)` when the agent
/// emitted no form (the loop treats absence as a gap and nudges a retry). A
/// present-but-malformed form is a hard error so garbage is never rendered.
fn review_form_from_aggregate(
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> Result<Option<crate::agent_task_review_dossier::AiFilledReviewForm>> {
    let Some(outcome) = aggregate.selected_outcome().or_else(|| {
        (aggregate.outcomes.len() == 1)
            .then(|| aggregate.outcomes.first())
            .flatten()
    }) else {
        return Ok(None);
    };
    crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(&outcome.outputs)
}

/// Project the PR-dossier contract before the first finalizing provider attempt
/// is persisted or executed. Standalone and no-finalize requests retain their
/// caller-defined contract.
fn project_initial_finalizing_review_form_contract(options: &mut AgentTaskCookServiceOptions) {
    if options.no_finalize {
        return;
    }

    for request in &mut options.initial_plan.tasks {
        request.output_declarations.retain(|declaration| {
            declaration.name != crate::agent_task_review_dossier::AI_REVIEW_FORM_OUTPUT_KEY
        });
        request
            .output_declarations
            .push(crate::agent_task_review_dossier::review_form_output_declaration());
        if !request.instructions.contains("reviewer-facing PR dossier") {
            request.instructions.push_str(
                "\n\nProvide the reviewer-facing PR dossier in `outputs.review_form`. Return an object with `summary` (the change and its purpose), `what_changed` (concrete change bullets), qualitative `compatibility` (impact assessment), optional structured `verification` entries (exact command plus total/passed/failed/ignored counts), and `used_for` (a concise reflection of the process used). Homeboy links verification entries to durable candidate gate evidence. A successful response supplies specific, complete content for every field so Homeboy can finalize a clear pull request.",
            );
        }
    }
}

/// Executes one provider attempt while cook retains ownership of promotion,
/// gates, retries, and finalization.
pub trait AgentTaskCookAttemptDispatcher: Send + Sync + std::fmt::Debug {
    /// Durable, generic transport descriptor used to reconstruct this
    /// dispatcher in a fresh controller process.
    fn durable_recipe(&self) -> Result<Value>;

    /// Establish transport readiness before the cook pins its runtime
    /// generation. A reconnect can promote the runner runtime, which must not
    /// wait on the cook that needs that reconnect.
    fn prepare_for_cook(&self) -> Result<()> {
        Ok(())
    }

    /// External transports identify dispatch failures before a provider can
    /// execute so candidate recovery remains distinct from provider failures.
    fn pre_execution_failure_phase(&self) -> &'static str {
        "cook_pre_execution"
    }

    /// `derived_cook_baseline` is process-local authority for a gate-fix retry.
    /// Implementations must not serialize it into the provider request.
    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct AgentTaskCookServiceOptions {
    pub cook_id: String,
    pub initial_run_id: String,
    /// Controller-compiled first attempt. The cook service owns dispatching it
    /// through the same local-or-Lab transport used by gate-feedback retries.
    pub initial_plan: AgentTaskPlan,
    pub to_worktree: String,
    pub source_worktree_path: Option<PathBuf>,
    pub provider_command: Option<String>,
    pub provider_invocation: Option<CommandInvocation>,
    /// Shared deterministic verification gate fields, factored out of the
    /// per-field duplication that previously spanned the loop/promote types.
    pub gates: VerifyGateOptions,
    pub max_attempts: u32,
    pub no_finalize: bool,
    /// Publish a newly created PR as a draft while retaining normal finalization.
    pub draft_pr: bool,
    pub base: String,
    pub task_base_sha: Option<String>,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub source_refs: Vec<String>,
    pub protected_branches: Vec<String>,
    pub ai_tool: String,
    pub ai_model: Option<String>,
    pub ai_used_for: String,
    /// The route-selected provider transport. `None` executes locally.
    pub attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
    pub harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
}

const COOK_CONTINUE_ROUTE_SCHEMA: &str = "homeboy/agent-task-cook-continue-route/v1";

/// Record that the `cook-continue` lifecycle route selected this exact attempt.
/// This is durable route authority, not caller-controlled plan metadata.
pub fn authorize_cook_continue_route(options: &AgentTaskCookServiceOptions) -> Result<()> {
    agent_task_lifecycle::exact_record(&options.initial_run_id)?;
    agent_task_lifecycle::record_metadata_value(
        &options.initial_run_id,
        "cook_continue_route",
        serde_json::json!({
            "schema": COOK_CONTINUE_ROUTE_SCHEMA,
            "cook_id": options.cook_id,
            "run_id": options.initial_run_id,
        }),
    )
}

fn has_cook_continue_route(options: &AgentTaskCookServiceOptions) -> bool {
    agent_task_lifecycle::exact_record(&options.initial_run_id)
        .ok()
        .and_then(|record| record.metadata.get("cook_continue_route").cloned())
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|context| {
            context.get("schema").and_then(Value::as_str) == Some(COOK_CONTINUE_ROUTE_SCHEMA)
                && context.get("cook_id").and_then(Value::as_str) == Some(options.cook_id.as_str())
                && context.get("run_id").and_then(Value::as_str)
                    == Some(options.initial_run_id.as_str())
        })
}

/// Provenance supplied when Homeboy adopts a candidate prepared outside its
/// provider lifecycle.
#[derive(Debug, Clone, Default)]
pub struct AgentTaskCandidateAdoptionOptions {
    pub ai_model: Option<String>,
    pub replace_interrupted: bool,
}

/// A Cook outcome.
///
/// `Serialize` is written by hand rather than derived so the additive
/// [`RunLifecycleProjection`] is emitted by *every* producer of this type
/// without each of the ~30 struct literals that build it having to carry three
/// more fields it would then be free to compute differently. The projection is
/// derived from `status` and `disposition`, which are already present, so
/// there is nothing for a call site to supply and nothing for it to get wrong.
#[derive(Debug, Clone)]
pub struct AgentTaskCookReport {
    pub schema: &'static str,
    pub cook_id: String,
    pub latest_run_id: Option<String>,
    pub history_run_ids: Vec<String>,
    pub invocation_run_ids: Vec<String>,
    pub status: String,
    /// Whether the Cook will advance on its own, declared by the exit that
    /// produced this report rather than inferred from `status`.
    ///
    /// Consumers waiting on completion should read this instead of
    /// pattern-matching `status`, which is an open vocabulary.
    pub disposition: CookDisposition,
    pub attempts: Vec<AgentTaskCookAttemptReport>,
    pub finalization: Option<Value>,
    /// A verified review outcome that intentionally has no candidate to promote
    /// or finalize.
    pub intentional_no_change: Option<AgentTaskCookIntentionalNoChangeReport>,
    /// Candidate authority is separate from `latest_run_id`, which remains the
    /// chronological invocation/index compatibility field.
    pub selected_candidate: Option<Value>,
    pub stop_reason: Option<String>,
    /// Preserves the lifecycle-owned failure boundary when cook stops before
    /// provider dispatch instead of collapsing it into an attempt-budget result.
    pub terminal_phase: Option<String>,
    pub terminal_failure_classification: Option<String>,
    pub moving_base_recovery: Option<MovingBaseCookRecovery>,
    /// Generic durable recovery coordinates for a Cook that stopped after its
    /// recipe was materialized. This intentionally contains no provider or gate
    /// evidence; operators retrieve that through the listed diagnose command.
    pub failure_context: Option<AgentTaskCookFailureContext>,
}

/// The closed-vocabulary classification emitted beside an open `status`
/// string.
///
/// # Why a report carries this at all
///
/// `status` is an open vocabulary and stays that way — callers match on it and
/// several exits feed it straight from finalization JSON. `exit_code` is the
/// contract every *process* caller branches on, but a consumer reading a
/// persisted report or an HTTP response has no exit code. Without this, such a
/// consumer receiving `{"status": "moving_base"}` cannot decide success,
/// completion, or retry at all.
///
/// # Where each field's authority comes from
///
/// - `lifecycle_status` is [`RunLifecycleStatus`], projected from
///   [`CookStatus`]. Closed, so it is always decidable.
/// - `terminal` is the *declared* [`CookDisposition`], never re-derived from
///   the status string. The exit that produced the report already knew whether
///   it handed the work to a durable owner; that answer is authoritative and
///   this only forwards it.
/// - `retryable` is [`RunLifecycleStatus::is_retryable`], gated on `terminal`.
///   A Cook a durable owner is still carrying must never be advertised as
///   retryable, whatever its status string reads.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RunLifecycleProjection {
    pub lifecycle_status: RunLifecycleStatus,
    pub terminal: bool,
    pub retryable: bool,
}

impl RunLifecycleProjection {
    /// Classify a report whose exit declared a [`CookDisposition`].
    pub fn from_status_and_disposition(status: &str, disposition: CookDisposition) -> Self {
        Self::new(
            RunLifecycleStatus::from(&CookStatus::from_status(status)),
            disposition.is_terminal(),
        )
    }

    /// Classify a report that has no declared disposition — a batch cell whose
    /// child never produced one, for instance.
    ///
    /// Terminality then falls back to [`CookStatus::is_in_flight`], the
    /// vocabulary's own closed reading of "still progressing". That is the
    /// existing status-only answer documented in `cook_status`, not a new
    /// opinion: an unrecognized status reads as not-in-flight there, and reads
    /// as terminal here.
    pub fn from_status(status: &str) -> Self {
        let status = CookStatus::from_status(status);
        let terminal = !status.is_in_flight();
        Self::new(RunLifecycleStatus::from(&status), terminal)
    }

    fn new(lifecycle_status: RunLifecycleStatus, terminal: bool) -> Self {
        Self {
            lifecycle_status,
            terminal,
            // Gated on terminality so a Cook still being carried by a durable
            // owner is never advertised as something to retry.
            retryable: terminal && lifecycle_status.is_retryable(),
        }
    }
}

impl serde::Serialize for AgentTaskCookReport {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        /// Mirrors [`AgentTaskCookReport`] field for field, in order, with the
        /// same `skip_serializing_if` rules, plus the three appended
        /// projection fields. Borrowed so serialization stays
        /// allocation-free.
        #[derive(serde::Serialize)]
        struct Wire<'a> {
            schema: &'static str,
            cook_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            latest_run_id: Option<&'a String>,
            #[serde(skip_serializing_if = "slice_is_empty")]
            history_run_ids: &'a [String],
            #[serde(skip_serializing_if = "slice_is_empty")]
            invocation_run_ids: &'a [String],
            status: &'a str,
            disposition: CookDisposition,
            attempts: &'a [AgentTaskCookAttemptReport],
            #[serde(skip_serializing_if = "Option::is_none")]
            finalization: Option<&'a Value>,
            #[serde(skip_serializing_if = "Option::is_none")]
            intentional_no_change: Option<&'a AgentTaskCookIntentionalNoChangeReport>,
            #[serde(skip_serializing_if = "Option::is_none")]
            selected_candidate: Option<&'a Value>,
            #[serde(skip_serializing_if = "Option::is_none")]
            stop_reason: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            terminal_phase: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            terminal_failure_classification: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            moving_base_recovery: Option<&'a MovingBaseCookRecovery>,
            #[serde(skip_serializing_if = "Option::is_none")]
            failure_context: Option<&'a AgentTaskCookFailureContext>,
            // Additive. `status` above is unchanged and remains the field
            // callers match on.
            lifecycle_status: RunLifecycleStatus,
            terminal: bool,
            retryable: bool,
        }

        let lifecycle = self.lifecycle();
        let wire = Wire {
            schema: self.schema,
            cook_id: &self.cook_id,
            latest_run_id: self.latest_run_id.as_ref(),
            history_run_ids: &self.history_run_ids,
            invocation_run_ids: &self.invocation_run_ids,
            status: &self.status,
            disposition: self.disposition,
            attempts: &self.attempts,
            finalization: self.finalization.as_ref(),
            intentional_no_change: self.intentional_no_change.as_ref(),
            selected_candidate: self.selected_candidate.as_ref(),
            stop_reason: self.stop_reason.as_ref(),
            terminal_phase: self.terminal_phase.as_ref(),
            terminal_failure_classification: self.terminal_failure_classification.as_ref(),
            moving_base_recovery: self.moving_base_recovery.as_ref(),
            failure_context: self.failure_context.as_ref(),
            lifecycle_status: lifecycle.lifecycle_status,
            terminal: lifecycle.terminal,
            retryable: lifecycle.retryable,
        };
        serde::Serialize::serialize(&wire, serializer)
    }
}

impl AgentTaskCookReport {
    /// The closed-vocabulary classification of this report.
    pub fn lifecycle(&self) -> RunLifecycleProjection {
        RunLifecycleProjection::from_status_and_disposition(&self.status, self.disposition)
    }
}

/// `skip_serializing_if` predicate for a borrowed slice field.
///
/// Serde passes the predicate a reference to the field, so a `&'a [T]` field
/// arrives as `&&[T]`. Spelled out rather than relying on `<[T]>::is_empty`
/// resolving through an extra layer of deref.
fn slice_is_empty<T>(value: &&[T]) -> bool {
    value.is_empty()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookIntentionalNoChangeReport {
    #[serde(flatten)]
    pub declaration: AgentTaskIntentionalNoChange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_form: Option<crate::agent_task_review_dossier::AiFilledReviewForm>,
}

/// Durable identity and legal recovery surface for a failed Cook.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookFailureContext {
    pub cook_id: String,
    pub latest_run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion_provenance: Option<Value>,
    pub durable_recipe_ref: String,
    pub lifecycle_state: String,
    pub phase: String,
    pub reason_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Value>,
    /// Bounded, durable admission predicates for a denied historical
    /// continuation. It intentionally contains no provider output or evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_admission: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_claim: Option<Value>,
    pub provider_budget_consumed: bool,
    pub provider_executions_consumed: u64,
    pub recovery_legal: bool,
    pub recovery_reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legal_actions: Vec<AgentTaskCookRecoveryAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<AgentTaskCookRecoveryAction>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ContinuationAdmissionTrace {
    schema: &'static str,
    predicates: Vec<ContinuationAdmissionPredicate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_authoritative_denial: Option<&'static str>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ContinuationAdmissionPredicate {
    name: &'static str,
    outcome: &'static str,
}

impl ContinuationAdmissionTrace {
    fn new() -> Self {
        Self {
            schema: "homeboy/cook-continuation-admission/v1",
            predicates: Vec::new(),
            first_authoritative_denial: None,
        }
    }

    fn pass(&mut self, name: &'static str) {
        self.predicates.push(ContinuationAdmissionPredicate {
            name,
            outcome: "pass",
        });
    }

    fn deny(&mut self, name: &'static str, outcome: &'static str) {
        self.predicates
            .push(ContinuationAdmissionPredicate { name, outcome });
        if self.first_authoritative_denial.is_none() {
            self.first_authoritative_denial = Some(name);
        }
    }

    fn value(&self) -> Value {
        serde_json::to_value(self).expect("continuation admission trace serializes")
    }
}

/// An exact command that is legal for the durable Cook state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookRecoveryAction {
    pub action: String,
    pub command: String,
}

/// A bounded collection of independently durable cooks. Each cook retains the
/// same dispatch, retry, promotion, and lifecycle path as `run_cook`.
#[derive(Debug, Clone)]
pub struct AgentTaskCookBatchOptions {
    pub batch_id: String,
    pub cooks: Vec<AgentTaskCookServiceOptions>,
    pub max_concurrency: usize,
}

/// A batch child's failure, kept structured across the coordinator boundary.
///
/// # Why this is not a `String`
///
/// A `homeboy` [`Error`] carries `code`, `retryable`, `details` (including the
/// offending `field` for validation failures), and `hints`. The Cook's own
/// failure path already uses all of it to build an
/// [`AgentTaskCookFailureContext`] with typed `legal_actions` and
/// `next_actions`. At the batch boundary every one of those collapsed into
/// `error.to_string()` — so a child that failed *before* creating a durable
/// record handed the orchestrator prose and nothing to act on, which is
/// precisely the case where there is no durable record to go and read instead.
///
/// `message` is the same string the field used to hold, so a renderer that
/// printed `error` now prints `error.message` and shows the same text.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookCellError {
    /// The stable `homeboy` error code, e.g. `validation.invalid_argument`.
    /// This is the field a machine consumer branches on.
    pub code: String,
    /// Human-readable rendering — exactly what `error` used to carry.
    pub message: String,
    /// Whether the failure classified itself as retryable.
    ///
    /// `None` means the constructor did not classify it, which is materially
    /// different from "classified as not retryable" and is preserved as such
    /// rather than being flattened to `false`.
    pub retryable: Option<bool>,
    /// The argument or configuration key at fault, when the error named one.
    /// Lifted out of `details` because it is the field an orchestrator routes
    /// on; `details` still carries it too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The error's structured evidence, verbatim.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
    /// Operator-facing remediation the error already carried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl AgentTaskCookCellError {
    /// Project a `homeboy` [`Error`] without losing its structure.
    pub fn from_error(error: &Error) -> Self {
        Self {
            code: error.code.as_str().to_string(),
            message: error.message.clone(),
            retryable: error.retryable,
            field: error
                .details
                .get("field")
                .and_then(Value::as_str)
                .map(str::to_string),
            details: error.details.clone(),
            hints: error
                .hints
                .iter()
                .map(|hint| hint.message.clone())
                .collect(),
        }
    }

    /// A failure the coordinator itself declares, with no underlying [`Error`]
    /// to project.
    ///
    /// The code is explicit rather than defaulted so a call site cannot
    /// silently emit an uninformative one.
    pub fn declared(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retryable: Some(retryable),
            field: None,
            details: Value::Null,
            hints: Vec::new(),
        }
    }
}

impl From<&Error> for AgentTaskCookCellError {
    fn from(error: &Error) -> Self {
        Self::from_error(error)
    }
}

/// One child's outcome inside a batch.
///
/// `Serialize` is hand-written for the same reason as [`AgentTaskCookReport`]:
/// the lifecycle projection is derived from fields already present, so it is
/// emitted for every producer instead of being a fourth thing a call site can
/// forget or contradict.
#[derive(Debug, Clone)]
pub struct AgentTaskCookBatchCellReport {
    pub cook_id: String,
    pub initial_run_id: String,
    pub status: String,
    pub exit_code: i32,
    pub result: Option<AgentTaskCookReport>,
    /// Structured failure for a child that produced no `result`.
    ///
    /// Wire change, not an addition: this was a bare string and is now an
    /// object. `error.message` carries that former string verbatim.
    pub error: Option<AgentTaskCookCellError>,
}

impl AgentTaskCookBatchCellReport {
    /// The closed-vocabulary classification of this cell.
    ///
    /// A child that produced a Cook report is classified by that report, whose
    /// declared [`CookDisposition`] is authoritative. A child that never got
    /// that far — an infrastructure failure before any durable record — is
    /// classified from its status alone.
    pub fn lifecycle(&self) -> RunLifecycleProjection {
        match &self.result {
            Some(result) => result.lifecycle(),
            None => RunLifecycleProjection::from_status(&self.status),
        }
    }
}

impl serde::Serialize for AgentTaskCookBatchCellReport {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Wire<'a> {
            cook_id: &'a str,
            initial_run_id: &'a str,
            status: &'a str,
            exit_code: i32,
            #[serde(skip_serializing_if = "Option::is_none")]
            result: Option<&'a AgentTaskCookReport>,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: Option<&'a AgentTaskCookCellError>,
            lifecycle_status: RunLifecycleStatus,
            terminal: bool,
            retryable: bool,
        }

        let lifecycle = self.lifecycle();
        let wire = Wire {
            cook_id: &self.cook_id,
            initial_run_id: &self.initial_run_id,
            status: &self.status,
            exit_code: self.exit_code,
            result: self.result.as_ref(),
            error: self.error.as_ref(),
            lifecycle_status: lifecycle.lifecycle_status,
            terminal: lifecycle.terminal,
            retryable: lifecycle.retryable,
        };
        serde::Serialize::serialize(&wire, serializer)
    }
}

/// A batch's aggregate outcome.
///
/// Its `status` comes from
/// [`AgentTaskBatchState::outcome_status`](crate::agent_task_batch::AgentTaskBatchState::outcome_status),
/// a closed vocabulary that is *already* a subset of [`RunLifecycleStatus`] —
/// so the projection parses it rather than re-listing it by hand, which is the
/// exact drift `cook_status` was created to end.
#[derive(Debug, Clone)]
pub struct AgentTaskCookBatchReport {
    pub schema: &'static str,
    pub batch_id: String,
    pub status: String,
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub timed_out: usize,
    pub cooks: Vec<AgentTaskCookBatchCellReport>,
}

/// Project a batch aggregate status onto the canonical vocabulary.
///
/// [`AgentTaskBatchState::outcome_status`](crate::agent_task_batch::AgentTaskBatchState::outcome_status)
/// emits exactly the snake_case names [`RunLifecycleStatus`] deserializes
/// from, so this parses instead of keeping a second copy of that list.
/// Anything unparseable stays `Unknown` rather than being guessed.
fn batch_lifecycle_status(status: &str) -> RunLifecycleStatus {
    serde_json::from_value(Value::String(status.to_string())).unwrap_or(RunLifecycleStatus::Unknown)
}

impl AgentTaskCookBatchReport {
    /// The closed-vocabulary classification of this batch.
    pub fn lifecycle(&self) -> RunLifecycleProjection {
        let lifecycle_status = batch_lifecycle_status(&self.status);
        RunLifecycleProjection {
            lifecycle_status,
            terminal: lifecycle_status.is_terminal(),
            retryable: lifecycle_status.is_retryable(),
        }
    }
}

impl serde::Serialize for AgentTaskCookBatchReport {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Wire<'a> {
            schema: &'static str,
            batch_id: &'a str,
            status: &'a str,
            total: usize,
            queued: usize,
            running: usize,
            succeeded: usize,
            failed: usize,
            cancelled: usize,
            timed_out: usize,
            cooks: &'a [AgentTaskCookBatchCellReport],
            lifecycle_status: RunLifecycleStatus,
            terminal: bool,
            retryable: bool,
        }

        let lifecycle = self.lifecycle();
        let wire = Wire {
            schema: self.schema,
            batch_id: &self.batch_id,
            status: &self.status,
            total: self.total,
            queued: self.queued,
            running: self.running,
            succeeded: self.succeeded,
            failed: self.failed,
            cancelled: self.cancelled,
            timed_out: self.timed_out,
            cooks: &self.cooks,
            lifecycle_status: lifecycle.lifecycle_status,
            terminal: lifecycle.terminal,
            retryable: lifecycle.retryable,
        };
        serde::Serialize::serialize(&wire, serializer)
    }
}

/// The wire contract for the additive lifecycle projection.
///
/// These assert two things that are easy to break by accident: that `status`
/// and `exit_code` semantics are untouched, and that `terminal` continues to
/// come from the declared disposition rather than being re-derived from the
/// status string.
#[cfg(test)]
mod run_lifecycle_projection_tests {
    use super::*;

    fn report(status: &str, disposition: CookDisposition) -> AgentTaskCookReport {
        AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id: "cook-projection".to_string(),
            latest_run_id: None,
            history_run_ids: Vec::new(),
            invocation_run_ids: Vec::new(),
            status: status.to_string(),
            disposition,
            attempts: Vec::new(),
            finalization: None,
            intentional_no_change: None,
            selected_candidate: None,
            stop_reason: None,
            terminal_phase: None,
            terminal_failure_classification: None,
            moving_base_recovery: None,
            failure_context: None,
        }
    }

    fn cell(status: &str, result: Option<AgentTaskCookReport>) -> AgentTaskCookBatchCellReport {
        AgentTaskCookBatchCellReport {
            cook_id: "cook-projection".to_string(),
            initial_run_id: "cook-projection-attempt-1".to_string(),
            status: status.to_string(),
            exit_code: 1,
            result,
            error: None,
        }
    }

    /// The whole point: the raw, open status is untouched and the closed
    /// classification arrives beside it.
    #[test]
    fn a_cook_report_emits_the_projection_without_changing_status() {
        let value = serde_json::to_value(report("durable_failure", CookDisposition::Terminal))
            .expect("serialize");

        assert_eq!(value["status"], "durable_failure");
        assert_eq!(value["lifecycle_status"], "failed");
        assert_eq!(value["terminal"], true);
        assert_eq!(value["retryable"], true);
        // Unchanged fields must still be present and still be skipped when
        // absent, exactly as the derived implementation did.
        assert_eq!(value["schema"], "homeboy/agent-task-cook/v1");
        assert_eq!(value["disposition"], "terminal");
        assert!(value.get("latest_run_id").is_none());
        assert!(value.get("history_run_ids").is_none());
        assert!(value.get("failure_context").is_none());
    }

    /// `disposition` is the authority on terminality. A Cook a durable owner is
    /// still carrying must not be advertised as finished, or as retryable,
    /// however its status string reads.
    #[test]
    fn terminality_follows_the_declared_disposition_not_the_status_string() {
        let value = serde_json::to_value(report("durable_failure", CookDisposition::InFlight))
            .expect("serialize");

        assert_eq!(value["lifecycle_status"], "failed");
        assert_eq!(value["terminal"], false);
        assert_eq!(
            value["retryable"], false,
            "a Cook a durable owner still carries must never be advertised as retryable"
        );
    }

    /// A status from a newer binary produces an explicit "unknown"
    /// classification, never a manufactured failure — while `terminal` still
    /// arrives, because the exit declared it.
    #[test]
    fn an_unreadable_status_reports_unknown_but_still_reports_terminality() {
        let value = serde_json::to_value(report("moving_base", CookDisposition::Terminal))
            .expect("serialize");

        assert_eq!(value["status"], "moving_base");
        assert_eq!(value["lifecycle_status"], "unknown");
        assert_eq!(value["terminal"], true);
        assert_eq!(value["retryable"], false);
    }

    /// A cell that carries a child report defers to that report's declared
    /// disposition rather than re-deriving anything.
    #[test]
    fn a_batch_cell_defers_to_its_childs_declared_disposition() {
        let value = serde_json::to_value(cell(
            "durable_failure",
            Some(report("durable_failure", CookDisposition::InFlight)),
        ))
        .expect("serialize");

        assert_eq!(value["status"], "durable_failure");
        assert_eq!(value["exit_code"], 1);
        assert_eq!(value["lifecycle_status"], "failed");
        assert_eq!(value["terminal"], false);
        assert_eq!(value["result"]["terminal"], false);
    }

    /// A child that failed before producing any Cook report has no declared
    /// disposition, so the cell falls back to the vocabulary's own closed
    /// in-flight reading.
    #[test]
    fn a_batch_cell_without_a_child_report_classifies_from_status_alone() {
        let value = serde_json::to_value(cell("failed", None)).expect("serialize");

        assert_eq!(value["lifecycle_status"], "failed");
        assert_eq!(value["terminal"], true);
        assert_eq!(value["retryable"], true);
        assert!(value.get("result").is_none());
    }

    /// The batch aggregate vocabulary is a subset of the canonical one by
    /// construction. The exhaustive `match` makes a new `AgentTaskBatchState`
    /// variant a compile error here rather than a silent `Unknown` on the wire.
    #[test]
    fn every_batch_state_projects_onto_the_canonical_vocabulary() {
        use crate::agent_task_batch::AgentTaskBatchState;

        for state in [
            AgentTaskBatchState::Queued,
            AgentTaskBatchState::Running,
            AgentTaskBatchState::Succeeded,
            AgentTaskBatchState::PartialFailure,
            AgentTaskBatchState::Failed,
            AgentTaskBatchState::Cancelled,
            AgentTaskBatchState::TimedOut,
        ] {
            let expected = match state {
                AgentTaskBatchState::Queued => RunLifecycleStatus::Queued,
                AgentTaskBatchState::Running => RunLifecycleStatus::Running,
                AgentTaskBatchState::Succeeded => RunLifecycleStatus::Succeeded,
                AgentTaskBatchState::PartialFailure => RunLifecycleStatus::PartialFailure,
                AgentTaskBatchState::Failed => RunLifecycleStatus::Failed,
                AgentTaskBatchState::Cancelled => RunLifecycleStatus::Cancelled,
                AgentTaskBatchState::TimedOut => RunLifecycleStatus::TimedOut,
            };

            assert_eq!(
                batch_lifecycle_status(state.outcome_status()),
                expected,
                "{} must project onto the canonical vocabulary",
                state.outcome_status()
            );
        }
    }

    #[test]
    fn a_batch_report_emits_the_projection_beside_its_status() {
        let report = AgentTaskCookBatchReport {
            schema: "homeboy/agent-task-cook-batch/v1",
            batch_id: "batch-projection".to_string(),
            status: "partial_failure".to_string(),
            total: 2,
            queued: 0,
            running: 0,
            succeeded: 1,
            failed: 1,
            cancelled: 0,
            timed_out: 0,
            cooks: Vec::new(),
        };

        let value = serde_json::to_value(&report).expect("serialize");

        assert_eq!(value["status"], "partial_failure");
        assert_eq!(value["lifecycle_status"], "partial_failure");
        assert_eq!(value["terminal"], true);
        assert_eq!(
            value["retryable"], false,
            "a partially failed batch must not be advertised as blanket-retryable"
        );
    }
}

/// The batch boundary must not stringify a structured failure.
#[cfg(test)]
mod cell_error_envelope_tests {
    use super::*;

    #[test]
    fn the_envelope_preserves_code_retryability_field_and_hints() {
        let error = Error::validation_invalid_argument(
            "batch_id",
            "fanout batch has no child runs to resume",
            Some("batch-9525".to_string()),
            None,
        )
        .with_retryable(false)
        .with_hint("Run `homeboy agent-task fanout status batch-9525`.");
        let expected_message = error.message.clone();

        let envelope = AgentTaskCookCellError::from_error(&error);

        assert_eq!(envelope.code, "validation.invalid_argument");
        assert_eq!(envelope.retryable, Some(false));
        assert_eq!(envelope.field.as_deref(), Some("batch_id"));
        assert_eq!(
            envelope.hints,
            vec!["Run `homeboy agent-task fanout status batch-9525`.".to_string()]
        );
        assert_eq!(
            envelope.message, expected_message,
            "the human-readable rendering must survive verbatim so existing renderers do not go blank"
        );
        assert_eq!(envelope.details["id"], "batch-9525");
    }

    /// An unclassified error must not be reported as "not retryable" — that is
    /// a claim the constructor never made.
    #[test]
    fn an_unclassified_error_reports_unknown_retryability_rather_than_false() {
        let envelope =
            AgentTaskCookCellError::from_error(&Error::internal_unexpected("coordinator panicked"));

        assert_eq!(envelope.retryable, None);
        assert_eq!(envelope.code, "internal.unexpected");
    }

    #[test]
    fn the_envelope_serializes_with_the_message_a_string_error_used_to_carry() {
        let cell = AgentTaskCookBatchCellReport {
            cook_id: "cook-1".to_string(),
            initial_run_id: "cook-1-attempt-1".to_string(),
            status: "failed".to_string(),
            exit_code: 1,
            result: None,
            error: Some(AgentTaskCookCellError::from_error(
                &Error::invalid_argument("cook_id", "cook has no durable recipe"),
            )),
        };

        let value = serde_json::to_value(&cell).expect("serialize");

        assert_eq!(value["error"]["code"], "validation.invalid_argument");
        assert_eq!(value["error"]["field"], "cook_id");
        assert!(value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("cook has no durable recipe"));
        // Unchanged: the fields every existing caller already branches on.
        assert_eq!(value["status"], "failed");
        assert_eq!(value["exit_code"], 1);
    }

    /// A coordinator-declared failure still has to name a code, so it is
    /// routable rather than just readable.
    #[test]
    fn a_declared_failure_carries_a_code_and_an_explicit_retryability() {
        let envelope = AgentTaskCookCellError::declared(
            "agent_task.batch_cancelled",
            "batch cancelled before this child was started",
            false,
        );

        assert_eq!(envelope.code, "agent_task.batch_cancelled");
        assert_eq!(envelope.retryable, Some(false));
        assert!(envelope.field.is_none());
    }
}

/// Resolves a generic dispatch command once, before a typed cook is scheduled.
/// Callers compile workflow policy into the command and cook options; this
/// routine owns the shared dispatch compilation boundary.
pub fn compile_cook_attempt(
    options: AgentTaskCookServiceOptions,
    dispatch: AgentTaskDispatchCommand,
) -> Result<AgentTaskCookServiceOptions> {
    compile_cook_attempt_with_readiness_cache(
        options,
        dispatch,
        &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
    )
}

/// Compile a Cook with a caller-owned runtime-readiness cache. Batch callers
/// share this cache so identical provider/runtime/model verdicts probe once.
pub fn compile_cook_attempt_with_readiness_cache(
    mut options: AgentTaskCookServiceOptions,
    dispatch: AgentTaskDispatchCommand,
    readiness_cache: &mut crate::agent_task_provider::ProviderRuntimeReadinessCache,
) -> Result<AgentTaskCookServiceOptions> {
    validate_single_cook_prompt_source(
        dispatch.prompt.as_deref(),
        &dispatch.tasks,
        dispatch.core.tasks_json.as_deref(),
    )?;
    let request = agent_task_dispatch_service::resolve_dispatch_request(dispatch)?;
    options.initial_plan = build_dispatch_plan(&request)?;
    let catalog = crate::agent_task_provider::AgentTaskProviderCatalog::discover();
    catalog.validate_explicit_models(&options.initial_plan)?;
    crate::agent_task_provider::preflight_plan_provider_runtime_readiness_with_providers(
        &options.initial_plan,
        catalog.providers(),
        readiness_cache,
    )?;
    // Finalization disclosure is derived from the compiled provider invocation,
    // not a pre-resolution CLI value. The plan is persisted in the recipe and
    // remains authoritative across continuation.
    options.ai_model = options
        .initial_plan
        .tasks
        .first()
        .and_then(|task| task.executor.model())
        .map(str::to_string);
    Ok(options)
}

/// Whether a batch coordinator answers to a durable owner outside its process.
///
/// # Why every field defaults to off
///
/// The coordinator this controls has, until now, been an unconditionally
/// blocking thread pool: `std::thread::scope` joins before returning, with no
/// cancellation, no checkpoint, and no way for anything outside the process to
/// stop it or observe its progress. That is exactly the behaviour every
/// existing caller depends on, so [`Default`] reproduces it byte for byte and
/// only a caller that has a durable owner — today, the daemon-owned controller
/// job — opts in.
///
/// Each flag is durable-state-driven rather than a process-local token, because
/// the owner and the coordinator are in different processes: the daemon owns
/// the job, and a detached child runs the batch. A shared `AtomicBool` cannot
/// cross that boundary; the batch record can.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentTaskCookBatchControl {
    /// Stop starting new children once the durable batch record carries a
    /// coordinator cancellation.
    ///
    /// This bounds the *claim* loop, not a child already in flight. Stopping
    /// in-flight work stays with `agent_task_lifecycle::cancel_run`, the one
    /// established cancellation path, which the durable owner drives per child.
    pub honour_durable_cancellation: bool,
    /// Report a child whose durable record is already terminal instead of
    /// running it a second time.
    ///
    /// This is the property that lets a coordinator be re-entered after its
    /// owner recovered it. It is also what makes cancellation drain promptly:
    /// a cancelled child is terminal, so it is observed rather than started.
    pub skip_durably_terminal_children: bool,
    /// Persist each child's terminal outcome into the durable batch record as
    /// it happens, so an owner outside this process can checkpoint against it
    /// instead of waiting for the whole batch to return.
    pub publish_child_terminalization: bool,
}

impl AgentTaskCookBatchControl {
    /// The control a coordinator owned by a durable controller job runs under.
    pub fn daemon_owned() -> Self {
        Self {
            honour_durable_cancellation: true,
            skip_durably_terminal_children: true,
            publish_child_terminalization: true,
        }
    }
}

/// Runs independently durable cooks with bounded concurrency while preserving
/// input order for callers that join their own metadata onto the results.
/// Batch-cook fanout is the first caller; other cook coordinators can migrate
/// by compiling their own `AgentTaskCookServiceOptions` and using this runner.
///
/// This is the unowned coordinator: it runs to completion or dies with its
/// caller. A caller with a durable owner uses [`run_cook_batch_with_control`].
pub fn run_cook_batch<E>(
    options: AgentTaskCookBatchOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookBatchReport>>
where
    E: AgentTaskExecutorAdapter + Clone + Send,
{
    run_cook_batch_with_control(options, executor, AgentTaskCookBatchControl::default())
}

/// Run a cook batch under a control that a durable owner can observe and stop.
///
/// The concurrency ceiling and the wall-clock budget are the caller's, exactly
/// as before: this adds a cancellation check and a durable-terminality check to
/// the claim loop and nothing else. In particular it does not re-derive the
/// ceiling resolved by `resolve_batch_concurrency`, and it does not introduce a
/// second deadline — the batch `CookDeadline` is still captured once here and
/// re-bound onto every worker.
pub fn run_cook_batch_with_control<E>(
    options: AgentTaskCookBatchOptions,
    executor: E,
    control: AgentTaskCookBatchControl,
) -> Result<AgentTaskRunResult<AgentTaskCookBatchReport>>
where
    E: AgentTaskExecutorAdapter + Clone + Send,
{
    let total = options.cooks.len();
    if total == 0 {
        return Err(Error::validation_invalid_argument(
            "cooks",
            "agent-task cook batch requires at least one cook",
            Some(options.batch_id),
            None,
        ));
    }

    let workers = options.max_concurrency.max(1).min(total);
    let batch_id = Arc::new(options.batch_id.clone());
    let cooks = Arc::new(options.cooks);
    let next = Arc::new(Mutex::new(0usize));
    let (tx, rx) = mpsc::channel();
    // The caller's route is thread-local, so each worker must re-bind it or its
    // children submit unrouted and never notify the originating destination.
    let notification_route = homeboy_core::notification_route::capture();
    // The wall-clock budget is thread-local for the same reason and must cross
    // the same boundary. A budget bound only on the coordinator would leave
    // every child it started unbounded, which is the fanout multiplication this
    // budget exists to stop.
    let cook_deadline = capture_cook_deadline();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let batch_id = Arc::clone(&batch_id);
            let cooks = Arc::clone(&cooks);
            let next = Arc::clone(&next);
            let tx = tx.clone();
            let executor = executor.clone();
            let notification_route = notification_route.clone();
            scope.spawn(move || {
                cook_deadline.bind(|| {
                    notification_route.bind(|| loop {
                        let index = {
                            let mut next = next.lock().expect("cook batch work queue");
                            if *next == cooks.len() {
                                return;
                            }
                            let index = *next;
                            *next += 1;
                            index
                        };
                        let cook = cooks[index].clone();
                        // Claimed, but not yet started. Everything between here
                        // and `run_cook` is the coordinator refusing to begin
                        // work it should not begin: work its owner cancelled, or
                        // work a previous coordinator already finished.
                        //
                        // The claim still happens so the child keeps its slot in
                        // the report. A cancelled or already-terminal child is
                        // reported, never silently dropped, so `total` and the
                        // per-child ordering are the same shape callers already
                        // join their own metadata onto.
                        let cell = match claim_disposition(&batch_id, &cook, control) {
                            ClaimDisposition::Run => {
                                let cell = match run_cook(cook.clone(), executor.clone()) {
                                    Ok(result) => AgentTaskCookBatchCellReport {
                                        cook_id: cook.cook_id.clone(),
                                        initial_run_id: cook.initial_run_id.clone(),
                                        status: result.value.status.clone(),
                                        exit_code: result.exit_code,
                                        result: Some(result.value),
                                        error: None,
                                    },
                                    // The child failed before it could produce
                                    // a Cook report, so this envelope is the
                                    // only thing the orchestrator gets. It
                                    // keeps the error's code, retryability,
                                    // and hints rather than stringifying them.
                                    Err(error) => AgentTaskCookBatchCellReport {
                                        cook_id: cook.cook_id.clone(),
                                        initial_run_id: cook.initial_run_id.clone(),
                                        status: "failed".to_string(),
                                        exit_code: 1,
                                        result: None,
                                        error: Some(AgentTaskCookCellError::from_error(&error)),
                                    },
                                };
                                // Published as each child terminalizes rather
                                // than once at the end, so a durable owner can
                                // checkpoint against real progress instead of
                                // learning everything at once when the
                                // coordinator returns — or nothing at all if it
                                // never does.
                                if control.publish_child_terminalization {
                                    let _ = crate::agent_task_batch::record_child_finalization(
                                        &batch_id,
                                        &cell.initial_run_id,
                                        child_finalization_value(&cell),
                                    );
                                }
                                cell
                            }
                            ClaimDisposition::AlreadyTerminal(cell)
                            | ClaimDisposition::Cancelled(cell) => cell,
                        };
                        let _ = tx.send((index, cell));
                    })
                })
            });
        }
    });
    drop(tx);

    let mut cells = (0..total).map(|_| None).collect::<Vec<_>>();
    for (index, cell) in rx {
        cells[index] = Some(cell);
    }
    Ok(cook_batch_result(
        options.batch_id,
        cells.into_iter().flatten().collect(),
    ))
}

/// What the coordinator does with a child it has just claimed.
///
/// Extracted so the one correctness property that matters can be asserted
/// directly rather than inferred from a nest of conditions inside a worker
/// closure: **no variant other than `Run` starts provider work**, and `Run` is
/// unreachable for a child whose durable record is already terminal.
#[derive(Debug)]
enum ClaimDisposition {
    /// Nothing durable objects. Start the cook.
    Run,
    /// A previous coordinator already carried this child to a terminal state.
    /// Report what it observed; never run it again.
    AlreadyTerminal(AgentTaskCookBatchCellReport),
    /// The batch was cancelled before this child was started.
    Cancelled(AgentTaskCookBatchCellReport),
}

/// Decide a claimed child's fate from durable state alone.
///
/// Order matters. Durable terminality is checked *first*, because a child that
/// a cancellation already terminalized must be reported with the state it
/// actually reached — `cancelled`, `succeeded`, whatever it was — rather than
/// overwritten with a generic cancellation cell that would lose the outcome.
fn claim_disposition(
    batch_id: &str,
    cook: &AgentTaskCookServiceOptions,
    control: AgentTaskCookBatchControl,
) -> ClaimDisposition {
    if control.skip_durably_terminal_children {
        if let Some(state) = durably_terminal_child_state(&cook.cook_id) {
            return ClaimDisposition::AlreadyTerminal(observed_child_cell(cook, state));
        }
    }
    if control.honour_durable_cancellation
        && crate::agent_task_batch::coordinator_is_cancelled(batch_id)
    {
        return ClaimDisposition::Cancelled(AgentTaskCookBatchCellReport {
            cook_id: cook.cook_id.clone(),
            initial_run_id: cook.initial_run_id.clone(),
            status: CookStatus::Cancelled.as_str().to_string(),
            exit_code: 1,
            result: None,
            // Not retryable as-is: the coordinator's cancellation stands until
            // an operator starts a new batch.
            error: Some(AgentTaskCookCellError::declared(
                "agent_task.batch_cancelled",
                "batch cancelled before this child was started",
                false,
            )),
        });
    }
    ClaimDisposition::Run
}

/// The terminal state this child already reached, if it reached one.
///
/// A child with no durable record has not started, which is the overwhelmingly
/// common case on a first run: `persist_initial_recipe` writes a recipe, not a
/// lifecycle record, so a fresh batch reads `None` here for every child. An
/// unreadable record is also `None` — the coordinator's job is to run work, and
/// a transient read failure must not be turned into a silent skip.
fn durably_terminal_child_state(cook_id: &str) -> Option<agent_task_lifecycle::AgentTaskRunState> {
    let record = agent_task_lifecycle::status(cook_id).ok()?;
    record.state.is_terminal().then_some(record.state)
}

/// Report a child from the durable state a previous coordinator left, without
/// running it. The exit code follows the same rule the live path uses: only a
/// successful run is a zero exit.
fn observed_child_cell(
    cook: &AgentTaskCookServiceOptions,
    state: agent_task_lifecycle::AgentTaskRunState,
) -> AgentTaskCookBatchCellReport {
    let status = match state {
        // A terminal, successful durable run is a completed Cook. `Completed`
        // rather than a synthesized string so `cook_batch_result`'s
        // `CookStatus::from_status` classification is the same one the live
        // path produces.
        agent_task_lifecycle::AgentTaskRunState::Succeeded => CookStatus::Completed,
        agent_task_lifecycle::AgentTaskRunState::Cancelled => CookStatus::Cancelled,
        _ => CookStatus::Failed,
    };
    AgentTaskCookBatchCellReport {
        cook_id: cook.cook_id.clone(),
        initial_run_id: cook.initial_run_id.clone(),
        status: status.as_str().to_string(),
        exit_code: i32::from(state != agent_task_lifecycle::AgentTaskRunState::Succeeded),
        result: None,
        error: None,
    }
}

/// Resume a persisted cook batch after its original synchronous coordinator
/// exited or timed out. Each child's durable recipe fully reconstructs its cook
/// options, so re-running [`run_cook`] idempotently harvests every terminal
/// child through the SAME promotion, deterministic gates, commit, push, and PR
/// finalization the original caller owned — without redispatching a completed
/// provider attempt or duplicating a PR.
///
/// A child with no persisted recipe (never reached cook start) or that is still
/// in flight on a runner daemon is reported as-is rather than forced. The
/// per-child finalization state is reconciled back into the durable batch record
/// so repeated resume calls converge instead of re-finalizing (#9525).
pub fn resume_cook_batch<E, D>(
    batch_id: &str,
    executor: E,
    reconstruct_dispatcher: D,
) -> Result<AgentTaskRunResult<AgentTaskCookBatchReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: Fn(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
{
    resume_cook_batch_with_finalizer(
        batch_id,
        executor,
        reconstruct_dispatcher,
        finalize_or_load_cook_pr,
    )
}

/// Resume one durable Cook through its original provider, promotion, gates, and
/// finalization contract. Fanout supervisors use this child-local entrypoint so
/// a blocked sibling cannot prevent a ready candidate from advancing.
pub fn resume_cook<E, D>(
    cook_id: &str,
    executor: E,
    reconstruct_dispatcher: D,
    rerun_completed_gates: bool,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: Fn(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
{
    let mut finalize = finalize_or_load_cook_pr;
    let report = resume_batch_child_with_gate_rerun(
        cook_id,
        cook_id,
        executor,
        &reconstruct_dispatcher,
        &mut finalize,
        rerun_completed_gates,
    )?;
    Ok(AgentTaskRunResult {
        exit_code: cook_report_exit_code(&report),
        value: report,
    })
}

fn resume_cook_batch_with_finalizer<E, D, F>(
    batch_id: &str,
    executor: E,
    reconstruct_dispatcher: D,
    mut finalize: F,
) -> Result<AgentTaskRunResult<AgentTaskCookBatchReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: Fn(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
{
    let batch = crate::agent_task_batch::read_batch_record(batch_id)?;
    if batch.child_runs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "batch_id",
            format!("agent-task fanout batch `{batch_id}` has no child runs to resume"),
            Some(batch_id.to_string()),
            None,
        ));
    }

    let ready = crate::agent_task_batch::fanout_ready_child_run_ids(batch_id)?;
    let total = batch.child_runs.len();
    let mut cells = Vec::with_capacity(total);
    for child in &batch.child_runs {
        if ready
            .as_ref()
            .is_some_and(|ready| !ready.contains(&child.run_id))
        {
            cells.push(AgentTaskCookBatchCellReport {
                cook_id: child.task_id.clone(),
                initial_run_id: child.run_id.clone(),
                status: "blocked_by_dependency".to_string(),
                exit_code: 0,
                result: None,
                error: None,
            });
            continue;
        }
        // The persisted batch child `run_id` is the cook id (`cook-<id>`), which
        // is exactly the durable recipe key. Reconstruct from that recipe so the
        // resumed cook re-runs its own gates and finalization contract.
        let cook_id = child.run_id.clone();
        let cell = match resume_batch_child(
            batch_id,
            &cook_id,
            executor.clone(),
            &reconstruct_dispatcher,
            &mut finalize,
        ) {
            Ok(report) => {
                let exit_code = cook_report_exit_code(&report);
                AgentTaskCookBatchCellReport {
                    cook_id: report.cook_id.clone(),
                    initial_run_id: cook_id,
                    status: report.status.clone(),
                    exit_code,
                    result: Some(report),
                    error: None,
                }
            }
            Err(error) => AgentTaskCookBatchCellReport {
                cook_id: child.task_id.clone(),
                initial_run_id: cook_id,
                status: "failed".to_string(),
                exit_code: 1,
                result: None,
                error: Some(AgentTaskCookCellError::from_error(&error)),
            },
        };
        // Persist each child's finalization outcome as it is harvested so a
        // repeated resume (or a crash mid-batch) converges idempotently.
        crate::agent_task_batch::record_child_finalization(
            batch_id,
            &cell.initial_run_id,
            child_finalization_value(&cell),
        )?;
        cells.push(cell);
    }

    Ok(cook_batch_result(batch.batch_id, cells))
}

fn cook_batch_result(
    batch_id: String,
    cooks: Vec<AgentTaskCookBatchCellReport>,
) -> AgentTaskRunResult<AgentTaskCookBatchReport> {
    let total = cooks.len();
    let mut totals = crate::agent_task_batch::AgentTaskBatchTotals::default();
    for cell in &cooks {
        match CookStatus::from_status(&cell.status) {
            CookStatus::Queued => totals.queued += 1,
            CookStatus::Running | CookStatus::InFlight => totals.running += 1,
            CookStatus::Cancelled => totals.cancelled += 1,
            CookStatus::TimedOut => totals.timed_out += 1,
            _ if cell.exit_code == 0 => totals.succeeded += 1,
            _ => totals.failed += 1,
        }
    }
    let state = crate::agent_task_batch::aggregate_state(&totals);

    AgentTaskRunResult {
        exit_code: state.exit_code(),
        value: AgentTaskCookBatchReport {
            schema: "homeboy/agent-task-cook-batch/v1",
            batch_id,
            status: state.outcome_status().to_string(),
            total,
            queued: totals.queued,
            running: totals.running,
            succeeded: totals.succeeded,
            failed: totals.failed,
            cancelled: totals.cancelled,
            timed_out: totals.timed_out,
            cooks,
        },
    }
}

/// Reconstruct one batch child's cook from its durable recipe and re-run it.
/// A missing recipe means the child never reached cook start; surface an
/// actionable resumability error instead of fabricating a cook.
fn resume_batch_child<E, D, F>(
    batch_id: &str,
    cook_id: &str,
    executor: E,
    reconstruct_dispatcher: &D,
    finalize: &mut F,
) -> Result<AgentTaskCookReport>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: Fn(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
{
    resume_batch_child_with_gate_rerun(
        batch_id,
        cook_id,
        executor,
        reconstruct_dispatcher,
        finalize,
        false,
    )
}

fn resume_batch_child_with_gate_rerun<E, D, F>(
    batch_id: &str,
    cook_id: &str,
    executor: E,
    reconstruct_dispatcher: &D,
    finalize: &mut F,
    rerun_completed_gates: bool,
) -> Result<AgentTaskCookReport>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: Fn(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
{
    if !super::recipe_exists(cook_id)? {
        return Err(Error::validation_invalid_argument(
            "cook_id",
            format!(
                "cook `{cook_id}` has no durable recipe; it never reached cook start and cannot be resumed"
            ),
            Some(cook_id.to_string()),
            Some(vec![format!(
                "Re-dispatch this cook, or inspect it with `homeboy agent-task status {cook_id}`."
            )]),
        ));
    }
    let recipe = super::load_recipe(cook_id)?;
    let mut attempt_run_ids = recipe
        .attempts
        .iter()
        .map(|attempt| attempt.run_id.clone())
        .collect::<Vec<_>>();
    if let Ok(index) = agent_task_lifecycle::cook_index(cook_id) {
        attempt_run_ids.extend(index.attempts.into_iter().map(|attempt| attempt.run_id));
    }
    let checkpoint_run_id = attempt_run_ids.into_iter().find(|run_id| {
        agent_task_lifecycle::exact_record(run_id)
            .ok()
            .is_some_and(|record| {
                record
                    .metadata
                    .get("cook_recovery_source_checkpoint")
                    .is_some()
                    || record
                        .metadata
                        .get("latest_promotion")
                        .and_then(|promotion| promotion.get("status"))
                        .and_then(Value::as_str)
                        == Some("verification_pending")
            })
    });
    if checkpoint_run_id.is_none() {
        agent_task_lifecycle::reconcile_terminal_artifact_projection(cook_id)?;
        if let Some(reason) = agent_task_lifecycle::terminal_artifact_projection_readiness(cook_id)?
        {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                format!("cook `{cook_id}` cannot resume until controller-side patch projection is ready: {reason}"),
                Some(cook_id.to_string()),
                Some(vec![format!(
                    "Run `homeboy agent-task status {cook_id}` to reconcile the controller projection."
                )]),
            ));
        }
    }
    // Faithfully reconstruct the recipe's transport so re-running `run_cook`
    // matches the persisted durable inputs (a stripped dispatcher would look
    // like a conflicting new cook). A terminal child is not re-dispatched — its
    // `needs_execution` check is false — so the reconstructed transport is only
    // used to satisfy the recipe contract, never to spend a provider attempt
    // (#9525).
    let attempt_dispatcher =
        reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
    let mut options = super::reconstruct_options_with_dispatcher(&recipe, attempt_dispatcher)?;
    options.gates.rerun_completed_gates = rerun_completed_gates;
    if let Some(run_id) = checkpoint_run_id {
        let attempt = recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == run_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "cook_recovery_source_checkpoint",
                    "checkpointed source attempt is absent from the durable cook recipe",
                    Some(run_id.clone()),
                    None,
                )
            })?;
        // An earlier post-apply checkpoint remains the source of truth even if
        // a later provider attempt failed and became the cook-index latest run.
        options.initial_run_id = attempt.run_id.clone();
        options.initial_plan = attempt.plan.clone();
        agent_task_lifecycle::record_cook_recovery_checkpoint(
            &attempt.run_id,
            "verification_pending",
            &format!("homeboy agent-task fanout resume {batch_id}"),
        )?;
    }
    Ok(
        run_cook_with_finalizer(options, executor, |options, run_id, promotion| {
            finalize(options, run_id, promotion)
        })?
        .value,
    )
}

/// The durable checkpoint an owner outside this process reads.
///
/// `error` follows [`AgentTaskCookBatchCellReport::error`] and is therefore now
/// an object rather than a string; `error.message` carries the text the field
/// used to hold. `status` and `exit_code` are unchanged, and
/// `lifecycle_status`/`terminal`/`retryable` are added so an owner reading only
/// this checkpoint can classify the child without loading its report.
fn child_finalization_value(cell: &AgentTaskCookBatchCellReport) -> Value {
    let lifecycle = cell.lifecycle();
    serde_json::json!({
        "resumed_at": chrono::Utc::now().to_rfc3339(),
        "exit_code": cell.exit_code,
        "status": cell.status,
        "lifecycle_status": lifecycle.lifecycle_status,
        "terminal": lifecycle.terminal,
        "retryable": lifecycle.retryable,
        "error": cell.error,
    })
}

fn cook_report_exit_code(report: &AgentTaskCookReport) -> i32 {
    // A review-ready or already-finalized cook is a success; anything the cook
    // could not carry to a green, finalized state is a non-zero resume result
    // the operator must still act on.
    match CookStatus::from_status(&report.status) {
        status if status.is_success_exit() => 0,
        _ => {
            if report
                .finalization
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "review_ready" | "draft_published"))
            {
                0
            } else {
                1
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookAttemptReport {
    pub attempt: u32,
    pub run_id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<AgentTaskPromotionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<AgentTaskCookLoopReport>,
}

pub(crate) enum CookFollowUpDispatch {
    Dispatched { run_id: String },
    BudgetExhausted { reason: String },
    PolicyFailure { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CookFollowUpBudgetScope {
    Cook,
    FreshCookReview,
    CandidateAdoptionReview,
}

fn remediation_tool_policy_error(request: &crate::agent_task::AgentTaskRequest) -> Option<String> {
    (!request.policy.permits_workspace_read_tool()).then(|| {
        "Cook remediation policy must grant the runner read access to the task workspace before dispatch"
            .to_string()
    })
}

fn follow_up_budget_scope(
    source_request: &crate::agent_task::AgentTaskRequest,
    follow_up_request: &crate::agent_task::AgentTaskRequest,
) -> CookFollowUpBudgetScope {
    if follow_up_request.inputs["cook_loop"]["review_form_required"] != true {
        return CookFollowUpBudgetScope::Cook;
    }
    match source_request.inputs["cook_loop"]["execution_budget_authority"]["kind"].as_str() {
        Some("fresh_cook_review") => CookFollowUpBudgetScope::FreshCookReview,
        Some("candidate_adoption_review") => CookFollowUpBudgetScope::CandidateAdoptionReview,
        Some(_) => CookFollowUpBudgetScope::Cook,
        None => CookFollowUpBudgetScope::FreshCookReview,
    }
}

fn budget_authority_matches_scope(authority: &Value, scope: CookFollowUpBudgetScope) -> bool {
    authority["kind"].as_str()
        == Some(match scope {
            CookFollowUpBudgetScope::FreshCookReview => "fresh_cook_review",
            CookFollowUpBudgetScope::CandidateAdoptionReview => "candidate_adoption_review",
            CookFollowUpBudgetScope::Cook => return false,
        })
}

fn scoped_follow_up_budget(
    scope: CookFollowUpBudgetScope,
    cook_budget: &AgentTaskExecutionBudget,
    cook_usage: ExecutionBudgetUsage,
    persisted_authority: Option<&Value>,
) -> (AgentTaskExecutionBudget, ExecutionBudgetUsage) {
    match scope {
        CookFollowUpBudgetScope::Cook => (cook_budget.clone(), cook_usage),
        CookFollowUpBudgetScope::FreshCookReview
        | CookFollowUpBudgetScope::CandidateAdoptionReview => {
            let authority = persisted_authority.unwrap_or(&Value::Null);
            // Review-only remediation has a fixed upper bound, but a durable
            // authority may intentionally narrow it. Never mint a broader
            // allowance when resuming that authority.
            let bounded = |key, maximum| {
                authority[key]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .map(|value| value.min(maximum))
                    .unwrap_or(maximum)
            };
            (
                AgentTaskExecutionBudget::new(
                    bounded("max_provider_executions", 2),
                    bounded("max_same_provider_retries", 1),
                    bounded("max_provider_rotations", 0),
                ),
                ExecutionBudgetUsage::default(),
            )
        }
    }
}

fn review_budget_authority(
    scope: CookFollowUpBudgetScope,
    persisted_authority: Option<&Value>,
) -> Value {
    let (budget, _) = scoped_follow_up_budget(
        scope,
        &AgentTaskExecutionBudget::new(0, 0, 0),
        ExecutionBudgetUsage::default(),
        persisted_authority,
    );
    let kind = match scope {
        CookFollowUpBudgetScope::FreshCookReview => "fresh_cook_review",
        CookFollowUpBudgetScope::CandidateAdoptionReview => "candidate_adoption_review",
        CookFollowUpBudgetScope::Cook => unreachable!(),
    };
    let review_plan_provider_executions = persisted_authority
        .and_then(|authority| authority["review_plan_provider_executions"].as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.min(1))
        .unwrap_or(1);
    serde_json::json!({
        "kind": kind,
        "max_provider_executions": budget.max_provider_executions,
        "max_same_provider_retries": budget.max_same_provider_retries,
        "max_provider_rotations": budget.max_provider_rotations,
        "review_plan_provider_executions": review_plan_provider_executions,
    })
}

fn child_execution_budget(
    scope: CookFollowUpBudgetScope,
    budget_limit: &AgentTaskExecutionBudget,
) -> AgentTaskExecutionBudget {
    match scope {
        CookFollowUpBudgetScope::Cook => budget_limit.clone(),
        CookFollowUpBudgetScope::FreshCookReview
        | CookFollowUpBudgetScope::CandidateAdoptionReview => {
            AgentTaskExecutionBudget::new(1, 0, 0)
        }
    }
}

/// Append and dispatch one remediation attempt from an authenticated promoted
/// candidate. Both ordinary Cook feedback and external candidate adoption use
/// this boundary so their budget, provenance, and baseline authority match.
#[expect(
    clippy::too_many_arguments,
    reason = "Cook follow-up retains explicit durable recipe and dispatch identity fields"
)]
pub(crate) fn dispatch_cook_follow_up<E>(
    options: &AgentTaskCookServiceOptions,
    executor: E,
    cook_id: &str,
    attempt: u32,
    source_run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    promotion: &AgentTaskPromotionReport,
    mut follow_up_request: crate::agent_task::AgentTaskRequest,
    known_same_executor: bool,
    budget_scope: CookFollowUpBudgetScope,
    budget_limit: &AgentTaskExecutionBudget,
    budget_used: ExecutionBudgetUsage,
    remediation_category_usage: &mut ExecutionBudgetUsage,
) -> Result<CookFollowUpDispatch>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    if let Some(reason) = remediation_tool_policy_error(&follow_up_request) {
        return Ok(CookFollowUpDispatch::PolicyFailure { reason });
    }
    let recipe = super::load_recipe(cook_id)?;
    let related_attempts = recipe.attempts.iter().filter(|recipe_attempt| {
        recipe_attempt.plan.tasks.len() == 1
            && recipe_attempt.plan.tasks[0].inputs["cook_loop"]["artifact_provenance"]
                ["source_run_id"]
                .as_str()
                == Some(source_run_id)
    });
    let replay = related_attempts
        .clone()
        .max_by_key(|recipe_attempt| recipe_attempt.attempt)
        .filter(|recipe_attempt| recipe_attempt.attempt > attempt)
        .filter(|recipe_attempt| {
            recipe_attempt.plan.tasks[0].inputs["cook_loop"]["review_form_required"] == true
                && retryable_provider_discovery_failure(&recipe_attempt.run_id)
        })
        .cloned();
    let persisted_budget_authority = plan
        .tasks
        .first()
        .map(|task| &task.inputs["cook_loop"]["execution_budget_authority"])
        .filter(|authority| budget_authority_matches_scope(authority, budget_scope))
        .cloned();
    let (budget_limit, mut durable_budget_used) = scoped_follow_up_budget(
        budget_scope,
        budget_limit,
        budget_used,
        persisted_budget_authority.as_ref(),
    );
    let mut authority_run_ids = std::collections::BTreeSet::new();
    if let Some(authority) = persisted_budget_authority.as_ref() {
        let mut ancestor_run_id = Some(source_run_id);
        while let Some(run_id) = ancestor_run_id {
            let Some(recipe_attempt) = recipe
                .attempts
                .iter()
                .find(|recipe_attempt| recipe_attempt.run_id == run_id)
                .filter(|recipe_attempt| {
                    recipe_attempt.plan.tasks[0].inputs["cook_loop"]["execution_budget_authority"]
                        == *authority
                })
            else {
                break;
            };
            authority_run_ids.insert(recipe_attempt.run_id.as_str());
            ancestor_run_id = recipe_attempt.plan.tasks[0].inputs["cook_loop"]
                ["artifact_provenance"]["source_run_id"]
                .as_str();
        }
    }
    for recipe_attempt in recipe.attempts.iter().filter(|recipe_attempt| {
        authority_run_ids.contains(recipe_attempt.run_id.as_str())
            || (persisted_budget_authority.is_none()
                && recipe_attempt.plan.tasks[0].inputs["cook_loop"]["artifact_provenance"]
                    ["source_run_id"]
                    .as_str()
                    == Some(source_run_id))
    }) {
        if let Ok(aggregate) = agent_task_lifecycle::read_aggregate(&recipe_attempt.run_id) {
            durable_budget_used.add(execution_budget_usage(&aggregate));
        }
    }
    if known_same_executor {
        durable_budget_used.same_provider_retries =
            durable_budget_used.same_provider_retries.saturating_add(
                related_attempts
                    .map(|recipe_attempt| recipe_attempt.attempt)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    .try_into()
                    .unwrap_or(u32::MAX),
            );
    }
    let Some(remaining_budget) = budget_remaining(&budget_limit, durable_budget_used) else {
        return Ok(CookFollowUpDispatch::BudgetExhausted {
            reason: "max_provider_executions".to_string(),
        });
    };
    let same_provider = (known_same_executor
        || follow_up_request.inputs["cook_loop"]["review_form_required"] == true)
        .then_some(true)
        .or_else(|| {
            let durable_provider_executions = agent_task_lifecycle::status(source_run_id)
                .ok()
                .and_then(|record| record.metadata.get("provider_executions").cloned())
                .filter(|executions| {
                    executions
                        .as_array()
                        .is_some_and(|executions| !executions.is_empty())
                });
            terminal_executor_matches(
                aggregate,
                plan,
                durable_provider_executions.as_ref(),
                &follow_up_request.executor,
            )
        });
    let Some(same_provider) = same_provider else {
        return Ok(CookFollowUpDispatch::PolicyFailure {
            reason: "cannot classify Cook remediation without terminal executor identity"
                .to_string(),
        });
    };
    let reservation = if replay.is_some() {
        ExecutionBudgetUsage::default()
    } else {
        match reserve_remediation_budget(&remaining_budget, same_provider) {
            Ok(reservation) => reservation,
            Err(reason) => {
                return Ok(CookFollowUpDispatch::BudgetExhausted {
                    reason: reason.to_string(),
                })
            }
        }
    };
    // This is reviewable lineage, not the process-local baseline capability.
    follow_up_request.inputs["cook_loop"]["artifact_provenance"] = serde_json::json!({
        "source_run_id": source_run_id,
        "source_task_id": promotion.source.task_id,
        "source_patch_artifact_sha256": promotion.patch_artifact.sha256,
    });
    if budget_scope != CookFollowUpBudgetScope::Cook {
        follow_up_request.inputs["cook_loop"]["execution_budget_authority"] =
            review_budget_authority(budget_scope, persisted_budget_authority.as_ref());
    }
    let (next_attempt, next_run_id, mut follow_up_plan, replaced_run_id) = match replay {
        Some(recipe_attempt) => (
            recipe_attempt.attempt,
            agent_task_lifecycle::cook_attempt_run_id(cook_id, recipe_attempt.attempt),
            recipe_attempt.plan.clone(),
            Some(recipe_attempt.run_id.clone()),
        ),
        None => {
            let next_attempt = recipe
                .attempts
                .iter()
                .map(|recipe_attempt| recipe_attempt.attempt)
                .max()
                .unwrap_or(attempt)
                .max(attempt)
                .checked_add(1)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "cook_recipe.attempts",
                        "durable cook attempt sequence is exhausted",
                        Some(cook_id.to_string()),
                        None,
                    )
                })?;
            let next_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, next_attempt);
            let mut follow_up_plan = AgentTaskPlan::new(
                format!("{cook_id}-cook-attempt-{next_attempt}"),
                vec![follow_up_request],
            );
            follow_up_plan.options = plan.options.clone();
            // Gate-feedback is a child execution, not a fresh one-shot policy.
            // Review-only continuations retain their separately bounded plan.
            follow_up_plan.options.execution_budget =
                child_execution_budget(budget_scope, &budget_limit);
            if budget_scope != CookFollowUpBudgetScope::Cook {
                follow_up_plan.options.retry.max_attempts = 1;
            }
            (next_attempt, next_run_id, follow_up_plan, None)
        }
    };
    let review_form_only =
        follow_up_plan.tasks[0].inputs["cook_loop"]["review_form_required"] == true;
    if let Some(replaced_run_id) = replaced_run_id {
        super::record_recipe_attempt_replacement(cook_id, &replaced_run_id, &next_run_id)?;
    } else {
        super::record_recipe_attempt(cook_id, next_attempt, &next_run_id, &follow_up_plan)?;
    }
    if attempt_needs_execution(&next_run_id) {
        let baseline = materialize_follow_up_baseline(
            promotion,
            source_run_id,
            &follow_up_plan.tasks[0].task_id,
        )?;
        follow_up_plan.tasks[0].workspace.root = Some(baseline.path.display().to_string());
        follow_up_plan.tasks[0].inputs["cook_loop"]["artifact_provenance"] =
            baseline.artifact_provenance();
        // Follow-up retries intentionally move into an authenticated baseline.
        // Refresh the durable execution attestation before this plan can be
        // persisted or handed to a local or detached provider.
        bind_dispatch_workspace_attestations(&mut follow_up_plan)?;
        agent_task_lifecycle::submit_plan(&follow_up_plan, Some(&next_run_id))?;
        agent_task_lifecycle::record_cook_attempt(cook_id, next_attempt, &next_run_id)?;
        if budget_scope == CookFollowUpBudgetScope::CandidateAdoptionReview {
            agent_task_lifecycle::checkpoint_candidate_adoption_remediation(
                source_run_id,
                &next_run_id,
            )?;
        }
        if let Some(dispatcher) = &options.attempt_dispatcher {
            // A detached dispatcher may return before any executor-side
            // lifecycle write, so a controller crash after the runner accepts
            // the retry but before its state advances would re-dispatch on
            // restart. Bracket the dispatch with a durable operation claim keyed
            // by the retry run id: a completed claim means the retry is already
            // dispatched, so a resumed pass continues from it instead of
            // sending a second handoff (#8357).
            // Persist the exact materialized plan first so a continuation resumes
            // this baseline-bound workspace contract, and so the run record the
            // claim is written onto exists.
            let operation_key = retry_dispatch_operation_key(&next_run_id);
            match agent_task_lifecycle::claim_cook_operation(
                &next_run_id,
                &operation_key,
                RETRY_DISPATCH_CLAIM_LEASE,
            )? {
                agent_task_lifecycle::ClaimOutcome::Acquired => {
                    dispatcher.dispatch_attempt(
                        follow_up_plan,
                        &next_run_id,
                        Some(baseline.capability()),
                    )?;
                    agent_task_lifecycle::complete_cook_operation(
                        &next_run_id,
                        &operation_key,
                        serde_json::json!({ "dispatched_run_id": next_run_id }),
                    )?;
                }
                // The retry was already durably dispatched (completed) or is
                // owned by a concurrent pass (lease held). Either way, do not
                // send a second handoff; the persisted plan and run state carry
                // the existing dispatch forward.
                agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_)
                | agent_task_lifecycle::ClaimOutcome::LeaseHeld => {}
            }
        } else {
            run_loaded_plan_with_derived_cook_baseline(
                follow_up_plan,
                Some(&next_run_id),
                executor,
                Some(baseline.capability()),
                Some(cook_attempt_harvest_context(&options.harvest_context)),
            )?;
        }
    }
    // The generated ID is random by design. Link the execution only after its
    // materialized plan is durable, so a resumed controller selects this exact
    // run without replacing its baseline-bound workspace contract.
    if !attempt_needs_execution(&next_run_id) {
        agent_task_lifecycle::record_cook_attempt(cook_id, next_attempt, &next_run_id)?;
    }
    if review_form_only {
        // A form-only retry deliberately makes no code changes. Carry the
        // already-authenticated candidate forward so its finalization path can
        // consume the new form without selecting or applying a patch again.
        let mut carried_promotion = promotion.clone();
        carried_promotion.source.run_id = Some(next_run_id.clone());
        carried_promotion.provenance["cook_follow_up"] = serde_json::json!({
            "kind": "review_form_only",
            "source_run_id": source_run_id,
        });
        agent_task_lifecycle::record_promotion(
            &next_run_id,
            serde_json::to_value(carried_promotion)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
        )?;
    }
    remediation_category_usage.add(reservation);
    Ok(CookFollowUpDispatch::Dispatched {
        run_id: next_run_id,
    })
}

fn adopted_attempt_is_ready_for_cook_continuation(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<String>> {
    let Some(promotion) = persisted_promotion_for_attempt(&record.run_id)? else {
        return Ok(None);
    };
    let source_record = promotion.provenance["cook_follow_up"]["source_run_id"]
        .as_str()
        .map(agent_task_lifecycle::status)
        .transpose()?;
    let adoption = record
        .candidate_adoption
        .as_ref()
        .or_else(|| source_record.as_ref()?.candidate_adoption.as_ref());
    let Some(adoption) = adoption else {
        return Ok(None);
    };
    if adoption.state != "completed" {
        return Ok(None);
    }
    let provenance = &promotion.provenance["adoption"];
    if provenance["candidate_ref"].as_str() == Some(adoption.candidate_sha.as_str())
        && provenance["ai_model"].as_str() == Some(adoption.ai_model.as_str())
    {
        return Ok(Some(adoption.ai_model.clone()));
    }
    Ok(None)
}

fn review_form_attempt_is_ready_for_cook_continuation(
    plan: &AgentTaskPlan,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<bool> {
    let Some(task) = plan.tasks.first() else {
        return Ok(false);
    };
    if task.inputs["cook_loop"]["review_form_required"] != true
        || !budget_authority_matches_scope(
            &task.inputs["cook_loop"]["execution_budget_authority"],
            follow_up_budget_scope(task, task),
        )
    {
        return Ok(false);
    }
    let Some(promotion) = persisted_promotion_for_attempt(&record.run_id)? else {
        return Ok(false);
    };
    let Some(source_run_id) = promotion.provenance["cook_follow_up"]["source_run_id"].as_str()
    else {
        return Ok(false);
    };
    if promotion.provenance["cook_follow_up"]["kind"] != "review_form_only" {
        return Ok(false);
    }
    let Some(source) = persisted_promotion_for_attempt(source_run_id)? else {
        return Ok(false);
    };
    Ok(promotion.status == source.status
        && promotion.target == source.target
        && promotion.to_worktree == source.to_worktree
        && promotion.patch_artifact == source.patch_artifact
        && promotion.changed_files == source.changed_files
        && promotion.verified_base == source.verified_base
        && promotion.deterministic_gates == source.deterministic_gates
        && promotion.command_evidence == source.command_evidence
        && promotion.provenance.get("candidate") == source.provenance.get("candidate")
        && promotion.provenance.get("gate_feedback_baseline")
            == source.provenance.get("gate_feedback_baseline"))
}

fn retryable_review_form_terminal_failure(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> bool {
    retryable_pre_execution_failure(record)
        || retryable_provider_discovery_failure(&record.run_id)
        || aggregate.outcomes.iter().any(|outcome| {
            outcome.status == crate::agent_task::AgentTaskOutcomeStatus::Timeout
                || outcome
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.class == "agent_task.provider_timeout")
        })
}

/// Whether a terminal review-form attempt may re-enter Cook's historical
/// continuation path. This is intentionally narrower than terminality: only
/// authenticated form-only continuations with a provider timeout or an
/// existing retryable provider/pre-execution signal may execute again.
pub fn terminal_review_form_continuation_is_eligible(
    plan: &AgentTaskPlan,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<bool> {
    if !matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Failed
            | agent_task_lifecycle::AgentTaskRunState::PartialFailure
    ) || !review_form_attempt_is_ready_for_cook_continuation(plan, record)?
    {
        return Ok(false);
    }
    let aggregate = match agent_task_lifecycle::read_aggregate(&record.run_id) {
        Ok(aggregate) => aggregate,
        Err(_) => return Ok(false),
    };
    Ok(retryable_review_form_terminal_failure(record, &aggregate))
}

/// Validate the read-only admission boundary for a reconstructed continuation.
/// This deliberately stops before recipe/lifecycle materialization, transport
/// preparation, provider dispatch, and finalization.
pub fn preflight_cook_continuation_admission(options: &AgentTaskCookServiceOptions) -> Result<()> {
    let moving_base_continuation = agent_task_lifecycle::status(&options.initial_run_id)
        .ok()
        .and_then(|record| record.metadata.get("cook_moving_base_recovery").cloned())
        .is_some();
    let verification_pending_continuation = agent_task_lifecycle::status(&options.initial_run_id)
        .ok()
        .is_some_and(|record| {
            record
                .metadata
                .pointer("/latest_promotion/status")
                .and_then(Value::as_str)
                == Some("verification_pending")
        });
    let authenticated_historical_review_continuation =
        authenticated_historical_review_form_workspace(options)?;
    if !moving_base_continuation
        && !verification_pending_continuation
        && !authenticated_historical_review_continuation
        && options.attempt_dispatcher.is_none()
        && options.provider_command.is_none()
        && options.provider_invocation.is_none()
    {
        crate::agent_task_promotion::preflight_configured_workspace_provider(&options.to_worktree)?;
    }
    if cook_attempt_needs_execution(&options.initial_run_id)
        && (options.attempt_dispatcher.is_none() || options.source_worktree_path.is_some())
    {
        validate_cook_workspace(options)?;
    }
    validate_cook_candidate_group(&options.initial_plan)
}

pub fn run_cook<E>(
    options: AgentTaskCookServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_cook_with_finalizer(options, executor, finalize_or_load_cook_pr)
}

pub fn run_terminal_cook_continuation<E>(
    options: AgentTaskCookServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let side_effects = DefaultCookSideEffects::new(finalize_or_load_cook_pr);
    run_cook_with_boundaries_observed_policy(options, executor, side_effects, None, true)
}

/// Run Cook while reporting the authoritative attempt only after its durable
/// recipe has been persisted. Callers must treat pre-observer work as
/// invocation-local because no run recovery identity exists yet.
pub fn run_cook_with_durable_observer<E>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    observer: &CookProgressObserver<'_>,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let side_effects = DefaultCookSideEffects::new(finalize_or_load_cook_pr);
    run_cook_with_boundaries_observed(options, executor, side_effects, Some(observer))
}

pub(crate) fn run_cook_with_finalizer<E, F>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    finalize: F,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
{
    let side_effects = DefaultCookSideEffects::new(finalize);
    run_cook_with_boundaries(options, executor, side_effects)
}

fn run_cook_with_boundaries<E, S>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    side_effects: S,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    S: CookSideEffectService,
{
    run_cook_with_boundaries_observed(options, executor, side_effects, None)
}

/// The component a cook is working on, for notification attribution.
///
/// Read from the plan's own component contract rather than parsed out of the
/// worktree handle, so a renamed or detached worktree cannot mislabel it.
fn cook_component(options: &AgentTaskCookServiceOptions) -> Option<String> {
    options
        .initial_plan
        .component_contracts
        .iter()
        .chain(
            options
                .initial_plan
                .tasks
                .iter()
                .flat_map(|task| task.component_contracts.iter()),
        )
        .find_map(|contract| contract.slug.clone())
}

pub(crate) fn exhausted_budget_guidance(
    max_attempts: u32,
    budget: &AgentTaskExecutionBudget,
    reason: &str,
    review_form_only: bool,
) -> String {
    let remediation = if review_form_only {
        "review-form remediation"
    } else {
        "gate or provider remediation"
    };
    format!(
        "Cook stopped during {remediation} because {reason} was exhausted. Configured budget: --max-attempts {}, --max-provider-executions {}, --max-same-provider-retries {}, --max-provider-rotations {}. Start a new Cook with `--max-attempts {} --max-provider-executions {} --max-same-provider-retries {}`; rotations cannot fund same-provider gate or review-form retries.",
        max_attempts,
        budget.max_provider_executions,
        budget.max_same_provider_retries,
        budget.max_provider_rotations,
        max_attempts,
        max_attempts,
        max_attempts.saturating_sub(1),
    )
}

fn run_cook_with_boundaries_observed<E, S>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    side_effects: S,
    durable_observer: Option<&CookProgressObserver<'_>>,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    S: CookSideEffectService,
{
    run_cook_with_boundaries_observed_policy(
        options,
        executor,
        side_effects,
        durable_observer,
        false,
    )
}

fn run_cook_with_boundaries_observed_policy<E, S>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    side_effects: S,
    durable_observer: Option<&CookProgressObserver<'_>>,
    allow_historical_terminal: bool,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    S: CookSideEffectService,
{
    // Every exit from the observed boundary funnels through one notification
    // point — including the durable-failure report built from a controller
    // error — so the failure path is not the one that stays silent.
    let notification_options = options.clone();
    let result = run_cook_with_boundaries_reported(
        options,
        executor,
        side_effects,
        durable_observer,
        allow_historical_terminal,
    );
    if let Ok(result) = &result {
        if result.value.disposition.is_terminal() {
            crate::agent_task_notify::cook_terminal(
                &result.value,
                cook_component(&notification_options).as_deref(),
                result.exit_code,
            );
        }
    }
    result
}

fn run_cook_with_boundaries_reported<E, S>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    side_effects: S,
    durable_observer: Option<&CookProgressObserver<'_>>,
    allow_historical_terminal: bool,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    S: CookSideEffectService,
{
    let failure_options = options.clone();
    let result = match run_cook_with_boundaries_observed_inner(
        options,
        executor,
        side_effects,
        durable_observer,
        allow_historical_terminal,
    ) {
        Ok(result) => result,
        Err(error) => return durable_cook_error_report(&failure_options, error),
    };
    if let Some(run_id) = result.value.latest_run_id.as_deref() {
        let attempt = result
            .value
            .attempts
            .last()
            .map(|attempt| attempt.attempt)
            .unwrap_or(1);
        let phase = result.value.disposition.phase();
        if let Err(error) = report_cook_progress(
            durable_observer,
            &result.value.cook_id,
            run_id,
            phase,
            attempt,
            Some(&result.value.status),
        ) {
            return durable_cook_error_report(&failure_options, error);
        }
        if phase == "terminal" {
            if let Err(error) = agent_task_lifecycle::record_cook_terminal_result(
                run_id,
                result.exit_code == 0,
                result.exit_code,
            ) {
                return durable_cook_error_report(&failure_options, error);
            }
        }
    }
    Ok(result)
}

/// Convert an error that occurs after recipe materialization into the normal
/// Cook result contract. Errors before materialization still return unchanged:
/// they have no durable identity and therefore no legal recovery command.
fn durable_cook_error_report(
    options: &AgentTaskCookServiceOptions,
    error: Error,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    if super::recipe_exists(&options.cook_id)? {
        let mut report = cook_report(CookReportInput {
            cook_id: options.cook_id.clone(),
            status: "durable_failure",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: Some(
                "Cook stopped after durable creation; use the recovery actions in \
                 failure_context to inspect or continue it."
                    .to_string(),
            ),
            exit_code: 1,
            // A controller failure still belongs to THIS invocation. Without
            // this, the report — and every recovery command
            // `cook_failure_context` stamps with it — falls back to the
            // cross-invocation Cook index and can name a prior session's run.
            invocation_latest_run_id: Some(&options.initial_run_id),
        });
        if let Some(context) = &mut report.value.failure_context {
            // This is a controller failure, not a provider attempt. Keep a
            // bounded, redacted cause so continuation never needs redispatch.
            context.phase = "controller".to_string();
            context.reason_code = error.code.as_str().to_string();
            context.diagnostic = Some(bounded_error_diagnostic(&error));
        }
        return Ok(report);
    }
    Err(error)
}

fn run_cook_with_boundaries_observed_inner<E, S>(
    mut options: AgentTaskCookServiceOptions,
    executor: E,
    mut side_effects: S,
    durable_observer: Option<&CookProgressObserver<'_>>,
    allow_historical_terminal: bool,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    S: CookSideEffectService,
{
    // The local detached launcher persists this fence before spawn. Recheck it
    // at each durable/external boundary so a dead launcher cannot revive work.
    agent_task_lifecycle::require_detached_cook_handoff_fence_open(&options.cook_id)?;
    // Validate new Cooks before provider discovery, workspace staging, recipe
    // persistence, or a detached handoff can spend provider work. Historical
    // immutable recipes retain their persisted behavior and receive actionable
    // exhaustion guidance if their legacy budget reaches remediation.
    if !super::recipe_exists(&options.cook_id)? {
        validate_effective_cook_budget(
            options.max_attempts,
            &options.initial_plan.options.execution_budget,
        )?;
    }
    project_initial_finalizing_review_form_contract(&mut options);
    // A configured provider is controller authority. Resolve it before an
    // external runner can spend a provider attempt; explicit transports are
    // caller-owned overrides and retain their existing behavior. A typed
    // moving-base continuation has already completed provider work and must
    // not require a provider merely to rebase and reverify its candidate.
    let moving_base_continuation = agent_task_lifecycle::status(&options.initial_run_id)
        .ok()
        .and_then(|record| record.metadata.get("cook_moving_base_recovery").cloned())
        .is_some();
    let verification_pending_continuation = agent_task_lifecycle::status(&options.initial_run_id)
        .ok()
        .is_some_and(|record| {
            record
                .metadata
                .get("cook_recovery_source_checkpoint")
                .and_then(|checkpoint| checkpoint.get("phase"))
                .and_then(Value::as_str)
                == Some("verification_pending")
                || record
                    .metadata
                    .get("latest_promotion")
                    .and_then(|promotion| promotion.get("status"))
                    .and_then(Value::as_str)
                    == Some("verification_pending")
        });
    let authenticated_historical_review_continuation = if allow_historical_terminal {
        authenticated_historical_review_form_workspace(&options)?
    } else {
        false
    };
    if !moving_base_continuation
        && !verification_pending_continuation
        && !authenticated_historical_review_continuation
        && options.attempt_dispatcher.is_none()
        && options.provider_command.is_none()
        && options.provider_invocation.is_none()
    {
        crate::agent_task_promotion::preflight_configured_workspace_provider(&options.to_worktree)?;
    }
    // The durable reconstruction boundary must exist before an external provider
    // can accept the first attempt.
    let adopted_model = agent_task_lifecycle::status(&options.initial_run_id)
        .ok()
        .map(|record| adopted_attempt_is_ready_for_cook_continuation(&record))
        .transpose()?
        .flatten();
    let existing_recipe = super::recipe_exists(&options.cook_id)?;
    // A form-only continuation has already appended and persisted its exact
    // attempt. Re-persisting it as a fresh initial recipe would falsely look
    // like an unsafe post-gate correction because the durable lineage now has
    // more attempts than the caller's one-attempt input.
    let recipe = if existing_recipe {
        let recipe = super::load_recipe(&options.cook_id)?;
        if recipe
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == options.initial_run_id)
        {
            recipe
        } else {
            super::persist_initial_recipe(&options)?
        }
    } else {
        agent_task_lifecycle::require_detached_cook_handoff_fence_open(&options.cook_id)?;
        super::persist_initial_recipe(&options)?
    };
    // A recipe can survive an interruption before its first lifecycle record.
    // Resume from the validated durable inputs so ambient transport state cannot
    // turn replay into a conflicting new cook.
    let requested_run_id = options.initial_run_id.clone();
    let mut options = if existing_recipe {
        let mut reconstructed = if adopted_model.is_some() || allow_historical_terminal {
            super::reconstruct_adoption_options_with_dispatcher(
                &recipe,
                options.attempt_dispatcher,
            )?
        } else {
            super::reconstruct_options_with_dispatcher(&recipe, options.attempt_dispatcher)?
        };
        if let Some(attempt) = recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == requested_run_id)
        {
            reconstructed.initial_run_id = attempt.run_id.clone();
            reconstructed.initial_plan = attempt.plan.clone();
            if agent_task_lifecycle::run_record_exists(&attempt.run_id)? {
                // The recipe freezes the task-worktree handle, while the
                // durable run plan freezes the baseline-bound continuation.
                // Resolve the former back to its active path: the baseline is
                // execution evidence, not a task-worktree identity.
                let continuation_plan = agent_task_lifecycle::load_plan(&attempt.run_id)?;
                rebind_baseline_continuation_workspace(&mut reconstructed, &continuation_plan)?;
                reconstructed.initial_plan = continuation_plan;
            }
        }
        reconstructed
    } else {
        options
    };
    // Candidate adoption records the concrete external model on the lifecycle
    // attempt. Reuse it only when the persisted promotion authenticates the
    // same candidate/model pair, including after a detached continuation.
    if let Some(model) = adopted_model {
        options.ai_model = Some(model);
    }
    // A persisted recipe can replace the just-validated inputs. Re-check its
    // workspace and candidate topology before it reaches transport preparation
    // or a resumed attempt.
    // A detached transport may own workspace materialization on the runner.
    // Its initial handoff has no controller-local source path to validate yet;
    // requiring one here turns an accepted detached retry into a local failure.
    if cook_attempt_needs_execution(&options.initial_run_id)
        && (options.attempt_dispatcher.is_none() || options.source_worktree_path.is_some())
    {
        validate_cook_workspace(&options)?;
    }
    validate_cook_candidate_group(&options.initial_plan)?;
    // Reserve the source tree's projected copy before the scheduler creates its
    // controller scratch lease or detached workspace. This includes dependency
    // trees (for example node_modules and vendor), whose inode demand can be
    // decisive even when their byte footprint is small.
    let _materialization_capacity = options
        .source_worktree_path
        .as_deref()
        .map(std::path::Path::new)
        .or_else(|| {
            options
                .initial_plan
                .tasks
                .first()
                .and_then(|task| task.workspace.root.as_deref())
                .map(std::path::Path::new)
        })
        .map(|workspace| {
            let demand = homeboy_core::capacity::demand_for_tree(workspace)?;
            homeboy_core::capacity::reserve_projected_capacity(
                &homeboy_core::paths::controller_scratch_store()?,
                "Cook controller scratch and workspace materialization",
                demand,
                homeboy_core::capacity::CapacityReserve::configured(),
            )
        })
        .transpose()?;
    agent_task_lifecycle::require_detached_cook_handoff_fence_open(&options.cook_id)?;
    materialize_initial_cook_attempt(&options)?;
    record_active_cook_worktree_warning(&options)?;
    // Durable identity now exists and resolves through the Cook alias. Publish
    // it before every remaining long controller phase — gate toolchain
    // preflight, transport preparation, and Lab materialization — so a caller
    // interrupted at any later point can still answer "what did I just start?"
    // from the first identity-bearing bytes it received. `provider_ready` used
    // to be the first identity-bearing observer event, which put the operator
    // handle behind work that can outlive a client timeout (#10419, #9163).
    report_cook_progress(
        durable_observer,
        &options.cook_id,
        &options.initial_run_id,
        "durable_identity",
        1,
        None,
    )?;
    // The same boundary, delivered to the operator's destination: this is the
    // first moment the cook can be watched, diagnosed, or cancelled by id.
    let notify_component = cook_component(&options);
    crate::agent_task_notify::cook_started(
        &options.cook_id,
        &options.initial_run_id,
        &options.title,
        notify_component.as_deref(),
        &options.base,
        options.max_attempts,
        &options.ai_tool,
    );
    if options.gates.has_npm_run_declaration() {
        let gate_workspace = options.source_worktree_path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "Cook requires a workspace before gate declaration preflight",
                Some(options.to_worktree.clone()),
                None,
            )
        })?;
        if let Err(error) = options
            .gates
            .preflight_declarations(std::path::Path::new(gate_workspace))
        {
            let error = with_pre_execution_phase(error, "gate_declaration_preflight");
            record_pre_execution_failure(
                &options.initial_plan,
                &options.initial_run_id,
                &error,
                "gate_declaration_preflight",
            )?;
            return Ok(pre_execution_failure_report(
                options.cook_id.clone(),
                Vec::new(),
                pre_execution_failure_details(
                    agent_task_lifecycle::exact_record(&options.initial_run_id)
                        .ok()
                        .as_ref(),
                    &error,
                ),
                error,
                Some(&options.initial_run_id),
            ));
        }
    }
    let required_toolchains = options.gates.required_toolchains();
    let preflight = if required_toolchains.is_empty()
        && options.gates.gate_package_artifacts.is_empty()
        && options.gates.gate_environment.extension_inputs.is_empty()
    {
        Ok(())
    } else {
        let gate_workspace = options.source_worktree_path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "Cook requires a workspace before gate toolchain preflight",
                Some(options.to_worktree.clone()),
                None,
            )
        })?;
        crate::agent_task_gate::preflight_gate_toolchains(
            gate_workspace,
            &options.gates.gate_environment,
            &required_toolchains,
            &options.gates.gate_package_artifacts,
            None,
            options.gates.gate_timeout(),
        )
    };
    if let Err(error) = preflight {
        let error = with_pre_execution_phase(error, "gate_toolchain_preflight");
        record_pre_execution_failure(
            &options.initial_plan,
            &options.initial_run_id,
            &error,
            "gate_toolchain_preflight",
        )?;
        return Ok(pre_execution_failure_report(
            options.cook_id.clone(),
            Vec::new(),
            pre_execution_failure_details(
                agent_task_lifecycle::exact_record(&options.initial_run_id)
                    .ok()
                    .as_ref(),
                &error,
            ),
            error,
            Some(&options.initial_run_id),
        ));
    }
    if let Some(latest_attempt) = recipe.attempts.last() {
        materialize_cook_attempt(
            &recipe.cook_id,
            &latest_attempt.run_id,
            &latest_attempt.plan,
        )?;
    }
    // The recipe alone is resumable input, not a status-addressable run. Publish
    // the run identity only after initial materialization and a lifecycle read
    // prove status/log recovery resolves for this exact attempt.
    let materialized_run = agent_task_lifecycle::status(&options.initial_run_id)?;
    if materialized_run.run_id != options.initial_run_id {
        return Err(Error::internal_unexpected(
            "materialized Cook lifecycle record does not match its initial run id",
        ));
    }
    report_cook_progress(
        durable_observer,
        &options.cook_id,
        &options.initial_run_id,
        "provider_ready",
        1,
        None,
    )?;
    // Transport readiness can serialize on a reconnect/runtime-promotion
    // lease. Complete it before entering the provider-attempt loop so that
    // waiting for a shared Lab session never consumes a cook attempt.
    if !verification_pending_continuation {
        if let Some(dispatcher) = &options.attempt_dispatcher {
            if let Err(error) = dispatcher.prepare_for_cook() {
                agent_task_lifecycle::record_pre_execution_failure(
                    &options.initial_run_id,
                    &options.initial_plan,
                    dispatcher.pre_execution_failure_phase(),
                    &error,
                )?;
                return Err(error);
            }
        }
    }
    // The initial attempt is the durable status/activity owner. Pin it rather
    // than the stable cook ID, which may not itself name a lifecycle record.
    let _runtime_generation =
        homeboy_core::runtime_promotion::pin_cook_generation(&options.initial_run_id)?;
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    // A retry may already be durably dispatched when this controller resumes.
    // Continue from that exact recorded attempt rather than re-entering the
    // original attempt and re-binding its immutable recipe identity.
    let requested_attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == options.initial_run_id)
        .map(|attempt| attempt.attempt)
        .unwrap_or(1);
    let resumed_run_id = resumable_cook_run_id(
        &recipe,
        &options.cook_id,
        &options.initial_run_id,
        requested_attempt,
        verification_pending_continuation,
    );
    let mut run_id = resumed_run_id
        .clone()
        .unwrap_or_else(|| options.initial_run_id.clone());
    let mut next_plan = resumed_run_id
        .is_none()
        .then(|| options.initial_plan.clone());
    let cook_id = options.cook_id.clone();
    let mut budget_limit = None;
    let mut observed_budget_used = ExecutionBudgetUsage::default();
    let mut remediation_category_usage = ExecutionBudgetUsage::default();

    let first_attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .map(|attempt| attempt.attempt)
        .unwrap_or(1);
    let continuation_attempt_limit = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .and_then(|attempt| attempt.plan.tasks.first())
        .filter(|task| {
            task.inputs["cook_loop"]["review_form_required"] == true
                && matches!(
                    task.inputs["cook_loop"]["execution_budget_authority"]["kind"].as_str(),
                    Some("fresh_cook_review" | "candidate_adoption_review")
                )
        })
        .and_then(|task| {
            task.inputs["cook_loop"]["execution_budget_authority"]["max_same_provider_retries"]
                .as_u64()
        })
        .and_then(|retries| u32::try_from(retries).ok())
        .unwrap_or(0);
    let attempt_limit = if continuation_attempt_limit > 0 {
        first_attempt.saturating_add(continuation_attempt_limit)
    } else {
        max_attempts
    };
    for attempt in first_attempt..=attempt_limit {
        // Attempt boundary. Checked before any provider work is dispatched, so
        // an expired budget costs nothing further and the attempt that would
        // have run is never started rather than started and killed.
        if let Some(deadline) = expired_cook_deadline() {
            return Ok(cook_deadline_report(
                cook_id,
                attempts,
                &run_id,
                &deadline,
                &format!("attempt {attempt} could be started"),
            ));
        }
        let plan = match next_plan.take() {
            Some(plan) => plan,
            None => agent_task_lifecycle::load_plan(&run_id)?,
        };
        let needs_execution = cook_attempt_needs_execution(&run_id);
        if needs_execution {
            // The local detached launcher persists this fence before spawn.
            // Recheck immediately before this attempt can publish provider work.
            agent_task_lifecycle::require_detached_cook_handoff_fence_open(&cook_id)?;
            // `provider_start` is a durable lifecycle transition. Create the
            // record before reporting it so a new Cook run is observable before
            // any preflight or provider work can block.
            if !agent_task_lifecycle::run_record_exists(&run_id)? {
                agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
            }
            report_cook_progress(
                durable_observer,
                &cook_id,
                &run_id,
                if attempt == 1 {
                    "provider_start"
                } else {
                    "retry"
                },
                attempt,
                None,
            )?;
            // Attempt boundaries only. The first attempt is already covered by
            // the started event, and the fifteen-second heartbeat is internal
            // liveness with no decision attached to it.
            if attempt > 1 {
                crate::agent_task_notify::cook_retrying(
                    &cook_id,
                    &run_id,
                    notify_component.as_deref(),
                    attempt,
                    attempt_limit,
                );
            }
            if options.attempt_dispatcher.is_none() || options.source_worktree_path.is_some() {
                validate_cook_workspace(&options)?;
            }
            let mut failed_dispatch_plan = None;
            let execution = (|| {
                homeboy_core::cleanup::admit_reconstructable_artifact_work(
                    plan.tasks
                        .iter()
                        .filter_map(|task| task.workspace.root.as_ref())
                        .map(PathBuf::from)
                        .collect(),
                )
                .map_err(|error| with_pre_execution_phase(error, "worktree_capacity_admission"))?;
                let initial_baseline = if attempt == 1 {
                    materialize_initial_candidate_baseline(
                        &plan,
                        options.source_worktree_path.as_deref(),
                        &run_id,
                    )
                    .map_err(|error| {
                        with_pre_execution_phase(error, "materialize_initial_candidate_baseline")
                    })?
                } else {
                    None
                };
                // For follow-up attempts (attempt > 1), the plan's workspace.root
                // was set by a previous dispatch_cook_follow_up to a baseline
                // worktree path. If that worktree was reaped (e.g. by tmp cleanup,
                // disk-pressure cleanup, or git worktree prune), re-materialize it
                // at the same path so the provider preflight check passes.
                let re_materialized_baseline = if attempt > 1 && initial_baseline.is_none() {
                    let root = plan
                        .tasks
                        .first()
                        .and_then(|t| t.workspace.root.as_deref())
                        .map(std::path::Path::new);
                    match root {
                        Some(path) if !path.exists() => {
                            let source_run_id = plan.tasks[0]
                                .inputs["cook_loop"]["artifact_provenance"]["source_run_id"]
                                .as_str()
                                .ok_or_else(|| {
                                    with_pre_execution_phase(
                                        Error::validation_invalid_argument(
                                            "cook_loop.artifact_provenance.source_run_id",
                                            "follow-up plan missing source run id for baseline re-materialization",
                                            None,
                                            None,
                                        ),
                                        "re_materialize_follow_up_baseline",
                                    )
                                })?;
                            let promotion = persisted_promotion_for_attempt(source_run_id)?
                                .ok_or_else(|| {
                                    with_pre_execution_phase(
                                        Error::validation_invalid_argument(
                                            "promotion",
                                            format!(
                                                "source attempt {source_run_id} has no persisted \
                                                 promotion for baseline re-materialization"
                                            ),
                                            Some(source_run_id.to_string()),
                                            None,
                                        ),
                                        "re_materialize_follow_up_baseline",
                                    )
                                })?;
                            let task_id = &plan.tasks[0].task_id;
                            Some(
                                re_materialize_follow_up_baseline(
                                    &promotion,
                                    path,
                                    source_run_id,
                                    task_id,
                                )
                                .map_err(|error| {
                                    with_pre_execution_phase(
                                        error,
                                        "re_materialize_follow_up_baseline",
                                    )
                                })?,
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let effective_baseline = initial_baseline
                    .as_ref()
                    .or(re_materialized_baseline.as_ref());
                let mut dispatch_plan = plan.clone();
                if let Some(baseline) = effective_baseline {
                    for task in &mut dispatch_plan.tasks {
                        // The baseline is immutable evidence for this dispatch,
                        // never the durable workspace a retry continues in.
                        task.metadata["cook_continuation_workspace"] = serde_json::json!({
                            "candidate_source_root": options.source_worktree_path,
                            "task_workspace": {
                                "root": task.workspace.root.clone(),
                                "kind": task.workspace.kind.clone(),
                                "materialization": task.workspace.materialization.clone(),
                            },
                        });
                        task.workspace.root = Some(baseline.path.display().to_string());
                        task.metadata["cook_initial_candidate_baseline"] = serde_json::json!({
                            "source_root": options.source_worktree_path,
                            "commit": baseline.capability.commit(),
                            "tree": baseline.capability.tree(),
                        });
                    }
                }
                if options.attempt_dispatcher.is_none() || options.source_worktree_path.is_some() {
                    bind_dispatch_workspace_attestations(&mut dispatch_plan)?;
                }
                failed_dispatch_plan = Some(dispatch_plan.clone());
                if let Some(dispatcher) = &options.attempt_dispatcher {
                    if options.source_worktree_path.is_some() {
                        validate_cook_workspace(&options)?;
                    }
                    dispatcher.dispatch_attempt(
                        dispatch_plan,
                        &run_id,
                        effective_baseline.map(CookFollowUpBaseline::capability),
                    )
                } else {
                    validate_cook_workspace(&options)?;
                    let (heartbeat_stop, heartbeat_wait) = mpsc::channel();
                    let heartbeat_run_id = run_id.clone();
                    let heartbeat_cook_id = cook_id.clone();
                    // Bind the probe to the workspace this dispatch actually
                    // hands the provider, not to the cook's configured
                    // destination: a follow-up attempt runs in a baseline
                    // worktree, and measuring the wrong tree would report
                    // "no files written" while the provider was editing.
                    let heartbeat_activity = CookActivityProbe::new(
                        dispatch_plan
                            .tasks
                            .first()
                            .and_then(|task| task.workspace.root.as_deref())
                            .map(PathBuf::from)
                            .or_else(|| options.source_worktree_path.clone()),
                    );
                    // The provider runs as a descendant of this controller
                    // process, so the controller pid is the root of the tree to
                    // sample — and, when a budget orders a stop, the root of
                    // the tree to terminate. `terminate_process_tree` excludes
                    // the calling process, so this ends the provider's work
                    // without ending the controller that has to record why.
                    let heartbeat_owner_pid = std::process::id();
                    // Resolved before the thread so a malformed policy document
                    // fails the attempt loudly. An operator who declared a stop
                    // budget and silently got none believes this host is
                    // protected and is not (#7015).
                    let supervision_policy = resolve_supervision_policy()?;
                    let supervision_backend = dispatch_plan
                        .tasks
                        .first()
                        .map(|task| task.executor.backend.clone());
                    // Captured before the spawn: the budget lives in a
                    // thread-local, and the heartbeat runs on a thread of its
                    // own that would otherwise start unbudgeted.
                    let heartbeat_deadline = capture_cook_deadline();
                    std::thread::scope(|scope| {
                        scope.spawn(move || {
                            let mut supervisor =
                                CookSupervisor::new(supervision_policy, supervision_backend);
                            let mut deadline_stop_issued = false;
                            while let Err(mpsc::RecvTimeoutError::Timeout) =
                                heartbeat_wait.recv_timeout(COOK_HEARTBEAT_INTERVAL)
                            {
                                let activity_owner_pid = agent_task_lifecycle::running_owner_pid(
                                    &heartbeat_run_id,
                                )
                                .ok()
                                .flatten()
                                .unwrap_or(heartbeat_owner_pid);
                                let activity = heartbeat_activity.sample(activity_owner_pid);
                                let tick = supervisor.observe(&activity);
                                let detail = tick.detail_line();
                                let _ = report_cook_progress_with_activity(
                                    durable_observer,
                                    &heartbeat_cook_id,
                                    &heartbeat_run_id,
                                    "heartbeat",
                                    attempt,
                                    Some(
                                        detail
                                            .as_deref()
                                            .unwrap_or("provider execution is still running"),
                                    ),
                                    (!activity.is_empty()).then_some(&activity),
                                );
                                // Supervision evidence is written even when the
                                // policy is undeclared: the resource timeline is
                                // how "was this run expensive?" stops being a
                                // question only answerable by having watched it.
                                if !tick.is_empty() {
                                    let _ = agent_task_lifecycle::record_cook_supervision(
                                        &heartbeat_run_id,
                                        attempt,
                                        (!tick.sample.is_empty())
                                            .then(|| serde_json::to_value(tick.sample))
                                            .and_then(std::result::Result::ok),
                                        tick.decisions
                                            .iter()
                                            .filter_map(|decision| {
                                                serde_json::to_value(decision).ok()
                                            })
                                            .collect(),
                                    );
                                }
                                if tick.stop_now {
                                    let outcome = match homeboy_core::process::terminate_process_tree(
                                        heartbeat_owner_pid,
                                    ) {
                                        Ok(termination) => serde_json::json!({
                                            "status": "terminated",
                                            "signal": termination.signal,
                                            "signalled_pids": termination.signalled_pids,
                                            "killed_pids": termination.killed_pids,
                                            "surviving_pids": termination.surviving_pids,
                                            "recovery_commands": termination.recovery_commands,
                                        }),
                                        // A host that cannot terminate a tree
                                        // (no process-tree cancellation, a pid
                                        // that outlives SIGKILL) must say so.
                                        // Recording the order alone would let a
                                        // reader conclude the run was stopped.
                                        Err(error) => serde_json::json!({
                                            "status": "termination_failed",
                                            "error": error.to_string(),
                                            "recovery_commands":
                                                homeboy_core::process::process_tree_recovery_commands(
                                                    heartbeat_owner_pid,
                                                ),
                                        }),
                                    };
                                    let _ = agent_task_lifecycle::record_cook_supervision_stop(
                                        &heartbeat_run_id,
                                        attempt,
                                        outcome,
                                    );
                                }
                                // A deadline that only fired at attempt and
                                // gate boundaries would not bound a single
                                // provider execution that runs past it, so the
                                // budget would be advisory for exactly the case
                                // it exists to bound. Expiry therefore takes
                                // the same route a supervision stop does:
                                // terminate the provider's process tree, from
                                // the same thread, recorded through the same
                                // durable evidence. Issued once — a second
                                // SIGKILL adds nothing and would overwrite the
                                // first outcome record.
                                if !deadline_stop_issued
                                    && heartbeat_deadline
                                        .deadline()
                                        .is_some_and(|deadline| deadline.is_expired())
                                {
                                    deadline_stop_issued = true;
                                    let outcome = match homeboy_core::process::terminate_process_tree(
                                        heartbeat_owner_pid,
                                    ) {
                                        Ok(termination) => serde_json::json!({
                                            "status": "terminated",
                                            "cause": "cook_deadline",
                                            "signal": termination.signal,
                                            "signalled_pids": termination.signalled_pids,
                                            "killed_pids": termination.killed_pids,
                                            "surviving_pids": termination.surviving_pids,
                                            "recovery_commands": termination.recovery_commands,
                                        }),
                                        // Same reasoning as the supervision
                                        // stop above: a host that cannot
                                        // terminate the tree must say so rather
                                        // than let a reader conclude the
                                        // provider was stopped.
                                        Err(error) => serde_json::json!({
                                            "status": "termination_failed",
                                            "cause": "cook_deadline",
                                            "error": error.to_string(),
                                            "recovery_commands":
                                                homeboy_core::process::process_tree_recovery_commands(
                                                    heartbeat_owner_pid,
                                                ),
                                        }),
                                    };
                                    let _ = agent_task_lifecycle::record_cook_supervision_stop(
                                        &heartbeat_run_id,
                                        attempt,
                                        outcome,
                                    );
                                }
                            }
                        });
                        let result = run_loaded_plan_with_derived_cook_baseline(
                            dispatch_plan,
                            Some(&run_id),
                            executor.clone(),
                            effective_baseline.map(CookFollowUpBaseline::capability),
                            Some(cook_attempt_harvest_context(&options.harvest_context)),
                        )
                        .map(|_| ());
                        let _ = heartbeat_stop.send(());
                        result
                    })
                }
            })();
            if let Err(error) = execution {
                if let Some(dispatch_plan) = failed_dispatch_plan.as_ref() {
                    // Baseline cleanup runs when the dispatch scope exits. Restore
                    // the exact continuation contract only after that cleanup so
                    // retry never loses its controller-owned plan.
                    agent_task_lifecycle::persist_controller_plan(&run_id, dispatch_plan)?;
                }
                let record = match agent_task_lifecycle::status(&run_id) {
                    Ok(record)
                        if record.state == agent_task_lifecycle::AgentTaskRunState::Queued =>
                    {
                        let phase = pre_execution_failure_phase(
                            &error,
                            options.attempt_dispatcher.as_deref(),
                        );
                        record_pre_execution_failure(&plan, &run_id, &error, phase)?;
                        agent_task_lifecycle::status(&run_id).ok()
                    }
                    Ok(record) => Some(record),
                    Err(_) => {
                        let phase = pre_execution_failure_phase(
                            &error,
                            options.attempt_dispatcher.as_deref(),
                        );
                        record_pre_execution_failure(&plan, &run_id, &error, phase)?;
                        agent_task_lifecycle::status(&run_id).ok()
                    }
                };
                let pre_execution_failure = pre_execution_failure_details(record.as_ref(), &error);
                agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: record
                        .as_ref()
                        .map(|record| format!("{:?}", record.state))
                        .unwrap_or_else(|| "DispatchFailed".to_string()),
                    aggregate_path: record
                        .as_ref()
                        .and_then(|record| record.aggregate_path.clone()),
                    promotion: None,
                    feedback: None,
                });
                if !pre_execution_failure.retryable {
                    return Ok(pre_execution_failure_report(
                        cook_id,
                        attempts,
                        pre_execution_failure,
                        error,
                        Some(&run_id),
                    ));
                }
                if attempt == max_attempts {
                    return Ok(cook_report(CookReportInput {
                        cook_id,
                        status: "retries_exhausted",
                        disposition: CookDisposition::Terminal,
                        attempts,
                        finalization: None,
                        stop_reason: Some(error.to_string()),
                        exit_code: 1,
                        invocation_latest_run_id: Some(&run_id),
                    }));
                }
                let next_attempt = attempt + 1;
                let next_run_id = agent_task_lifecycle::cook_attempt_run_id(&cook_id, next_attempt);
                super::record_recipe_attempt(&cook_id, next_attempt, &next_run_id, &plan)?;
                materialize_cook_attempt(&cook_id, &next_run_id, &plan)?;
                run_id = next_run_id;
                next_plan = Some(plan);
                continue;
            }
        }
        agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
        let mut record = agent_task_lifecycle::status(&run_id)?;
        // A local controller can disappear after the provider ledger records a
        // terminal result but before the run projection is terminalized. Repair
        // only this Cook attempt, never the active fleet, before deciding whether
        // an absent aggregate is safe to continue from.
        if record.state == agent_task_lifecycle::AgentTaskRunState::Running
            && record.is_stale_running()
            && record.aggregate_path.is_none()
        {
            super::reconcile_run(&run_id, false)?;
            record = agent_task_lifecycle::status(&run_id)?;
        }
        let controller_owned_staging = record
            .metadata
            .get("lab_staging_controller_job_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|job_id| !job_id.is_empty());
        if matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Queued
                | agent_task_lifecycle::AgentTaskRunState::Running
        ) && (record.runner_job_id().is_some() || controller_owned_staging)
        {
            // Detached staging or runner handoff has a durable owner. It owns
            // timeout and provider rotation, so Cook must not read a future
            // aggregate before that owner has produced it.
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(cook_report(CookReportInput {
                cook_id,
                status: "in_flight",
                disposition: CookDisposition::InFlight,
                attempts,
                finalization: None,
                stop_reason: Some("provider attempt accepted by the runner daemon".to_string()),
                exit_code: 0,
                invocation_latest_run_id: Some(&run_id),
            }));
        }
        let plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
        budget_limit.get_or_insert_with(|| plan.options.execution_budget.clone());
        let aggregate = match agent_task_lifecycle::read_aggregate(&run_id) {
            Ok(aggregate) => aggregate,
            // An aggregate path is authoritative evidence that an aggregate was
            // committed. Its read failure must surface for repair rather than be
            // misclassified as an interruption and bypass immutable output.
            Err(_error) if record.state.is_terminal() && record.aggregate_path.is_none() => {
                let phase = pre_artifact_interruption_phase(&record);
                // Aggregates normally provide this accounting. A missing
                // aggregate must still carry only ledger-proven executions
                // into later remediation budget decisions.
                observed_budget_used.executions = observed_budget_used
                    .executions
                    .saturating_add(pre_artifact_execution_count(&record));
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path.clone(),
                    promotion: None,
                    feedback: None,
                });
                if attempt >= max_attempts {
                    return Ok(pre_artifact_interruption_report(
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt} is terminal without aggregate evidence ({}) and the Cook retry budget is exhausted; inspect durable provider execution metadata before starting a new Cook",
                            phase.name(),
                        ),
                        1,
                    ));
                }
                let budget_limit = budget_limit
                    .as_ref()
                    .expect("budget is initialized from the loaded attempt plan");
                if budget_remaining(budget_limit, observed_budget_used).is_none() {
                    return Ok(pre_artifact_interruption_report(
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt} is terminal without aggregate evidence ({}) and its ledger-proven provider execution exhausts the Cook provider budget; inspect the durable attempt before increasing the budget",
                            phase.name(),
                        ),
                        1,
                    ));
                }
                match claim_pre_artifact_interruption_retry(&cook_id, attempt, &run_id, &plan)? {
                    Some((_next_attempt, next_run_id)) => {
                        run_id = next_run_id;
                        next_plan = Some(plan);
                        continue;
                    }
                    None => {
                        return Ok(pre_artifact_interruption_report(
                            cook_id,
                            attempts,
                            &run_id,
                            phase,
                            format!(
                                "attempt {attempt} is terminal without aggregate evidence ({}) and another controller is claiming its retry; resume Cook after the durable claim completes",
                                phase.name(),
                            ),
                            0,
                        ));
                    }
                }
            }
            Err(error) => return Err(error),
        };
        observed_budget_used.add(execution_budget_usage(&aggregate));
        let mut budget_used = observed_budget_used;
        budget_used.same_provider_retries = budget_used
            .same_provider_retries
            .saturating_add(remediation_category_usage.same_provider_retries);
        budget_used.provider_rotations = budget_used
            .provider_rotations
            .saturating_add(remediation_category_usage.provider_rotations);
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(cook_report(CookReportInput {
                cook_id,
                status: "policy_failure",
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: None,
                stop_reason: Some(
                    "agent-task cook requires a plan with one source task".to_string(),
                ),
                exit_code: 1,
                invocation_latest_run_id: Some(&run_id),
            }));
        };
        validate_cook_candidate_group(&plan)?;

        let adopted_continuation = adopted_attempt_is_ready_for_cook_continuation(&record)?;
        let review_form_continuation = allow_historical_terminal
            && review_form_attempt_is_ready_for_cook_continuation(&plan, &record)?
            && retryable_review_form_terminal_failure(&record, &aggregate);
        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
                | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
        ) && adopted_continuation.is_none()
            && !review_form_continuation
        {
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(cook_report(CookReportInput {
                cook_id,
                status: "provider_failure",
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: None,
                stop_reason: Some(format!(
                    "agent-task run {run_id} ended in state {:?}",
                    record.state
                )),
                exit_code: 1,
                invocation_latest_run_id: Some(&run_id),
            }));
        }

        if let Some(finalization) = record
            .metadata
            .get("cook_finalization")
            .filter(|finalization| !finalization.is_null())
            .cloned()
        {
            // A terminal child may outlive its coordinator. Its finalization is
            // the durable completion receipt, so harvesting it must not repeat
            // promotion, gates, or any provider-facing work.
            let status = finalization["status"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let exit_code = if matches!(status.as_str(), "review_ready" | "draft_published") {
                0
            } else {
                1
            };
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(cook_report(CookReportInput {
                cook_id,
                status: &status,
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: Some(finalization),
                stop_reason: None,
                exit_code,
                invocation_latest_run_id: Some(&run_id),
            }));
        }

        // Gate dispatch boundary. Promotion is where the deterministic gates
        // run, and gates are the expensive half of the budget: five gates at
        // the default 30-minute timeout outlast the provider execution that
        // produced the candidate. Checked before dispatch so an expired budget
        // does not start a fresh multi-hour gate sweep.
        //
        // The provider's candidate is already durable at this point, so this
        // stops without discarding work: the Cook is recoverable by the normal
        // continuation route once the operator grants more budget.
        if let Some(deadline) = expired_cook_deadline() {
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path.clone(),
                promotion: None,
                feedback: None,
            });
            return Ok(cook_deadline_report(
                cook_id,
                attempts,
                &run_id,
                &deadline,
                &format!("attempt {attempt} could enter promotion and run its gates"),
            ));
        }
        report_cook_progress(
            durable_observer,
            &cook_id,
            &run_id,
            "promotion",
            attempt,
            None,
        )?;
        let mut promotion = match side_effects.promote(&options, &run_id) {
            Ok(report) => report,
            Err(_error) => {
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                let recovery = "promotion provider response was rejected. The successful candidate remains durable; use failure_context to inspect or continue the Cook.".to_string();
                return Ok(cook_report(CookReportInput {
                    cook_id,
                    status: "policy_failure",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(recovery),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
        };
        if promotion.status.gate_failed() {
            let base_sha = promotion
                .verified_base
                .as_ref()
                .map(|base| base.sha.clone())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "promotion.verified_base",
                        "gate-failed Cook promotion has no immutable verified base for differential verification",
                        None,
                        None,
                    )
                })?;
            let repository_root = promotion.target.path.clone().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion.target.path",
                    "gate-failed Cook promotion has no candidate repository path for differential verification",
                    None,
                    None,
                )
            })?;
            compare_gate_failures_to_verified_base(
                &mut promotion,
                std::path::Path::new(&repository_root),
                &base_sha,
                options.gates.gate_timeout(),
                |_compared, _total| Ok(()),
            )?;
            agent_task_lifecycle::record_promotion(
                &run_id,
                serde_json::to_value(&promotion)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
            )?;
        }

        let review_form = review_form_from_aggregate(&aggregate)?;
        let intentional_no_change = intentional_no_change_from_aggregate(&aggregate);
        let previous_failure_set = attempts
            .last()
            .and_then(|attempt| attempt.feedback.as_ref())
            .and_then(|feedback| feedback.follow_up_request.as_ref())
            .and_then(|request| request.inputs.pointer("/cook_loop/failure_set"))
            .cloned()
            .unwrap_or(Value::Null);
        let feedback_promotion =
            if review_form_continuation && promotion.status == AgentTaskPromotionStatus::Applied {
                // Historical terminal review-form continuations can carry the
                // source attempt's failed gate entries even though their copied,
                // authenticated promotion is already applied. The applied status
                // is the durable gate authority for this form-only retry.
                let mut feedback_promotion = promotion.clone();
                feedback_promotion.deterministic_gates.retain(|gate| {
                    gate.status != crate::agent_task_gate::AgentTaskGateStatus::Failed
                });
                feedback_promotion
                    .gate_results
                    .retain(|gate| gate.status != homeboy_core::gate::HomeboyGateStatus::Failed);
                feedback_promotion
            } else {
                promotion.clone()
            };
        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request.clone(),
            promotion_report: feedback_promotion,
            attempt,
            max_attempts: attempt_limit,
            source_run_id: Some(run_id.clone()),
            current_diff: gate_feedback_current_diff(&promotion),
            // The form is publication evidence. A no-finalize Cook preserves
            // the verified patch for review without entering finalization.
            require_review_form: !options.no_finalize,
            review_form: review_form.clone(),
            metadata: serde_json::json!({
                "previous_failure_set": previous_failure_set,
                "intentional_no_change": intentional_no_change,
            }),
        });
        let feedback_status = feedback.status;
        let follow_up_request = feedback.follow_up_request.clone();
        attempts.push(AgentTaskCookAttemptReport {
            attempt,
            run_id: run_id.clone(),
            run_state: format!("{:?}", record.state),
            aggregate_path: record.aggregate_path,
            promotion: Some(promotion.clone()),
            feedback: Some(feedback.clone()),
        });

        match feedback_status {
            AgentTaskCookLoopStatus::IntentionalNoChange => {
                let declaration = feedback
                    .intentional_no_change
                    .expect("intentional no-change feedback carries its declaration");
                let mut report = cook_report(CookReportInput {
                    cook_id,
                    status: "intentional_no_change",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(format!(
                        "provider completed a verified {} review without a candidate patch; publication was skipped{}",
                        declaration.verdict,
                        declaration
                            .next_action
                            .is_empty()
                            .then(String::new)
                            .unwrap_or_else(|| format!("; next action: {}", declaration.next_action))
                    )),
                    exit_code: 0,
                    invocation_latest_run_id: Some(&run_id),
                });
                report.value.intentional_no_change = Some(AgentTaskCookIntentionalNoChangeReport {
                    declaration,
                    review_form,
                });
                return Ok(report);
            }
            AgentTaskCookLoopStatus::GreenCompleted => {
                if options.no_finalize {
                    return Ok(cook_report(CookReportInput {
                        cook_id,
                        status: "green_no_finalize",
                        disposition: CookDisposition::Terminal,
                        attempts,
                        finalization: None,
                        stop_reason: Some(
                            "deterministic gates completed green; --no-finalize skipped \
     commit, push, and PR finalization"
                                .to_string(),
                        ),
                        exit_code: 0,
                        invocation_latest_run_id: Some(&run_id),
                    }));
                }
                let mut active_moving_base_recovery = None;
                let promotion = match moving_base_recovery_for_run(&run_id)? {
                    Some(recovery) => match side_effects.recover_moving_base(&options, &recovery) {
                        Ok(promotion) => {
                            agent_task_lifecycle::record_promotion(
                                &run_id,
                                serde_json::to_value(&promotion).map_err(|error| {
                                    Error::internal_json(error.to_string(), None)
                                })?,
                            )?;
                            let recovery = refreshed_moving_base_recovery(recovery, &promotion);
                            agent_task_lifecycle::record_cook_moving_base_recovery(
                                &run_id,
                                serde_json::to_value(&recovery).map_err(|error| {
                                    Error::internal_json(error.to_string(), None)
                                })?,
                            )?;
                            if promotion.status != AgentTaskPromotionStatus::Applied {
                                let mut recovery = recovery;
                                recovery.blocker = format!(
                                    "rebased candidate did not pass the declared deterministic gates ({:?}); finalization was not attempted",
                                    promotion.status
                                );
                                agent_task_lifecycle::record_cook_moving_base_recovery(
                                    &run_id,
                                    serde_json::to_value(&recovery).map_err(|error| {
                                        Error::internal_json(error.to_string(), None)
                                    })?,
                                )?;
                                return Ok(moving_base_recovery_report(
                                    cook_id,
                                    attempts,
                                    recovery,
                                    false,
                                    Some(&run_id),
                                ));
                            }
                            active_moving_base_recovery = Some(recovery);
                            promotion
                        }
                        Err(error) => {
                            let recovery = next_moving_base_recovery(recovery, error.to_string());
                            agent_task_lifecycle::record_cook_moving_base_recovery(
                                &run_id,
                                serde_json::to_value(&recovery).map_err(|error| {
                                    Error::internal_json(error.to_string(), None)
                                })?,
                            )?;
                            if recovery.base_movements < 3 {
                                super::enqueue_terminal_continuation(&cook_id, &run_id)?;
                            }
                            let continuation_queued = recovery.base_movements < 3;
                            return Ok(moving_base_recovery_report(
                                cook_id,
                                attempts,
                                recovery,
                                continuation_queued,
                                Some(&run_id),
                            ));
                        }
                    },
                    None => promotion,
                };
                report_cook_progress(
                    durable_observer,
                    &cook_id,
                    &run_id,
                    "finalization",
                    attempt,
                    None,
                )?;
                let finalization = match side_effects.finalize(&options, &run_id, &promotion) {
                    Ok(finalization) => {
                        if active_moving_base_recovery.is_some() {
                            agent_task_lifecycle::clear_cook_moving_base_recovery(&run_id)?;
                        }
                        finalization
                    }
                    Err(error) if is_moving_base_finalization_error(&error) => {
                        let recovery = next_moving_base_recovery(
                            active_moving_base_recovery.unwrap_or_else(|| {
                                moving_base_recovery_from_promotion(&cook_id, &run_id, promotion)
                            }),
                            error.to_string(),
                        );
                        agent_task_lifecycle::record_cook_moving_base_recovery(
                            &run_id,
                            serde_json::to_value(&recovery)
                                .map_err(|error| Error::internal_json(error.to_string(), None))?,
                        )?;
                        if recovery.base_movements < 3 {
                            super::enqueue_terminal_continuation(&cook_id, &run_id)?;
                        }
                        let continuation_queued = recovery.base_movements < 3;
                        return Ok(moving_base_recovery_report(
                            cook_id,
                            attempts,
                            recovery,
                            continuation_queued,
                            Some(&run_id),
                        ));
                    }
                    Err(error) if error.message.contains("awaiting_acceptance") => {
                        return Ok(cook_report(CookReportInput {
                            cook_id,
                            status: "awaiting_acceptance",
                            disposition: CookDisposition::Terminal,
                            attempts,
                            finalization: Some(serde_json::json!({
                                "status": "awaiting_acceptance",
                                "reason": error.message,
                                "run_id": run_id,
                                "status_command": format!(
                                    "homeboy agent-task status {run_id} --full"
                                ),
                            })),
                            stop_reason: Some(
                                "deterministic gates passed; an independent acceptance verdict \
     is required before PR finalization"
                                    .to_string(),
                            ),
                            exit_code: 1,
                            invocation_latest_run_id: Some(&run_id),
                        }));
                    }
                    Err(error) => return Err(error),
                };
                let final_status = finalization["status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let exit_code =
                    if matches!(final_status.as_str(), "review_ready" | "draft_published") {
                        0
                    } else {
                        1
                    };
                let stop_reason = (final_status == "no_changes").then(|| {
                    "cook completed provider execution and gates, but finalization found no changed files; task likely still requires review or retry".to_string()
                });
                return Ok(cook_report(CookReportInput {
                    cook_id,
                    status: &final_status,
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: Some(finalization),
                    stop_reason,
                    exit_code,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::BaselineRed => {
                if options.gates.accept_inherited_failures && promotion.finalization_eligible(true)
                {
                    report_cook_progress(
                        durable_observer,
                        &cook_id,
                        &run_id,
                        "finalization",
                        attempt,
                        Some("accepted inherited baseline-red gate"),
                    )?;
                    let finalization = side_effects.finalize(&options, &run_id, &promotion)?;
                    let final_status = finalization["status"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string();
                    return Ok(cook_report(CookReportInput {
                        cook_id,
                        status: &final_status,
                        disposition: CookDisposition::Terminal,
                        attempts,
                        finalization: Some(finalization),
                        stop_reason: Some(
                            "finalized under explicit accepted inherited baseline-red gate policy"
                                .to_string(),
                        ),
                        exit_code: if matches!(
                            final_status.as_str(),
                            "review_ready" | "draft_published"
                        ) {
                            0
                        } else {
                            1
                        },
                        invocation_latest_run_id: Some(&run_id),
                    }));
                }
                return Ok(cook_report(CookReportInput {
                    cook_id,
                    status: "baseline_red",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "candidate and immutable baseline failed the same required gate; repair the inherited infrastructure or gate environment before rerunning Cook"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::NoChanges => {
                return Ok(cook_report(CookReportInput {
                    cook_id,
                    status: "no_changes",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "cook completed provider execution but produced no changed \
     files; task likely still requires review or retry"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::NoOpGateFailed => {
                return Ok(cook_report(CookReportInput {
                    cook_id,
                    status: "no_op_gate_failed",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "provider produced no patch and the pinned candidate failed \
     deterministic verification"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(follow_up_request) = follow_up_request else {
                    return Ok(cook_report(CookReportInput {
                        cook_id,
                        status: "policy_failure",
                        disposition: CookDisposition::Terminal,
                        attempts,
                        finalization: None,
                        stop_reason: Some(
                            "cook feedback requested retry without a follow-up request".to_string(),
                        ),
                        exit_code: 1,
                        invocation_latest_run_id: Some(&run_id),
                    }));
                };
                let budget_limit = budget_limit
                    .as_ref()
                    .expect("budget is initialized from the loaded attempt plan");
                let budget_scope = follow_up_budget_scope(&source_request, &follow_up_request);
                let persisted_authority = plan
                    .tasks
                    .first()
                    .map(|task| &task.inputs["cook_loop"]["execution_budget_authority"])
                    .filter(|authority| budget_authority_matches_scope(authority, budget_scope));
                let scoped_budget_limit = scoped_follow_up_budget(
                    budget_scope,
                    budget_limit,
                    budget_used,
                    persisted_authority,
                )
                .0;
                let review_form_only =
                    follow_up_request.inputs["cook_loop"]["review_form_required"] == true;
                match dispatch_cook_follow_up(
                    &options,
                    executor.clone(),
                    &cook_id,
                    attempt,
                    &run_id,
                    &plan,
                    &aggregate,
                    &promotion,
                    follow_up_request,
                    false,
                    budget_scope,
                    budget_limit,
                    budget_used,
                    &mut remediation_category_usage,
                )? {
                    CookFollowUpDispatch::Dispatched {
                        run_id: next_run_id,
                    } => run_id = next_run_id,
                    CookFollowUpDispatch::BudgetExhausted { reason } => {
                        return Ok(cook_report(CookReportInput {
                            cook_id,
                            status: "execution_budget_exhausted",
                            disposition: CookDisposition::Terminal,
                            attempts,
                            finalization: None,
                            stop_reason: Some(exhausted_budget_guidance(
                                attempt_limit,
                                &scoped_budget_limit,
                                &reason,
                                review_form_only,
                            )),
                            exit_code: 1,
                            invocation_latest_run_id: Some(&run_id),
                        }));
                    }
                    CookFollowUpDispatch::PolicyFailure { reason } => {
                        return Ok(cook_report(CookReportInput {
                            cook_id,
                            status: "policy_failure",
                            disposition: CookDisposition::Terminal,
                            attempts,
                            finalization: None,
                            stop_reason: Some(reason),
                            exit_code: 1,
                            invocation_latest_run_id: Some(&run_id),
                        }));
                    }
                }
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                return Ok(cook_report(CookReportInput {
                    cook_id,
                    status: "retries_exhausted",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
        }
    }

    Ok(cook_report(CookReportInput {
        cook_id,
        status: "retries_exhausted",
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some("cook attempt budget exhausted".to_string()),
        exit_code: 1,
        invocation_latest_run_id: Some(&run_id),
    }))
}

fn resumable_cook_run_id(
    recipe: &super::AgentTaskCookRecipe,
    cook_id: &str,
    initial_run_id: &str,
    requested_attempt: u32,
    verification_pending_continuation: bool,
) -> Option<String> {
    (!verification_pending_continuation)
        .then(|| agent_task_lifecycle::select_cook_candidate(cook_id).ok())
        .flatten()
        .map(|selection| selection.run_id)
        .filter(|run_id| run_id != initial_run_id)
        .filter(|run_id| {
            recipe
                .attempts
                .iter()
                .find(|attempt| attempt.run_id == *run_id)
                .is_some_and(|attempt| attempt.attempt >= requested_attempt)
        })
}

/// A multi-candidate Cook has one controller-owned destination. Reject ambiguous
/// plans before any provider preflight or scheduler execution can spend work.
fn validate_cook_candidate_group(plan: &AgentTaskPlan) -> Result<()> {
    if plan.tasks.len() <= 1 {
        return Ok(());
    }
    let group_key = plan.group_key.as_deref().or_else(|| {
        plan.tasks
            .first()
            .and_then(|task| task.group_key.as_deref())
    });
    let Some(group_key) = group_key else {
        return Err(Error::validation_invalid_argument(
            "group_key",
            "Cook candidates require one explicit shared group",
            None,
            None,
        ));
    };
    if plan
        .tasks
        .iter()
        .any(|task| task.group_key.as_deref() != Some(group_key))
    {
        return Err(Error::validation_invalid_argument(
            "group_key",
            "every Cook candidate must use the plan shared group",
            Some(group_key.to_string()),
            None,
        ));
    }
    Ok(())
}

/// Only Cook's authenticated baseline transition may replace a durable task
/// workspace identity. Preserve the predecessor as provenance; callers cannot
/// mint this attestation through an arbitrary provider request.
fn bind_dispatch_workspace_attestations(plan: &mut AgentTaskPlan) -> Result<()> {
    for task in &mut plan.tasks {
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "Cook dispatch requires a workspace root",
                Some(task.task_id.clone()),
                None,
            )
        })?;
        let prior = task.metadata.get("cook_workspace_identity").cloned();
        let identity =
            crate::agent_task_workspace_identity::attest_workspace(std::path::Path::new(root))?;
        task.metadata["cook_workspace_identity"] = identity;
        if let Some(prior) = prior {
            task.metadata["cook_workspace_identity_predecessor"] = prior;
        }
    }
    Ok(())
}

fn cook_attempt_needs_execution(run_id: &str) -> bool {
    agent_task_lifecycle::status(run_id)
        .map(|record| {
            (!matches!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Succeeded
                    | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                    | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
                    | agent_task_lifecycle::AgentTaskRunState::PartialFailure
                    | agent_task_lifecycle::AgentTaskRunState::Failed
                    | agent_task_lifecycle::AgentTaskRunState::Cancelled
            ) || retryable_pre_execution_failure(&record))
                && !record.lab_handoff.as_ref().is_some_and(|handoff| {
                    handoff.state == agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
                })
        })
        .unwrap_or(true)
}

/// Re-resolve the declared Cook target before a provider can run. Durable
/// recipes may outlive provider metadata, so the filesystem identity is checked
/// again on local, Lab, retry, and resume paths rather than trusting the plan.
fn validate_cook_workspace(options: &AgentTaskCookServiceOptions) -> Result<()> {
    let continuation = tracked_promotion_continuation(options)?;
    let direct_path = std::path::Path::new(&options.to_worktree);
    let target = if direct_path.is_dir() {
        direct_path.to_path_buf()
    } else if let Some(record) =
        homeboy_core::worktree::resolve_workspace_ref_if_present(&options.to_worktree)?
    {
        if record.state() != &homeboy_core::worktree::TaskWorktreeState::Active {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                "declared Cook task worktree is no longer active",
                Some(options.to_worktree.clone()),
                None,
            ));
        }
        PathBuf::from(record.path())
    } else {
        homeboy_core::worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
            &options.to_worktree,
            &homeboy_core::defaults::load_config(),
            continuation
                .as_ref()
                .map(|continuation| &continuation.baseline),
        )?
        .worktree
        .path
        .into()
    };
    homeboy_core::worktree_providers::validate_task_worktree_root(&target, &options.to_worktree)?;
    let source = options.source_worktree_path.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace",
            "Cook requires the provider workspace to be the declared task worktree",
            Some(options.to_worktree.clone()),
            Some(vec!["Create or select the task worktree through the configured workspace provider, then retry Cook.".to_string()]),
        )
    })?;
    let target = std::fs::canonicalize(&target).map_err(|error| {
        Error::internal_io(error.to_string(), Some(target.display().to_string()))
    })?;
    if let Some(continuation) = continuation {
        authenticate_tracked_promotion_continuation(&target, &continuation)?;
    }
    let source = std::fs::canonicalize(source).map_err(|error| {
        Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    if source != target {
        return Err(Error::validation_invalid_argument(
            "workspace",
            "Cook provider workspace differs from its declared task worktree; refusing provider execution",
            Some(options.to_worktree.clone()),
            Some(vec!["Re-run Cook without a source CWD override so Homeboy binds the declared task worktree.".to_string()]),
        ));
    }
    Ok(())
}

/// The normal configured-provider preflight rejects every dirty destination. A
/// terminal review-form retry can retain its candidate only after its copied
/// promotion lineage and the resolved destination prove an exact match.
fn authenticated_historical_review_form_workspace(
    options: &AgentTaskCookServiceOptions,
) -> Result<bool> {
    let mut trace = ContinuationAdmissionTrace::new();
    if !agent_task_lifecycle::run_record_exists(&options.initial_run_id)? {
        trace.deny("run_record", "unavailable");
        return Ok(false);
    }
    trace.pass("run_record");
    let record = agent_task_lifecycle::status(&options.initial_run_id)?;
    match terminal_review_form_continuation_is_eligible(&options.initial_plan, &record) {
        Ok(true) => trace.pass("terminal_review_form_eligibility"),
        Ok(false) => {
            trace.deny("terminal_review_form_eligibility", "fail");
            record_continuation_admission_trace(&options.initial_run_id, &trace)?;
            return Ok(false);
        }
        Err(_) => {
            trace.deny("terminal_review_form_eligibility", "unavailable");
            record_continuation_admission_trace(&options.initial_run_id, &trace)?;
            return Ok(false);
        }
    }
    let Some(promotion) = persisted_promotion_for_attempt(&options.initial_run_id).unwrap_or(None)
    else {
        trace.deny("applied_promotion", "unavailable");
        record_continuation_admission_trace(&options.initial_run_id, &trace)?;
        return Ok(false);
    };
    if promotion.status != AgentTaskPromotionStatus::Applied {
        trace.deny("applied_promotion", "fail");
        record_continuation_admission_trace(&options.initial_run_id, &trace)?;
        return Ok(false);
    }
    trace.pass("applied_promotion");
    let continuation = match tracked_promotion_continuation(options) {
        Ok(Some(continuation)) => continuation,
        Ok(None) => {
            trace.deny("continuation_evidence", "unavailable");
            record_continuation_admission_trace(&options.initial_run_id, &trace)?;
            return Ok(false);
        }
        Err(_) => {
            trace.deny("continuation_evidence", "fail");
            record_continuation_admission_trace(&options.initial_run_id, &trace)?;
            return Ok(false);
        }
    };
    trace.pass("continuation_evidence");
    let resolution =
        match homeboy_core::worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
            &options.to_worktree,
            &homeboy_core::defaults::load_config(),
            Some(&continuation.baseline),
        ) {
            Ok(resolution) => resolution,
            Err(error) => {
                let predicate = if error.details.pointer("/workspace/classification")
                    == Some(&Value::String("workspace.resolved_but_dirty".to_string()))
                {
                    "provider_baseline_verification"
                } else {
                    "provider_resolution"
                };
                trace.deny(predicate, "fail");
                record_continuation_admission_trace(&options.initial_run_id, &trace)?;
                return Ok(false);
            }
        };
    trace.pass("provider_resolution");
    trace.pass("provider_baseline_verification");
    if resolution.worktree.handle != options.to_worktree
        || homeboy_core::worktree_providers::validate_task_worktree_root(
            std::path::Path::new(&resolution.worktree.path),
            &options.to_worktree,
        )
        .is_err()
    {
        trace.deny("worktree_root", "fail");
        record_continuation_admission_trace(&options.initial_run_id, &trace)?;
        return Ok(false);
    }
    trace.pass("worktree_root");
    let Ok(target) = std::fs::canonicalize(&resolution.worktree.path) else {
        trace.deny("candidate_fingerprint", "unavailable");
        record_continuation_admission_trace(&options.initial_run_id, &trace)?;
        return Ok(false);
    };
    if authenticate_tracked_promotion_continuation(&target, &continuation).is_err() {
        trace.deny("candidate_fingerprint", "fail");
        record_continuation_admission_trace(&options.initial_run_id, &trace)?;
        return Ok(false);
    }
    trace.pass("candidate_fingerprint");
    record_continuation_admission_trace(&options.initial_run_id, &trace)?;
    Ok(true)
}

fn record_continuation_admission_trace(
    run_id: &str,
    trace: &ContinuationAdmissionTrace,
) -> Result<()> {
    agent_task_lifecycle::record_metadata_value(
        run_id,
        "cook_continuation_admission",
        trace.value(),
    )
}

struct TrackedPromotionContinuation {
    baseline: Value,
    path: PathBuf,
    branch: String,
    candidate: crate::agent_task_promotion::AgentTaskPromotionCandidate,
}

/// A dirty destination is reusable only for the exact post-apply candidate
/// checkpoint owned by this Cook attempt. Core verifies the supplied baseline
/// during provider resolution; Cook binds it to this attempt's target identity.
fn tracked_promotion_continuation(
    options: &AgentTaskCookServiceOptions,
) -> Result<Option<TrackedPromotionContinuation>> {
    if !agent_task_lifecycle::run_record_exists(&options.initial_run_id)? {
        return Ok(None);
    }
    let Some(promotion) = persisted_promotion_for_attempt(&options.initial_run_id)? else {
        return Ok(None);
    };
    let has_post_apply_checkpoint = ["/post_apply", "/resumed_post_apply_promotion"]
        .into_iter()
        .any(|pointer| promotion.provenance.pointer(pointer) == Some(&Value::Bool(true)));
    let authenticated_legacy_review =
        if promotion.status == AgentTaskPromotionStatus::Applied && !has_post_apply_checkpoint {
            let record = agent_task_lifecycle::status(&options.initial_run_id)?;
            terminal_review_form_continuation_is_eligible(&options.initial_plan, &record)?
        } else {
            false
        };
    let normal_continuation = matches!(
        promotion.status,
        AgentTaskPromotionStatus::VerificationPending | AgentTaskPromotionStatus::Applied
    ) && (has_post_apply_checkpoint || authenticated_legacy_review);
    // A gate-failed candidate remains eligible only when this exact lifecycle
    // route selected its exact Cook attempt. Plan metadata is caller-controlled
    // and is therefore never authorization for dirty-worktree reuse.
    let route_authorized_gate_failure = promotion.status.gate_failed()
        && has_post_apply_checkpoint
        && has_cook_continue_route(options);
    if !(normal_continuation || route_authorized_gate_failure) {
        return Ok(None);
    }
    if promotion.to_worktree != options.to_worktree
        || promotion.target.worktree != options.to_worktree
    {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination does not match its tracked post-apply promotion",
            Some(options.to_worktree.clone()),
            None,
        ));
    }
    let path = promotion.target.path.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion.target.path",
            "Cook continuation requires the tracked post-apply promotion destination path",
            Some(options.initial_run_id.clone()),
            None,
        )
    })?;
    let branch = promotion
        .target
        .branch
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "latest_promotion.target.branch",
                "Cook continuation requires the tracked post-apply promotion destination branch",
                Some(options.initial_run_id.clone()),
                None,
            )
        })?;
    let candidate = promotion
        .provenance
        .get("candidate")
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "latest_promotion.provenance.candidate",
                "Cook continuation requires the tracked post-apply candidate fingerprint",
                Some(options.initial_run_id.clone()),
                None,
            )
        })?;
    let candidate = serde_json::from_value(candidate).map_err(|_| {
        Error::validation_invalid_argument(
            "latest_promotion.provenance.candidate",
            "Cook continuation tracked candidate fingerprint is invalid",
            Some(options.initial_run_id.clone()),
            None,
        )
    })?;
    let mut baseline = promotion
        .provenance
        .get("gate_feedback_baseline")
        .filter(|baseline| {
            baseline.get("schema").and_then(Value::as_str)
                == Some("homeboy/agent-task-gate-feedback-baseline/v1")
        })
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "latest_promotion.provenance.gate_feedback_baseline",
                "Cook continuation requires the tracked post-apply candidate baseline",
                Some(options.initial_run_id.clone()),
                None,
            )
        })?;
    baseline["patch_artifact"] =
        serde_json::to_value(&promotion.patch_artifact).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize persisted promotion artifact baseline".to_string()),
            )
        })?;
    // Provider preflight runs before this function can authenticate the resolved
    // target below. Carry the complete immutable continuation claim through its
    // baseline verifier so a dirty destination is never admitted provisionally.
    baseline["tracked_promotion"] = serde_json::json!({
        "target_path": path,
        "branch": branch,
        "candidate": candidate,
        "changed_files": promotion.changed_files,
    });
    Ok(Some(TrackedPromotionContinuation {
        baseline,
        path: PathBuf::from(path),
        branch,
        candidate,
    }))
}

fn authenticate_tracked_promotion_continuation(
    target: &std::path::Path,
    continuation: &TrackedPromotionContinuation,
) -> Result<()> {
    let expected_path = std::fs::canonicalize(&continuation.path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(continuation.path.display().to_string()),
        )
    })?;
    if target != expected_path {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination path differs from its tracked post-apply promotion",
            Some(target.display().to_string()),
            None,
        ));
    }
    let branch = homeboy_core::git::current_branch(target).ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination has no branch",
            Some(target.display().to_string()),
            None,
        )
    })?;
    if branch != continuation.branch {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination branch differs from its tracked post-apply promotion",
            Some(target.display().to_string()),
            None,
        ));
    }
    let actual =
        crate::agent_task_promotion::candidate_fingerprint(target.to_string_lossy().as_ref())?;
    if actual != continuation.candidate {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination differs from its exact tracked post-apply candidate",
            Some(target.display().to_string()),
            None,
        ));
    }
    Ok(())
}

fn record_active_cook_worktree_warning(options: &AgentTaskCookServiceOptions) -> Result<()> {
    let Some(source) = options.source_worktree_path.as_deref() else {
        return Ok(());
    };
    let target = std::fs::canonicalize(source).map_err(|error| {
        Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    let mut active = agent_task_lifecycle::list_records()?
        .into_iter()
        .filter(|record| record.run_id != options.initial_run_id && !record.state.is_terminal())
        .filter(|record| record.metadata.get("cook_id").is_some())
        .filter_map(|record| {
            let plan = agent_task_lifecycle::load_plan_for_execution(&record.run_id).ok()?;
            let matches_target = plan.tasks.iter().any(|task| {
                task.workspace
                    .root
                    .as_deref()
                    .and_then(|root| std::fs::canonicalize(root).ok())
                    .as_ref()
                    == Some(&target)
            });
            matches_target.then_some(record)
        })
        .collect::<Vec<_>>();
    active.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    if active.is_empty() {
        return Ok(());
    }
    let run_ids = active
        .iter()
        .map(|record| record.run_id.clone())
        .collect::<Vec<_>>();
    agent_task_lifecycle::record_metadata_value(
        &options.initial_run_id,
        "cook_active_worktree_warning",
        serde_json::json!({
            "schema": "homeboy/cook-active-worktree-warning/v1",
            "canonical_worktree": target,
            "active_run_ids": run_ids,
            "status_commands": active.iter().map(|record| format!("homeboy agent-task status {}", record.run_id)).collect::<Vec<_>>(),
        }),
    )?;
    Ok(())
}

/// A baseline is immutable provider evidence. Continue provider and controller
/// work from the active task-worktree named by the recipe, never from that
/// temporary baseline path.
fn rebind_baseline_continuation_workspace(
    options: &mut AgentTaskCookServiceOptions,
    continuation_plan: &AgentTaskPlan,
) -> Result<()> {
    let baseline_bound_continuation = continuation_plan.tasks.first().is_some_and(|task| {
        task.inputs
            .pointer("/cook_loop/artifact_provenance/source_run_id")
            .is_some()
            || task
                .metadata
                .get("cook_initial_candidate_baseline")
                .is_some()
    });
    if !baseline_bound_continuation {
        return Ok(());
    }
    if std::path::Path::new(&options.to_worktree).is_dir() {
        options.source_worktree_path = Some(options.to_worktree.clone().into());
    } else if let Some(worktree) =
        homeboy_core::worktree::resolve_workspace_ref_if_present(&options.to_worktree)?
    {
        if worktree.state() == &homeboy_core::worktree::TaskWorktreeState::Active {
            options.source_worktree_path = Some(worktree.path().into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "cook_tests.rs"]
mod tests;

/// Wall-clock budget behaviour at the boundaries the Cook loop checks.
///
/// These cover the decision and the contract it produces. The loop plumbing
/// that calls them is exercised by the integration cases in `cook_tests.rs`.
#[cfg(test)]
mod cook_deadline_tests {
    use super::*;
    use crate::agent_task_timeout::{
        current_cook_deadline, now_unix_ms, with_current_cook_deadline,
    };

    fn expired() -> CookDeadline {
        CookDeadline::from_unix_ms(now_unix_ms().saturating_sub(1))
    }

    /// The default has to remain "no budget": every caller that never set one
    /// must reach its attempt boundary exactly as before.
    #[test]
    fn an_unbudgeted_cook_never_reports_an_expired_boundary() {
        assert_eq!(current_cook_deadline(), None);
        assert_eq!(expired_cook_deadline(), None);
    }

    /// A live budget must not stop a Cook that still has time. Firing early is
    /// the failure mode that kills real work.
    #[test]
    fn a_live_budget_does_not_trip_a_boundary() {
        with_current_cook_deadline(Some(CookDeadline::from_duration_seconds(3_600)), || {
            assert_eq!(expired_cook_deadline(), None);
        });
    }

    #[test]
    fn an_expired_budget_trips_the_boundary() {
        let deadline = expired();
        with_current_cook_deadline(Some(deadline), || {
            assert_eq!(expired_cook_deadline(), Some(deadline));
        });
    }

    /// Expiry must terminalize cleanly and inspectably: a known status, a
    /// declared terminal disposition, a non-zero exit, and a stop reason that
    /// names the boundary and the remedy.
    #[test]
    fn deadline_expiry_at_an_attempt_boundary_terminalizes_as_timed_out() {
        let deadline = expired();
        let result = cook_deadline_report(
            "cook-1".to_string(),
            Vec::new(),
            "run-1",
            &deadline,
            "attempt 2 could be started",
        );

        assert_eq!(result.exit_code, 1, "expiry is not a success");
        assert_eq!(result.value.status, "timed_out");
        assert_eq!(result.value.disposition, CookDisposition::Terminal);
        assert_eq!(
            result.value.terminal_phase.as_deref(),
            Some("cook_deadline"),
            "the phase must distinguish a budget stop from any other timeout"
        );
        assert_eq!(
            result.value.terminal_failure_classification.as_deref(),
            Some("timed_out")
        );

        let reason = result.value.stop_reason.clone().expect("a stop reason");
        assert!(reason.contains("--max-duration"), "{reason}");
        assert!(reason.contains("attempt 2 could be started"), "{reason}");
    }

    /// The status must classify the same way every other terminal Cook status
    /// does, or batch totals and exit codes disagree with the report.
    ///
    /// Also pins the literal written by `cook_deadline_report` to the enum
    /// entry it is meant to be, so the two cannot drift apart.
    #[test]
    fn the_reported_status_reads_as_terminal_and_not_as_success() {
        assert_eq!(CookStatus::TimedOut.as_str(), "timed_out");
        let status = CookStatus::from_status("timed_out");
        assert_eq!(status, CookStatus::TimedOut);
        assert!(!status.is_in_flight());
        assert!(!status.is_success_exit());
    }

    /// A gate-boundary stop names the gates, because "out of time" and "out of
    /// time before the gates could run" call for different follow-ups.
    #[test]
    fn a_gate_boundary_stop_names_the_gate_phase() {
        let reason = cook_deadline_report(
            "cook-1".to_string(),
            Vec::new(),
            "run-1",
            &expired(),
            "attempt 1 could enter promotion and run its gates",
        )
        .value
        .stop_reason
        .expect("a stop reason");
        assert!(reason.contains("run its gates"), "{reason}");
    }
}
