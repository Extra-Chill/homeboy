//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::agent_task_dispatch_plan::build_dispatch_plan;
use crate::agent_task_dispatch_service::{self, AgentTaskDispatchCommand};
use crate::agent_task_gate::VerifyGateOptions;
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{AgentTaskPromotionReport, AgentTaskPromotionStatus};
use crate::agent_task_scheduler::{
    AgentTaskExecutionBudget, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::{Error, Result};

use super::cook_baseline::{
    cook_attempt_harvest_context, materialize_follow_up_baseline,
    materialize_initial_candidate_baseline, CookFollowUpBaseline, DerivedCookBaselineCapability,
};
use super::cook_budget::{
    budget_remaining, execution_budget_usage, reserve_remediation_budget, ExecutionBudgetUsage,
};
use super::cook_pre_execution::{
    materialize_initial_cook_attempt, pre_execution_failure_details, pre_execution_failure_phase,
    pre_execution_failure_report, record_pre_execution_failure, retryable_pre_execution_failure,
    terminal_executor_matches, with_pre_execution_phase,
};
use super::cook_promotion::{
    attempt_needs_execution, cook_report, finalize_or_load_cook_pr,
    is_moving_base_finalization_error, moving_base_recovery_for_run,
    moving_base_recovery_from_promotion, moving_base_recovery_report, next_moving_base_recovery,
    persisted_promotion_for_attempt, promote_or_load_attempt, recover_moving_base_cook_candidate,
    refreshed_moving_base_recovery, MovingBaseCookRecovery,
};
use super::execution::run_loaded_plan_with_derived_cook_baseline;
use super::AgentTaskRunResult;

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
    let Some(outcome) = aggregate.outcomes.last() else {
        return Ok(None);
    };
    crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(&outcome.outputs)
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

/// Provenance supplied when Homeboy adopts a candidate prepared outside its
/// provider lifecycle.
#[derive(Debug, Clone, Default)]
pub struct AgentTaskCandidateAdoptionOptions {
    pub ai_model: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookReport {
    pub schema: &'static str,
    pub cook_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_run_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocation_run_ids: Vec<String>,
    pub status: String,
    pub attempts: Vec<AgentTaskCookAttemptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalization: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Preserves the lifecycle-owned failure boundary when cook stops before
    /// provider dispatch instead of collapsing it into an attempt-budget result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_failure_classification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moving_base_recovery: Option<MovingBaseCookRecovery>,
}

/// A bounded collection of independently durable cooks. Each cook retains the
/// same dispatch, retry, promotion, and lifecycle path as `run_cook`.
#[derive(Debug, Clone)]
pub struct AgentTaskCookBatchOptions {
    pub batch_id: String,
    pub cooks: Vec<AgentTaskCookServiceOptions>,
    pub max_concurrency: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookBatchCellReport {
    pub cook_id: String,
    pub initial_run_id: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AgentTaskCookReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookBatchReport {
    pub schema: &'static str,
    pub batch_id: String,
    pub status: String,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cooks: Vec<AgentTaskCookBatchCellReport>,
}

/// Resolves a generic dispatch command once, before a typed cook is scheduled.
/// Callers compile workflow policy into the command and cook options; this
/// routine owns the shared dispatch compilation boundary.
pub fn compile_cook_attempt(
    mut options: AgentTaskCookServiceOptions,
    dispatch: AgentTaskDispatchCommand,
) -> Result<AgentTaskCookServiceOptions> {
    let request = agent_task_dispatch_service::resolve_dispatch_request(dispatch.into())?;
    options.initial_plan = build_dispatch_plan(&request)?;
    Ok(options)
}

/// Runs independently durable cooks with bounded concurrency while preserving
/// input order for callers that join their own metadata onto the results.
/// Batch-cook fanout is the first caller; other cook coordinators can migrate
/// by compiling their own `AgentTaskCookServiceOptions` and using this runner.
pub fn run_cook_batch<E>(
    options: AgentTaskCookBatchOptions,
    executor: E,
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
    let cooks = Arc::new(options.cooks);
    let next = Arc::new(Mutex::new(0usize));
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let cooks = Arc::clone(&cooks);
            let next = Arc::clone(&next);
            let tx = tx.clone();
            let executor = executor.clone();
            scope.spawn(move || loop {
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
                let cell = match run_cook(cook.clone(), executor.clone()) {
                    Ok(result) => AgentTaskCookBatchCellReport {
                        cook_id: cook.cook_id,
                        initial_run_id: cook.initial_run_id,
                        exit_code: result.exit_code,
                        result: Some(result.value),
                        error: None,
                    },
                    Err(error) => AgentTaskCookBatchCellReport {
                        cook_id: cook.cook_id,
                        initial_run_id: cook.initial_run_id,
                        exit_code: 1,
                        result: None,
                        error: Some(error.to_string()),
                    },
                };
                let _ = tx.send((index, cell));
            });
        }
    });
    drop(tx);

    let mut cells = (0..total).map(|_| None).collect::<Vec<_>>();
    for (index, cell) in rx {
        cells[index] = Some(cell);
    }
    let cooks = cells.into_iter().flatten().collect::<Vec<_>>();
    let failed = cooks.iter().filter(|cell| cell.exit_code != 0).count();
    Ok(AgentTaskRunResult {
        exit_code: if failed == 0 { 0 } else { 1 },
        value: AgentTaskCookBatchReport {
            schema: "homeboy/agent-task-cook-batch/v1",
            batch_id: options.batch_id,
            status: if failed == 0 {
                "succeeded".to_string()
            } else {
                "failed".to_string()
            },
            total,
            succeeded: total - failed,
            failed,
            cooks,
        },
    })
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
    let batch = crate::agent_task_batch::read_batch_record(batch_id)?;
    if batch.child_runs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "batch_id",
            format!("agent-task fanout batch `{batch_id}` has no child runs to resume"),
            Some(batch_id.to_string()),
            None,
        ));
    }

    let total = batch.child_runs.len();
    let mut cells = Vec::with_capacity(total);
    for child in &batch.child_runs {
        // The persisted batch child `run_id` is the cook id (`cook-<id>`), which
        // is exactly the durable recipe key. Reconstruct from that recipe so the
        // resumed cook re-runs its own gates and finalization contract.
        let cook_id = child.run_id.clone();
        let cell = match resume_batch_child(&cook_id, executor.clone(), &reconstruct_dispatcher) {
            Ok(report) => {
                let exit_code = cook_report_exit_code(&report);
                AgentTaskCookBatchCellReport {
                    cook_id: report.cook_id.clone(),
                    initial_run_id: cook_id,
                    exit_code,
                    result: Some(report),
                    error: None,
                }
            }
            Err(error) => AgentTaskCookBatchCellReport {
                cook_id: child.task_id.clone(),
                initial_run_id: cook_id,
                exit_code: 1,
                result: None,
                error: Some(error.to_string()),
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

    let failed = cells.iter().filter(|cell| cell.exit_code != 0).count();
    Ok(AgentTaskRunResult {
        exit_code: if failed == 0 { 0 } else { 1 },
        value: AgentTaskCookBatchReport {
            schema: "homeboy/agent-task-cook-batch/v1",
            batch_id: batch.batch_id,
            status: if failed == 0 {
                "succeeded".to_string()
            } else {
                "failed".to_string()
            },
            total,
            succeeded: total - failed,
            failed,
            cooks: cells,
        },
    })
}

/// Reconstruct one batch child's cook from its durable recipe and re-run it.
/// A missing recipe means the child never reached cook start; surface an
/// actionable resumability error instead of fabricating a cook.
fn resume_batch_child<E, D>(
    cook_id: &str,
    executor: E,
    reconstruct_dispatcher: &D,
) -> Result<AgentTaskCookReport>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: Fn(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
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
    agent_task_lifecycle::reconcile_terminal_artifact_projection(cook_id)?;
    if let Some(reason) = agent_task_lifecycle::terminal_artifact_projection_readiness(cook_id)? {
        return Err(Error::validation_invalid_argument(
            "cook_id",
            format!("cook `{cook_id}` cannot resume until controller-side patch projection is ready: {reason}"),
            Some(cook_id.to_string()),
            Some(vec![format!(
                "Run `homeboy agent-task status {cook_id}` to reconcile the controller projection."
            )]),
        ));
    }
    let recipe = super::load_recipe(cook_id)?;
    // Faithfully reconstruct the recipe's transport so re-running `run_cook`
    // matches the persisted durable inputs (a stripped dispatcher would look
    // like a conflicting new cook). A terminal child is not re-dispatched — its
    // `needs_execution` check is false — so the reconstructed transport is only
    // used to satisfy the recipe contract, never to spend a provider attempt
    // (#9525).
    let attempt_dispatcher =
        reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
    let options = super::reconstruct_options_with_dispatcher(&recipe, attempt_dispatcher)?;
    Ok(run_cook(options, executor)?.value)
}

fn child_finalization_value(cell: &AgentTaskCookBatchCellReport) -> Value {
    serde_json::json!({
        "resumed_at": chrono::Utc::now().to_rfc3339(),
        "exit_code": cell.exit_code,
        "status": cell
            .result
            .as_ref()
            .map(|report| report.status.clone())
            .unwrap_or_else(|| "error".to_string()),
        "error": cell.error,
    })
}

fn cook_report_exit_code(report: &AgentTaskCookReport) -> i32 {
    // A review-ready or already-finalized cook is a success; anything the cook
    // could not carry to a green, finalized state is a non-zero resume result
    // the operator must still act on.
    match report.status.as_str() {
        "review_ready" | "green_no_finalize" => 0,
        _ => {
            if report
                .finalization
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                == Some("review_ready")
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

/// Append and dispatch one remediation attempt from an authenticated promoted
/// candidate. Both ordinary Cook feedback and external candidate adoption use
/// this boundary so their budget, provenance, and baseline authority match.
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
    budget_limit: &AgentTaskExecutionBudget,
    budget_used: ExecutionBudgetUsage,
    remediation_category_usage: &mut ExecutionBudgetUsage,
) -> Result<CookFollowUpDispatch>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let Some(remaining_budget) = budget_remaining(budget_limit, budget_used) else {
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
    let reservation = match reserve_remediation_budget(&remaining_budget, same_provider) {
        Ok(reservation) => reservation,
        Err(reason) => {
            return Ok(CookFollowUpDispatch::BudgetExhausted {
                reason: reason.to_string(),
            })
        }
    };
    let next_attempt = attempt + 1;
    let next_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, next_attempt);
    // This is reviewable lineage, not the process-local baseline capability.
    follow_up_request.inputs["cook_loop"]["artifact_provenance"] = serde_json::json!({
        "source_run_id": source_run_id,
        "source_task_id": promotion.source.task_id,
        "source_patch_artifact_sha256": promotion.patch_artifact.sha256,
    });
    let mut follow_up_plan = AgentTaskPlan::new(
        format!("{cook_id}-cook-attempt-{next_attempt}"),
        vec![follow_up_request],
    );
    follow_up_plan.options = plan.options.clone();
    follow_up_plan.options.execution_budget = AgentTaskExecutionBudget::new(1, 0, 0);
    follow_up_plan.options.retry.max_attempts = 1;
    let review_form_only =
        follow_up_plan.tasks[0].inputs["cook_loop"]["review_form_required"] == true;
    super::record_recipe_attempt(cook_id, next_attempt, &next_run_id, &follow_up_plan)?;
    if attempt_needs_execution(&next_run_id) {
        let baseline = materialize_follow_up_baseline(
            promotion,
            source_run_id,
            &follow_up_plan.tasks[0].task_id,
        )?;
        follow_up_plan.tasks[0].workspace.root = Some(baseline.path.display().to_string());
        follow_up_plan.tasks[0].inputs["cook_loop"]["artifact_provenance"] =
            baseline.artifact_provenance();
        if let Some(dispatcher) = &options.attempt_dispatcher {
            // A detached dispatcher may return before any executor-side
            // lifecycle write. Persist the exact materialized plan first so a
            // continuation resumes this baseline-bound workspace contract.
            agent_task_lifecycle::submit_plan(&follow_up_plan, Some(&next_run_id))?;
            dispatcher.dispatch_attempt(
                follow_up_plan,
                &next_run_id,
                Some(baseline.capability()),
            )?;
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
    agent_task_lifecycle::record_cook_attempt(cook_id, next_attempt, &next_run_id)?;
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

pub fn run_cook<E>(
    options: AgentTaskCookServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_cook_with_finalizer(options, executor, finalize_or_load_cook_pr)
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
    run_cook_with_boundaries(
        options,
        executor,
        finalize,
        recover_moving_base_cook_candidate,
    )
}

fn run_cook_with_boundaries<E, F, R>(
    options: AgentTaskCookServiceOptions,
    executor: E,
    mut finalize: F,
    mut recover: R,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    F: FnMut(&AgentTaskCookServiceOptions, &str, &AgentTaskPromotionReport) -> Result<Value>,
    R: FnMut(
        &AgentTaskCookServiceOptions,
        &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport>,
{
    // A configured provider is controller authority. Resolve it before an
    // external runner can spend a provider attempt; explicit transports are
    // caller-owned overrides and retain their existing behavior. A typed
    // moving-base continuation has already completed provider work and must
    // not require a provider merely to rebase and reverify its candidate.
    let moving_base_continuation = agent_task_lifecycle::status(&options.initial_run_id)
        .ok()
        .and_then(|record| record.metadata.get("cook_moving_base_recovery").cloned())
        .is_some();
    if !moving_base_continuation
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
    let recipe = super::persist_initial_recipe(&options)?;
    // A recipe can survive an interruption before its first lifecycle record.
    // Resume from the validated durable inputs so ambient transport state cannot
    // turn replay into a conflicting new cook.
    let requested_run_id = options.initial_run_id.clone();
    let mut options = if existing_recipe {
        let mut reconstructed = if adopted_model.is_some() {
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
    materialize_initial_cook_attempt(&options)?;
    // Transport readiness can serialize on a reconnect/runtime-promotion
    // lease. Complete it before entering the provider-attempt loop so that
    // waiting for a shared Lab session never consumes a cook attempt.
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
    let _runtime_generation =
        homeboy_core::runtime_promotion::pin_cook_generation(&options.cook_id)?;
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
    let resumed_run_id = agent_task_lifecycle::cook_index(&options.cook_id)
        .ok()
        .map(|index| index.latest_run_id)
        .filter(|run_id| run_id != &options.initial_run_id)
        .filter(|run_id| {
            recipe
                .attempts
                .iter()
                .find(|attempt| attempt.run_id == *run_id)
                .is_some_and(|attempt| attempt.attempt >= requested_attempt)
        });
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
    for attempt in first_attempt..=max_attempts {
        let plan = match next_plan.take() {
            Some(plan) => plan,
            None => agent_task_lifecycle::load_plan(&run_id)?,
        };
        let needs_execution = agent_task_lifecycle::status(&run_id)
            .map(|record| {
                (!matches!(
                    record.state,
                    agent_task_lifecycle::AgentTaskRunState::Succeeded
                        | agent_task_lifecycle::AgentTaskRunState::PartialFailure
                        | agent_task_lifecycle::AgentTaskRunState::Failed
                        | agent_task_lifecycle::AgentTaskRunState::Cancelled
                ) || retryable_pre_execution_failure(&record))
                    && !record.lab_handoff.as_ref().is_some_and(|handoff| {
                        handoff.state == agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
                    })
            })
            .unwrap_or(true);
        if needs_execution {
            // Claim the durable attempt before candidate baseline staging. That
            // staging can take longer than the foreground controller's timeout;
            // a restarted controller must find the same immutable plan rather
            // than create an ownerless Lab admission.
            if !agent_task_lifecycle::run_record_exists(&run_id)? {
                agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
            }
            let execution = (|| {
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
                let mut dispatch_plan = plan.clone();
                if let Some(baseline) = initial_baseline.as_ref() {
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
                if let Some(dispatcher) = &options.attempt_dispatcher {
                    dispatcher.dispatch_attempt(
                        dispatch_plan,
                        &run_id,
                        initial_baseline
                            .as_ref()
                            .map(CookFollowUpBaseline::capability),
                    )
                } else {
                    run_loaded_plan_with_derived_cook_baseline(
                        dispatch_plan,
                        Some(&run_id),
                        executor.clone(),
                        initial_baseline
                            .as_ref()
                            .map(CookFollowUpBaseline::capability),
                        Some(cook_attempt_harvest_context(&options.harvest_context)),
                    )
                    .map(|_| ())
                }
            })();
            if let Err(error) = execution {
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
                    return Ok(cook_report(
                        cook_id,
                        "retries_exhausted",
                        attempts,
                        None,
                        Some(error.to_string()),
                        1,
                        Some(&run_id),
                    ));
                }
                let next_attempt = attempt + 1;
                let next_run_id = agent_task_lifecycle::cook_attempt_run_id(&cook_id, next_attempt);
                super::record_recipe_attempt(&cook_id, next_attempt, &next_run_id, &plan)?;
                run_id = next_run_id;
                next_plan = Some(plan);
                continue;
            }
        }
        agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
        let record = agent_task_lifecycle::status(&run_id)?;
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
            return Ok(cook_report(
                cook_id,
                "in_flight",
                attempts,
                None,
                Some("provider attempt accepted by the runner daemon".to_string()),
                0,
                Some(&run_id),
            ));
        }
        let plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
        budget_limit.get_or_insert_with(|| plan.options.execution_budget.clone());
        let aggregate = agent_task_lifecycle::read_aggregate(&run_id)?;
        observed_budget_used.add(execution_budget_usage(&aggregate));
        let mut budget_used = observed_budget_used;
        budget_used.same_provider_retries = budget_used
            .same_provider_retries
            .saturating_add(remediation_category_usage.same_provider_retries);
        budget_used.provider_rotations = budget_used
            .provider_rotations
            .saturating_add(remediation_category_usage.provider_rotations);
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(cook_report(
                cook_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task cook requires a plan with one source task".to_string()),
                1,
                Some(&run_id),
            ));
        };
        if plan.tasks.len() != 1 {
            return Ok(cook_report(
                cook_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task cook currently supports one task per cook attempt".to_string()),
                1,
                Some(&run_id),
            ));
        }

        let adopted_continuation = adopted_attempt_is_ready_for_cook_continuation(&record)?;
        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
                | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
        ) && adopted_continuation.is_none()
        {
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(cook_report(
                cook_id,
                "provider_failure",
                attempts,
                None,
                Some(format!(
                    "agent-task run {run_id} ended in state {:?}",
                    record.state
                )),
                1,
                Some(&run_id),
            ));
        }

        let promotion = match promote_or_load_attempt(&options, &run_id) {
            Ok(report) => report,
            Err(error) => {
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                return Ok(cook_report(
                    cook_id,
                    "policy_failure",
                    attempts,
                    None,
                    Some(error.to_string()),
                    1,
                    Some(&run_id),
                ));
            }
        };

        let review_form = review_form_from_aggregate(&aggregate)?;
        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt,
            max_attempts,
            source_run_id: Some(run_id.clone()),
            current_diff: gate_feedback_current_diff(&promotion),
            require_review_form: true,
            review_form,
            metadata: Value::Null,
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
            AgentTaskCookLoopStatus::GreenCompleted => {
                if options.no_finalize {
                    return Ok(cook_report(
                        cook_id,
                        "green_no_finalize",
                        attempts,
                        None,
                        Some(
                            "deterministic gates completed green; --no-finalize skipped commit, push, and PR finalization"
                                .to_string(),
                        ),
                        0,
                        Some(&run_id),
                    ));
                }
                let mut active_moving_base_recovery = None;
                let promotion = match moving_base_recovery_for_run(&run_id)? {
                    Some(recovery) => match recover(&options, &recovery) {
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
                let finalization = match finalize(&options, &run_id, &promotion) {
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
                    Err(error) => return Err(error),
                };
                let final_status = finalization["status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let exit_code = if final_status == "review_ready" { 0 } else { 1 };
                let stop_reason = (final_status == "no_changes").then(|| {
                    "cook completed provider execution and gates, but finalization found no changed files; task likely still requires review or retry".to_string()
                });
                return Ok(cook_report(
                    cook_id,
                    &final_status,
                    attempts,
                    Some(finalization),
                    stop_reason,
                    exit_code,
                    Some(&run_id),
                ));
            }
            AgentTaskCookLoopStatus::NoChanges => {
                return Ok(cook_report(
                    cook_id,
                    "no_changes",
                    attempts,
                    None,
                    Some(
                        "cook completed provider execution but produced no changed files; task likely still requires review or retry"
                            .to_string(),
                    ),
                    1,
                    Some(&run_id),
                ));
            }
            AgentTaskCookLoopStatus::NoOpGateFailed => {
                return Ok(cook_report(
                    cook_id,
                    "no_op_gate_failed",
                    attempts,
                    None,
                    Some(
                        "provider produced no patch and the pinned candidate failed deterministic verification"
                            .to_string(),
                    ),
                    1,
                    Some(&run_id),
                ));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(follow_up_request) = follow_up_request else {
                    return Ok(cook_report(
                        cook_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cook feedback requested retry without a follow-up request".to_string(),
                        ),
                        1,
                        Some(&run_id),
                    ));
                };
                let budget_limit = budget_limit
                    .as_ref()
                    .expect("budget is initialized from the loaded attempt plan");
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
                    budget_limit,
                    budget_used,
                    &mut remediation_category_usage,
                )? {
                    CookFollowUpDispatch::Dispatched {
                        run_id: next_run_id,
                    } => run_id = next_run_id,
                    CookFollowUpDispatch::BudgetExhausted { reason } => {
                        return Ok(cook_report(
                            cook_id,
                            "execution_budget_exhausted",
                            attempts,
                            None,
                            Some(format!(
                                "provider execution stopped because {reason} was exhausted"
                            )),
                            1,
                            Some(&run_id),
                        ));
                    }
                    CookFollowUpDispatch::PolicyFailure { reason } => {
                        return Ok(cook_report(
                            cook_id,
                            "policy_failure",
                            attempts,
                            None,
                            Some(reason),
                            1,
                            Some(&run_id),
                        ));
                    }
                }
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                return Ok(cook_report(
                    cook_id,
                    "retries_exhausted",
                    attempts,
                    None,
                    Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    1,
                    Some(&run_id),
                ));
            }
        }
    }

    Ok(cook_report(
        cook_id,
        "retries_exhausted",
        attempts,
        None,
        Some("cook attempt budget exhausted".to_string()),
        1,
        Some(&run_id),
    ))
}

#[cfg(test)]
#[path = "cook_tests.rs"]
mod tests;
