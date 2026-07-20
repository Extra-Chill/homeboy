//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use sha2::Digest;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};

use crate::agent_task::{AgentTaskExecutor, AgentTaskRequest};
use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::agent_task_dispatch_plan::build_dispatch_plan;
use crate::agent_task_dispatch_service::{self, AgentTaskDispatchCommand};
use crate::agent_task_finalization::{
    finalize_pr_with_backend, AgentTaskPrEvidence, AgentTaskPrFinalizationBackend,
    AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship,
    AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_gate::VerifyGateOptions;
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{
    normalize_promotion_patch, promote_with_checkpoint, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewTestStep,
};
use crate::agent_task_scheduler::{
    AgentTaskExecutionBudget, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::{config, Error, ErrorCode, Result};

use super::cook_budget::{
    budget_remaining, execution_budget_usage, reserve_remediation_budget, ExecutionBudgetUsage,
};
use super::execution::run_loaded_plan_with_derived_cook_baseline;
use super::AgentTaskRunResult;

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
    pub status: String,
    pub attempts: Vec<AgentTaskCookAttemptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalization: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

/// Adopt an externally prepared immutable commit into a durable cook. The
/// source recipe remains authoritative for repository, base, gates, and
/// finalization policy; adoption only replaces provider artifact harvesting.
pub fn adopt_cook_candidate(
    cook_or_run_id: &str,
    candidate_ref: &str,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    adopt_cook_candidate_with_options_and_dispatcher(
        cook_or_run_id,
        candidate_ref,
        AgentTaskCandidateAdoptionOptions::default(),
        |_| Ok(None),
    )
}

/// Compatibility entry point for callers that previously supplied attempt
/// transport reconstruction. Candidate adoption never replays provider work,
/// so the dispatcher is intentionally not reconstructed or prepared.
pub fn adopt_cook_candidate_with_dispatcher(
    cook_or_run_id: &str,
    candidate_ref: &str,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    adopt_cook_candidate_with_options_and_dispatcher(
        cook_or_run_id,
        candidate_ref,
        AgentTaskCandidateAdoptionOptions::default(),
        reconstruct_dispatcher,
    )
}

/// Adopt a candidate with provenance supplied by the external preparer.
pub fn adopt_cook_candidate_with_options_and_dispatcher(
    cook_or_run_id: &str,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    adopt_cook_candidate_with_dispatcher_and_backend(
        cook_or_run_id,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        &mut RealAgentTaskPrFinalizationBackend,
    )
}

fn adopt_cook_candidate_with_dispatcher_and_backend<B: AgentTaskPrFinalizationBackend>(
    cook_or_run_id: &str,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    _reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    backend: &mut B,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let (record, recipe) = resolve_adoption_target(cook_or_run_id)?;
    let cook_id = &recipe.cook_id;
    let mut options = super::reconstruct_adoption_options(&recipe)?;
    let run_id = record.run_id.clone();
    let plan = agent_task_lifecycle::load_plan(&run_id)?;
    let source_request = plan.tasks.first().cloned().ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            "candidate adoption requires a cook run with one source task",
            Some(run_id.clone()),
            None,
        )
    })?;
    if plan.tasks.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "candidate adoption supports one task per cook",
            Some(run_id),
            None,
        ));
    }
    let (source, source_path, recovery) = candidate_adoption_source(&record, &source_request)?;
    let mut promotion = promote_with_checkpoint(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some(record.run_id.clone()),
            source_path,
            source_worktree_path: options.source_worktree_path.clone(),
            base_ref: Some(options.base.clone()),
            task_base_sha: options.task_base_sha.clone(),
            candidate_ref: Some(candidate_ref.to_string()),
            to_worktree: options.to_worktree.clone(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: options.gates.clone(),
            provider_command: options.provider_command.clone(),
            provider_invocation: options.provider_invocation.clone(),
        },
        |checkpoint| {
            let checkpoint = serde_json::to_value(checkpoint).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize adopted candidate checkpoint".to_string()),
                )
            })?;
            agent_task_lifecycle::record_promotion(&record.run_id, checkpoint).map(|_| ())
        },
    )?;
    let (adoption_ai_model, ai_model_source) = match adoption.ai_model {
        Some(model) => (concrete_adoption_ai_model(&model)?, "candidate_input"),
        None => (
            concrete_adoption_ai_model(options.ai_model.as_deref().unwrap_or_default())?,
            "recipe_finalization",
        ),
    };
    // The adopted candidate did not run through this cook's provider lifecycle.
    // Bind its declared model to the authenticated promotion instead of inferring
    // one from the immutable execution plan.
    options.ai_model = Some(adoption_ai_model.clone());
    promotion.provenance["adoption"] = serde_json::json!({
        "source_run_id": record.run_id,
        "candidate_ref": candidate_ref,
        "source_worktree_path": options.source_worktree_path,
        "recorded_task_base": options.task_base_sha,
        "recovery": recovery,
        "ai_model": adoption_ai_model,
        "ai_model_source": ai_model_source,
    });
    let promotion_value = serde_json::to_value(&promotion)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    agent_task_lifecycle::record_promotion(&record.run_id, promotion_value)?;
    let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
        source_request,
        promotion_report: promotion.clone(),
        attempt: 1,
        max_attempts: options.max_attempts,
        source_run_id: Some(record.run_id.clone()),
        current_diff: String::new(),
        metadata: serde_json::json!({"adopted_candidate_ref": candidate_ref}),
    });
    let attempt = AgentTaskCookAttemptReport {
        attempt: 1,
        run_id: record.run_id.clone(),
        run_state: format!("{:?}", record.state),
        aggregate_path: record.aggregate_path.clone(),
        promotion: Some(promotion.clone()),
        feedback: Some(feedback.clone()),
    };
    if feedback.status != AgentTaskCookLoopStatus::GreenCompleted {
        return Ok(cook_report(
            cook_id.to_string(),
            "gate_failed",
            vec![attempt],
            None,
            Some("adopted candidate did not pass the original deterministic gates".to_string()),
            1,
        ));
    }
    if options.no_finalize {
        return Ok(cook_report(
            cook_id.to_string(),
            "green_no_finalize",
            vec![attempt],
            None,
            Some(
                "adopted candidate passed deterministic gates; recipe skips finalization"
                    .to_string(),
            ),
            0,
        ));
    }
    let finalization =
        finalize_or_load_cook_pr_with_backend(&options, &record.run_id, &promotion, backend)?;
    let status = finalization["status"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let exit_code = if status == "review_ready" { 0 } else { 1 };
    Ok(cook_report(
        cook_id.to_string(),
        &status,
        vec![attempt],
        Some(finalization),
        None,
        exit_code,
    ))
}

fn candidate_adoption_source(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    source_request: &AgentTaskRequest,
) -> Result<(String, Option<PathBuf>, Option<Value>)> {
    // The authenticated recovery marker is authoritative over a canonical
    // pre-execution aggregate, which exists only to record the transport error.
    if let Some(outcome) =
        agent_task_lifecycle::candidate_adoption_recovery_outcome(record, source_request)
    {
        let recovery = outcome.metadata["candidate_adoption_recovery"].clone();
        return Ok((
            serde_json::to_string(&outcome).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize candidate adoption recovery outcome".to_string()),
                )
            })?,
            None,
            Some(recovery),
        ));
    }
    if let Ok((source, path)) = agent_task_lifecycle::aggregate_source(&record.run_id) {
        return Ok((source, Some(path), None));
    }
    let (source, path) = promotion_source(&record.run_id)?;
    Ok((source, path, None))
}

fn concrete_adoption_ai_model(value: &str) -> Result<String> {
    let normalized = value.trim();
    if normalized.is_empty()
        || value != normalized
        || value.chars().any(char::is_control)
        || matches!(
            normalized.to_ascii_lowercase().as_str(),
            "not recorded"
                | "unknown"
                | "ai-assisted"
                | "ai assisted"
                | "legacy caller did not record a model"
        )
    {
        return Err(Error::validation_invalid_argument(
            "ai_model",
            "candidate adoption requires a concrete model identifier",
            None,
            None,
        ));
    }
    Ok(normalized.to_string())
}

/// Resolve an existing run first, then recover a deterministic persisted
/// attempt when a controller stopped after writing its recipe and before
/// writing the run.
fn resolve_adoption_target(
    cook_or_run_id: &str,
) -> Result<(
    agent_task_lifecycle::AgentTaskRunRecord,
    super::AgentTaskCookRecipe,
)> {
    if agent_task_lifecycle::run_record_exists(cook_or_run_id)? {
        let record = agent_task_lifecycle::status(cook_or_run_id)?;
        let cook_id = record
            .metadata
            .get("cook_id")
            .and_then(Value::as_str)
            .unwrap_or(cook_or_run_id)
            .to_string();
        return Ok((record, super::load_recipe(&cook_id)?));
    }

    let recipe = if super::recipe_exists(cook_or_run_id)? {
        super::load_recipe(cook_or_run_id)?
    } else if let Some(recipe) = super::load_recipe_for_attempt(cook_or_run_id)? {
        recipe
    } else {
        return Err(Error::validation_invalid_argument(
            "run_or_cook_id",
            "unknown agent-task run or durable cook id",
            Some(cook_or_run_id.to_string()),
            None,
        ));
    };
    let attempt = if recipe.cook_id == cook_or_run_id {
        resolve_cook_adoption_attempt(&recipe)?
    } else {
        recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == cook_or_run_id)
            .expect("attempt lookup returns a recipe that declares the run id")
    };
    if agent_task_lifecycle::run_record_exists(&attempt.run_id)? {
        return Ok((agent_task_lifecycle::status(&attempt.run_id)?, recipe));
    }

    agent_task_lifecycle::submit_plan(&attempt.plan, Some(&attempt.run_id))?;
    agent_task_lifecycle::record_cook_attempt(&recipe.cook_id, attempt.attempt, &attempt.run_id)?;
    let recovery = Error::internal_unexpected(
        "recovered orphaned durable cook recipe before provider dispatch".to_string(),
    );
    let record = agent_task_lifecycle::record_pre_execution_failure(
        &attempt.run_id,
        &attempt.plan,
        "transport_dispatcher_prepare",
        &recovery,
    )?;
    Ok((record, recipe))
}

/// A retried cook may have several lifecycle attempts for the same immutable
/// plan. The earliest is the stable target; different plans require an explicit
/// run ID so a candidate is never attached to the wrong policy.
fn resolve_cook_adoption_attempt(
    recipe: &super::AgentTaskCookRecipe,
) -> Result<&super::AgentTaskCookRecipeAttempt> {
    let first = recipe
        .attempts
        .first()
        .expect("loaded cook recipes always contain an attempt");
    if recipe
        .attempts
        .iter()
        .all(|attempt| attempt.plan == first.plan)
    {
        return Ok(first);
    }

    let attempts = recipe
        .attempts
        .iter()
        .map(|attempt| format!("attempt {}: {}", attempt.attempt, attempt.run_id))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::validation_invalid_argument(
        "cook_recipe.attempts",
        format!(
            "candidate adoption by cook id is ambiguous because durable attempt plans differ ({attempts}); rerun with the exact owning run id, for example `homeboy agent-task adopt {}`",
            first.run_id
        ),
        Some(recipe.cook_id.clone()),
        Some(vec![
            "Pass an attempt run id to select the candidate's exact recorded policy."
                .to_string(),
        ]),
    ))
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

pub fn run_cook<E>(
    options: AgentTaskCookServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    // A configured provider is controller authority. Resolve it before an
    // external runner can spend a provider attempt; explicit transports are
    // caller-owned overrides and retain their existing behavior.
    if options.attempt_dispatcher.is_none()
        && options.provider_command.is_none()
        && options.provider_invocation.is_none()
    {
        crate::agent_task_promotion::preflight_configured_workspace_provider(&options.to_worktree)?;
    }
    // The durable reconstruction boundary must exist before an external provider
    // can accept the first attempt.
    let existing_recipe = super::recipe_exists(&options.cook_id)?;
    let recipe = super::persist_initial_recipe(&options)?;
    // A recipe can survive an interruption before its first lifecycle record.
    // Resume from the validated durable inputs so ambient transport state cannot
    // turn replay into a conflicting new cook.
    let options = if existing_recipe {
        super::reconstruct_options_with_dispatcher(&recipe, options.attempt_dispatcher)?
    } else {
        options
    };
    // Transport readiness can serialize on a reconnect/runtime-promotion
    // lease. Complete it before entering the provider-attempt loop so that
    // waiting for a shared Lab session never consumes a cook attempt.
    if let Some(dispatcher) = &options.attempt_dispatcher {
        dispatcher.prepare_for_cook()?;
    }
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut run_id = options.initial_run_id.clone();
    let mut next_plan = Some(options.initial_plan.clone());
    let cook_id = options.cook_id.clone();
    let mut budget_limit = None;
    let mut observed_budget_used = ExecutionBudgetUsage::default();
    let mut remediation_category_usage = ExecutionBudgetUsage::default();

    for attempt in 1..=max_attempts {
        let plan = match next_plan.take() {
            Some(plan) => plan,
            None => agent_task_lifecycle::load_plan(&run_id)?,
        };
        let needs_execution = agent_task_lifecycle::status(&run_id)
            .map(|record| {
                !matches!(
                    record.state,
                    agent_task_lifecycle::AgentTaskRunState::Succeeded
                        | agent_task_lifecycle::AgentTaskRunState::PartialFailure
                        | agent_task_lifecycle::AgentTaskRunState::Failed
                        | agent_task_lifecycle::AgentTaskRunState::Cancelled
                ) && !record.lab_handoff.as_ref().is_some_and(|handoff| {
                    handoff.state == agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
                })
            })
            .unwrap_or(true);
        if needs_execution {
            let execution = (|| {
                let initial_baseline = if attempt == 1 {
                    materialize_initial_candidate_baseline(
                        &plan,
                        options.source_worktree_path.as_deref(),
                        &run_id,
                    )?
                } else {
                    None
                };
                let mut dispatch_plan = plan.clone();
                if let Some(baseline) = initial_baseline.as_ref() {
                    for task in &mut dispatch_plan.tasks {
                        task.workspace.root = Some(baseline.path.display().to_string());
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
                    Ok(record) => Some(record),
                    Err(_) => {
                        let phase = options
                            .attempt_dispatcher
                            .as_ref()
                            .map(|dispatcher| dispatcher.pre_execution_failure_phase())
                            .unwrap_or("cook_pre_execution");
                        record_pre_execution_failure(&plan, &run_id, &error, phase)?;
                        agent_task_lifecycle::status(&run_id).ok()
                    }
                };
                agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: record
                        .as_ref()
                        .map(|record| format!("{:?}", record.state))
                        .unwrap_or_else(|| "DispatchFailed".to_string()),
                    aggregate_path: record.and_then(|record| record.aggregate_path),
                    promotion: None,
                    feedback: None,
                });
                if is_deterministic_pre_execution_failure(&error) {
                    return Ok(cook_report(
                        cook_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(error.to_string()),
                        1,
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
        if record.state == agent_task_lifecycle::AgentTaskRunState::Running
            && record.runner_job_id().is_some()
        {
            // A detached runner handoff has durably accepted the provider
            // child. Its daemon owns timeout and provider rotation; this
            // controller returns without attempting to read a future aggregate.
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
            ));
        }

        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
                | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
        ) {
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
                ));
            }
        };

        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt,
            max_attempts,
            source_run_id: Some(run_id.clone()),
            current_diff: String::new(),
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
                    ));
                }
                let finalization = finalize_or_load_cook_pr(&options, &run_id, &promotion)?;
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
                ));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(mut follow_up_request) = follow_up_request else {
                    return Ok(cook_report(
                        cook_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cook feedback requested retry without a follow-up request".to_string(),
                        ),
                        1,
                    ));
                };
                let next_run_id = agent_task_lifecycle::cook_attempt_run_id(&cook_id, attempt + 1);
                let Some(remaining_budget) = budget_limit
                    .as_ref()
                    .and_then(|budget| budget_remaining(budget, budget_used))
                else {
                    return Ok(cook_report(
                        cook_id,
                        "execution_budget_exhausted",
                        attempts,
                        None,
                        Some("provider execution stopped because max_provider_executions was exhausted".to_string()),
                        1,
                    ));
                };
                let Some(same_provider) =
                    terminal_executor_matches(&aggregate, &follow_up_request.executor)
                else {
                    return Ok(cook_report(
                        cook_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cannot classify Cook remediation without terminal executor identity"
                                .to_string(),
                        ),
                        1,
                    ));
                };
                let reservation = match reserve_remediation_budget(&remaining_budget, same_provider)
                {
                    Ok(reservation) => reservation,
                    Err(exhausted_budget) => {
                        return Ok(cook_report(
                            cook_id,
                            "execution_budget_exhausted",
                            attempts,
                            None,
                            Some(format!("provider execution stopped because {exhausted_budget} was exhausted")),
                            1,
                        ));
                    }
                };
                // This is durable evidence, not the process-local baseline
                // capability. A restarted controller re-materializes and
                // verifies that capability from the exact promoted artifact.
                follow_up_request.inputs["cook_loop"]["artifact_provenance"] = serde_json::json!({
                    "source_run_id": run_id,
                    "source_task_id": promotion.source.task_id,
                    "source_patch_artifact_sha256": promotion.patch_artifact.sha256,
                });
                let mut follow_up_plan = AgentTaskPlan::new(
                    format!("{cook_id}-cook-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                follow_up_plan.options = plan.options.clone();
                follow_up_plan.options.execution_budget = AgentTaskExecutionBudget::new(1, 0, 0);
                follow_up_plan.options.retry.max_attempts = 1;
                super::record_recipe_attempt(&cook_id, attempt + 1, &next_run_id, &follow_up_plan)?;
                // A restarted controller may find this exact retry already
                // accepted or terminal. Its durable recipe is the dispatch
                // boundary; never send the provider a second copy.
                if attempt_needs_execution(&next_run_id) {
                    let baseline = match materialize_follow_up_baseline(&promotion, &run_id) {
                        Ok(baseline) => baseline,
                        Err(error) => {
                            return Ok(cook_report(
                                cook_id,
                                "policy_failure",
                                attempts,
                                None,
                                Some(error.to_string()),
                                1,
                            ));
                        }
                    };
                    let mut follow_up_plan = follow_up_plan;
                    follow_up_plan.tasks[0].workspace.root =
                        Some(baseline.path.display().to_string());
                    // Requests retain artifact facts for review, never authorization.
                    follow_up_plan.tasks[0].inputs["cook_loop"]["artifact_provenance"] =
                        baseline.artifact_provenance();
                    if let Some(dispatcher) = &options.attempt_dispatcher {
                        dispatcher.dispatch_attempt(
                            follow_up_plan,
                            &next_run_id,
                            Some(baseline.capability()),
                        )?;
                    } else {
                        run_loaded_plan_with_derived_cook_baseline(
                            follow_up_plan,
                            Some(&next_run_id),
                            executor.clone(),
                            Some(baseline.capability()),
                            Some(cook_attempt_harvest_context(&options.harvest_context)),
                        )?;
                    }
                }
                remediation_category_usage.add(reservation);
                run_id = next_run_id;
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
    ))
}

fn is_deterministic_pre_execution_failure(error: &Error) -> bool {
    matches!(
        error.code,
        ErrorCode::ConfigInvalidJson
            | ErrorCode::ConfigInvalidValue
            | ErrorCode::ValidationMissingArgument
            | ErrorCode::ValidationInvalidArgument
            | ErrorCode::ValidationInvalidJson
            | ErrorCode::ValidationMultipleErrors
    )
}

/// Pre-execution failures happen before a provider can receive work. Persist a
/// normal terminal run so the Cook alias can expose its complete retry history.
fn record_pre_execution_failure(
    plan: &AgentTaskPlan,
    run_id: &str,
    error: &Error,
    phase: &str,
) -> Result<()> {
    if agent_task_lifecycle::submit_plan(plan, Some(run_id)).is_ok() {
        agent_task_lifecycle::record_pre_execution_failure(run_id, plan, phase, error)?;
    }
    Ok(())
}

/// A cook-owned detached checkout turns the already-promoted dirty candidate
/// into a clean commit before the scheduler creates its normal attempt checkout.
struct CookFollowUpBaseline {
    source_root: PathBuf,
    path: PathBuf,
    capability: DerivedCookBaselineCapability,
}

fn cook_attempt_harvest_context(
    harvest_context: &crate::agent_task_scheduler::HarvestExecutionContext,
) -> crate::agent_task_scheduler::HarvestExecutionContext {
    harvest_context.clone()
}

/// Process-local authority for one materialized cook retry baseline. It is not
/// serializable and never enters a request, environment, or durable record.
pub struct DerivedCookBaselineCapability {
    canonical_path: PathBuf,
    commit: String,
    tree: String,
    artifact_sha256: String,
    source_run_id: String,
    source_task_id: String,
    parent_snapshot: Option<Value>,
    preexisting_candidate: bool,
}

impl DerivedCookBaselineCapability {
    pub fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }

    pub(crate) fn commit(&self) -> &str {
        &self.commit
    }

    pub(crate) fn tree(&self) -> &str {
        &self.tree
    }

    pub(crate) fn source_task_id(&self) -> &str {
        &self.source_task_id
    }

    pub(crate) fn parent_snapshot(&self) -> Option<&Value> {
        self.parent_snapshot.as_ref()
    }

    pub(crate) fn artifact_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "source_patch_artifact_sha256": self.artifact_sha256,
        })
    }

    /// Evidence derived from the controller-validated capability. It is not
    /// authorization for remote workspace or snapshot verification.
    pub fn verified_baseline_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "promoted_patch_artifact_sha256": self.artifact_sha256,
            "baseline_commit": self.commit,
            "baseline_tree": self.tree,
            "parent_snapshot_identity": self.parent_snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .get("workspace_snapshot_identity")
                    .cloned()
                    .or_else(|| snapshot.get("identity").cloned())
            }),
            "preexisting_candidate": self.preexisting_candidate,
        })
    }
}

impl CookFollowUpBaseline {
    fn capability(&self) -> &DerivedCookBaselineCapability {
        &self.capability
    }

    fn artifact_provenance(&self) -> Value {
        self.capability.artifact_provenance()
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_derived_cook_baseline_capability(
    path: PathBuf,
    commit: String,
    tree: String,
    task_id: &str,
    parent_snapshot: Option<Value>,
) -> DerivedCookBaselineCapability {
    DerivedCookBaselineCapability {
        canonical_path: path
            .canonicalize()
            .expect("test baseline path canonicalizes"),
        commit,
        tree,
        artifact_sha256: "test-artifact-sha256".to_string(),
        source_run_id: "test-source-run".to_string(),
        source_task_id: task_id.to_string(),
        parent_snapshot,
        preexisting_candidate: false,
    }
}

/// Materialize a Cook-declared dirty candidate in a detached checkout before
/// provider dispatch. The caller workspace is never staged, reset, or edited.
fn materialize_initial_candidate_baseline(
    plan: &AgentTaskPlan,
    source_root: Option<&std::path::Path>,
    source_run_id: &str,
) -> Result<Option<CookFollowUpBaseline>> {
    let Some(source_root) = source_root else {
        return Ok(None);
    };
    let status = git_output(
        source_root,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    if status.is_empty() {
        return Ok(None);
    }
    let task_id = plan
        .tasks
        .first()
        .map(|task| task.task_id.as_str())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "plan.tasks",
                "Cook cannot adopt a dirty candidate without a provider task",
                None,
                None,
            )
        })?;
    if plan.tasks.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "plan.tasks",
            "Cook can adopt a pre-existing candidate only for a single provider task",
            None,
            Some(vec![
                "Run one Cook task per dirty candidate workspace.".to_string()
            ]),
        ));
    }
    let base = git_output(source_root, &["rev-parse", "HEAD"])?;
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook candidate Git index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    git_output_with_env(
        source_root,
        &["read-tree", &base],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    git_output_with_env(
        source_root,
        &["add", "--all"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let tree = git_output_with_env(
        source_root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let commit = git_output_with_env(
        source_root,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "homeboy: Cook pre-existing candidate baseline",
        ],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let parent = std::env::temp_dir().join("homeboy-cook-initial-baselines");
    std::fs::create_dir_all(&parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook candidate baseline directory".to_string()),
        )
    })?;
    let path = parent.join(format!("baseline-{}", uuid::Uuid::new_v4()));
    let path_string = path.display().to_string();
    git_output(
        source_root,
        &["worktree", "add", "--detach", &path_string, &commit],
    )?;
    let canonical_path = path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook candidate baseline".to_string()),
        )
    })?;
    Ok(Some(CookFollowUpBaseline {
        source_root: source_root.to_path_buf(),
        path,
        capability: DerivedCookBaselineCapability {
            canonical_path,
            commit,
            tree: tree.clone(),
            artifact_sha256: format!("{:x}", sha2::Sha256::digest(tree.as_bytes())),
            source_run_id: source_run_id.to_string(),
            source_task_id: task_id.to_string(),
            parent_snapshot: None,
            preexisting_candidate: true,
        },
    }))
}

impl Drop for CookFollowUpBaseline {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.source_root)
            .status();
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_root)
            .status();
    }
}

fn materialize_follow_up_baseline(
    promotion: &AgentTaskPromotionReport,
    source_run_id: &str,
) -> Result<CookFollowUpBaseline> {
    let source_root = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "gate-failed promotion did not report its managed target workspace",
                None,
                None,
            )
        })?;
    let expected_head = promotion.target.head.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "promotion.target.head",
            "gate-failed promotion did not record the immutable target HEAD",
            None,
            None,
        )
    })?;
    if git_output(&source_root, &["rev-parse", "HEAD"])? != expected_head {
        return Err(Error::validation_invalid_argument(
            "promotion.target.head",
            "promotion target HEAD changed after the gate-failed promotion; refusing cook retry baseline",
            None,
            None,
        ));
    }
    let parent_snapshot = std::env::var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV)
        .ok()
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument("source_snapshot", error.to_string(), None, None)
        })?;
    let artifact_bytes = std::fs::read(&promotion.patch_artifact.path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read promoted patch artifact".to_string()),
        )
    })?;
    let artifact_sha256 = format!("{:x}", sha2::Sha256::digest(&artifact_bytes));
    if let Some(expected) = promotion.patch_artifact.sha256.as_deref() {
        if expected != artifact_sha256 {
            return Err(Error::validation_invalid_argument(
                "promotion.patch_artifact.sha256",
                "promoted artifact bytes no longer match durable sha256",
                None,
                None,
            ));
        }
    }
    let artifact = std::str::from_utf8(&artifact_bytes).map_err(|error| {
        Error::validation_invalid_argument(
            "promotion.patch_artifact",
            format!("patch bytes are not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    let normalized = normalize_promotion_patch(artifact, &promotion.to_worktree)?;
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create cook baseline Git index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    git_output_with_env(
        &source_root,
        &["read-tree", expected_head],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    git_output_with_env(
        &source_root,
        &["add", "--all"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let target_tree = git_output_with_env(
        &source_root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let parent = std::env::temp_dir().join("homeboy-cook-follow-up-baselines");
    std::fs::create_dir_all(&parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create cook baseline directory".to_string()),
        )
    })?;
    let path = parent.join(format!("baseline-{}", uuid::Uuid::new_v4()));
    let path_string = path.display().to_string();
    git_output(
        &source_root,
        &["worktree", "add", "--detach", &path_string, expected_head],
    )?;
    let baseline = CookFollowUpBaseline {
        source_root,
        path: path.clone(),
        // The capability is completed only after the committed baseline's
        // identity has been verified below.
        capability: DerivedCookBaselineCapability {
            canonical_path: path,
            commit: String::new(),
            tree: String::new(),
            artifact_sha256,
            source_run_id: source_run_id.to_string(),
            source_task_id: promotion.source.task_id.clone(),
            parent_snapshot,
            preexisting_candidate: false,
        },
    };
    let patch_path = baseline.path.join(".homeboy-cook-baseline.patch");
    std::fs::write(&patch_path, normalized.content.as_bytes()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write cook baseline patch".to_string()),
        )
    })?;
    git_output(
        &baseline.path,
        &[
            "apply",
            "--whitespace=nowarn",
            &patch_path.display().to_string(),
        ],
    )?;
    std::fs::remove_file(&patch_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("remove cook baseline patch".to_string()),
        )
    })?;
    git_output(&baseline.path, &["add", "--all"])?;
    git_output(
        &baseline.path,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit",
            "--no-verify",
            "-m",
            "homeboy: cook promoted baseline",
        ],
    )?;
    let commit = git_output(&baseline.path, &["rev-parse", "HEAD"])?;
    let tree = git_output(&baseline.path, &["rev-parse", "HEAD^{tree}"])?;
    if tree != target_tree {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion target contains extra, missing, or unrelated changes; refusing cook retry baseline",
            None,
            None,
        ));
    }
    let mut baseline = baseline;
    baseline.capability.canonical_path = baseline.path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize cook retry baseline".to_string()),
        )
    })?;
    baseline.capability.commit = commit;
    baseline.capability.tree = tree;
    Ok(baseline)
}

fn git_output(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    git_output_with_env(cwd, args, &[])
}

fn git_output_with_env(
    cwd: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("git {}", args.join(" "))))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn terminal_executor_matches(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
    follow_up: &AgentTaskExecutor,
) -> Option<bool> {
    let outcome = aggregate.outcomes.last()?;
    let terminal = terminal_executor_identity(outcome)?;
    Some(
        terminal.backend == follow_up.backend
            && terminal.selector == follow_up.selector
            && terminal.model.as_deref() == follow_up.model(),
    )
}

pub(crate) fn provider_rotation_attempts(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Option<Vec<crate::agent_task_scheduler::AgentTaskProviderRotationAttempt>> {
    serde_json::from_value(
        outcome
            .metadata
            .pointer("/provider_rotation/attempts")?
            .clone(),
    )
    .ok()
}

struct TerminalExecutorIdentity {
    backend: String,
    selector: Option<String>,
    model: Option<String>,
}

fn terminal_executor_identity(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Option<TerminalExecutorIdentity> {
    if let Some(attempt) =
        provider_rotation_attempts(outcome).and_then(|attempts| attempts.last().cloned())
    {
        return Some(TerminalExecutorIdentity {
            backend: attempt.backend,
            selector: attempt.selector,
            model: attempt.model,
        });
    }
    let executor = outcome.metadata.get("executor")?;
    Some(TerminalExecutorIdentity {
        backend: executor.get("backend")?.as_str()?.to_string(),
        selector: executor
            .get("selector")
            .and_then(Value::as_str)
            .map(str::to_string),
        model: executor
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub fn source_worktree_path(cwd: Option<String>, workspace: Option<String>) -> Option<PathBuf> {
    cwd.or_else(|| {
        workspace.and_then(|workspace| {
            let path = PathBuf::from(&workspace);
            path.exists().then_some(workspace)
        })
    })
    .map(PathBuf::from)
}

pub fn ai_model_from_tool(ai_tool: &str) -> Option<String> {
    let start = ai_tool.find('(')?;
    let end = ai_tool[start + 1..].find(')')? + start + 1;
    let model = ai_tool[start + 1..end].trim();
    (!model.is_empty()).then(|| model.to_string())
}

pub fn promotion_source(spec: &str) -> Result<(String, Option<PathBuf>)> {
    if spec != "-" {
        let path = PathBuf::from(spec.strip_prefix('@').unwrap_or(spec));
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "read agent-task promotion source {}",
                        path.display()
                    )),
                )
            })?;
            return Ok((raw, Some(path)));
        }
    }

    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

fn promote_attempt(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let (source, source_path) = promotion_source(run_id)?;
    promote_with_checkpoint(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some(run_id.to_string()),
            source_path,
            source_worktree_path: options.source_worktree_path.clone(),
            base_ref: Some(options.base.clone()),
            task_base_sha: options.task_base_sha.clone(),
            candidate_ref: None,
            to_worktree: options.to_worktree.clone(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: options.gates.clone(),
            provider_command: options.provider_command.clone(),
            provider_invocation: options.provider_invocation.clone(),
        },
        |checkpoint| {
            agent_task_lifecycle::record_promotion(
                run_id,
                serde_json::to_value(checkpoint).map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("serialize pending cook promotion".to_string()),
                    )
                })?,
            )?;
            Ok(())
        },
    )
}

/// Promotion is the durable boundary between a terminal provider result and
/// controller-owned gates. Reconciliation must reuse this exact report rather
/// than apply the selected artifact again.
fn promote_or_load_attempt(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    if let Some(promotion) = persisted_promotion_for_attempt(run_id)? {
        return Ok(promotion);
    }
    let promotion = promote_attempt(options, run_id)?;
    crate::agent_task_lifecycle::record_promotion(
        run_id,
        serde_json::to_value(&promotion).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize cook promotion".to_string()),
            )
        })?,
    )?;
    Ok(promotion)
}

fn persisted_promotion_for_attempt(run_id: &str) -> Result<Option<AgentTaskPromotionReport>> {
    let record = agent_task_lifecycle::status(run_id)?;
    let Some(value) = record.metadata.get("latest_promotion") else {
        return Ok(None);
    };
    let promotion: AgentTaskPromotionReport =
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "latest_promotion",
                format!("persisted cook promotion is invalid: {error}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
    if promotion.source.run_id.as_deref() != Some(run_id) {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.source.run_id",
            "persisted cook promotion does not belong to this attempt",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(Some(promotion))
}

fn attempt_needs_execution(run_id: &str) -> bool {
    agent_task_lifecycle::status(run_id)
        .map(|record| {
            !matches!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Succeeded
                    | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                    | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
                    | agent_task_lifecycle::AgentTaskRunState::PartialFailure
                    | agent_task_lifecycle::AgentTaskRunState::Failed
                    | agent_task_lifecycle::AgentTaskRunState::Cancelled
            )
        })
        .unwrap_or(true)
}

/// Finalization publishes controller-owned state. Persist its completed report
/// on the attempt so a restarted continuation cannot open a second PR.
fn finalize_or_load_cook_pr(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<Value> {
    finalize_or_load_cook_pr_with_backend(
        options,
        successful_run_id,
        promotion,
        &mut RealAgentTaskPrFinalizationBackend,
    )
}

fn finalize_or_load_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    let record = agent_task_lifecycle::status(successful_run_id)?;
    if let Some(finalization) = record.metadata.get("cook_finalization") {
        return Ok(finalization.clone());
    }
    let finalization =
        finalize_cook_pr_with_backend(options, successful_run_id, promotion, backend)?;
    agent_task_lifecycle::record_cook_finalization(successful_run_id, finalization.clone())?;
    Ok(finalization)
}

fn finalize_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    if promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "agent-task cook finalization requires an applied promotion with green gates",
            None,
            None,
        ));
    }
    let path = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "promotion provider did not report the applied worktree path",
                None,
                None,
            )
        })?
        .to_string();
    let source_refs = options
        .source_refs
        .iter()
        .cloned()
        .chain(std::iter::once(format!(
            "homeboy://agent-task/run/{successful_run_id}"
        )))
        .collect();
    let artifact_refs = std::iter::once(promotion.patch_artifact.path.clone()).collect();
    let verified_base = promotion
        .verified_base
        .as_ref()
        .filter(|base| base.base == options.base && !base.sha.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.verified_base",
                "cook finalization requires the typed declared base snapshot captured before promotion gates; rerun promotion against the configured base before finalizing",
                None,
                None,
            )
        })?;
    crate::agent_task_lifecycle::record_promotion(
        successful_run_id,
        serde_json::to_value(promotion).unwrap_or(Value::Null),
    )?;
    let report = finalize_pr_with_backend(
        AgentTaskPrFinalizationOptions {
            path: path.clone(),
            run_id: successful_run_id.to_string(),
            base: options.base.clone(),
            verified_base_sha: Some(verified_base.sha.clone()),
            head: options.head.clone(),
            title: options.title.clone(),
            commit_message: options.commit_message.clone(),
            gate_results: Vec::new(),
            normalized_gate_results: promotion.gate_results.clone(),
            changed_files: promotion.changed_files.clone(),
            evidence: AgentTaskPrEvidence {
                source_refs,
                artifact_refs,
                attempt_summary: format!(
                    "{} deterministic cook gate attempt(s) completed green",
                    promotion.deterministic_gates.len()
                ),
                ai_tool: options.ai_tool.clone(),
                ai_model: options.ai_model.clone(),
                source_relationship: AgentTaskPrSourceRelationship::default(),
                verification: AgentTaskPrVerification {
                    targeted_checks_run: options.gates.verify.clone(),
                    targeted_checks_unavailable: None,
                    ci_expected: vec!["Homeboy CI after push".to_string()],
                    manual_reviewer_check: None,
                },
                runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
                lifecycle: crate::agent_task_lifecycle::status(successful_run_id)
                    .ok()
                    .map(|record| record.lifecycle),
            },
            ai_used_for: options.ai_used_for.clone(),
            review_dossier: AgentTaskReviewDossier {
                schema: "homeboy/agent-task-review-dossier/v1".to_string(),
                summary: options.title.clone(),
                what_changed: vec!["Applies the verified agent-task candidate.".to_string()],
                how_to_test: options
                    .gates
                    .verify
                    .iter()
                    .cloned()
                    .map(|command| AgentTaskReviewTestStep {
                        command,
                        expected: "passes".to_string(),
                    })
                    .collect(),
                compatibility: "No compatibility impact was recorded by the cook workflow."
                    .to_string(),
                evidence: Vec::new(),
                ai_assistance: AgentTaskReviewAiAssistance {
                    used: true,
                    tool: options.ai_tool.clone(),
                    model: options
                        .ai_model
                        .clone()
                        .unwrap_or_else(|| "not recorded".to_string()),
                    used_for: options.ai_used_for.clone(),
                },
                source_relationships: Vec::new(),
                overrides: Vec::new(),
            },
            review_profile: resolve_review_profile(&path)?,
            manual_finalization: false,
            protected_branches: options.protected_branches.clone(),
        },
        backend,
    )?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn cook_report(
    cook_id: String,
    status: &str,
    attempts: Vec<AgentTaskCookAttemptReport>,
    finalization: Option<Value>,
    stop_reason: Option<String>,
    exit_code: i32,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let (latest_run_id, history_run_ids) = agent_task_lifecycle::cook_index(&cook_id)
        .map(|index| {
            (
                Some(index.latest_run_id),
                index
                    .attempts
                    .into_iter()
                    .map(|attempt| attempt.run_id)
                    .collect(),
            )
        })
        .unwrap_or((None, Vec::new()));
    AgentTaskRunResult {
        value: AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id,
            latest_run_id,
            history_run_ids,
            status: status.to_string(),
            attempts,
            finalization,
            stop_reason,
        },
        exit_code,
    }
}

fn source_spec_path(spec: &str) -> Option<PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(PathBuf::from(spec.strip_prefix('@').unwrap_or(spec)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
    };
    use crate::agent_task_finalization::{
        AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrRef,
    };
    use crate::agent_task_scheduler::AgentTaskState;
    use homeboy_core::run_lifecycle_record::{
        ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
        RunLifecycleRecord,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, Condvar};

    #[test]
    fn cook_service_retry_uses_the_same_passed_context_after_ambient_mutation() {
        let _env_lock = homeboy_core::test_support::env_lock();
        let prior = std::env::var_os(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
        let context = crate::agent_task_scheduler::HarvestExecutionContext::default();
        let first_attempt = cook_attempt_harvest_context(&context);
        std::env::set_var(
            homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
            "ambient state must not affect a passed cook context",
        );
        let retry_attempt = cook_attempt_harvest_context(&context);
        match prior {
            Some(value) => std::env::set_var(
                homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
                value,
            ),
            None => std::env::remove_var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV),
        }

        assert_eq!(format!("{first_attempt:?}"), format!("{retry_attempt:?}"));
        assert_eq!(
            format!("{retry_attempt:?}"),
            "HarvestExecutionContext { source_snapshot: None, lab_offload: None }"
        );
    }

    #[derive(Debug)]
    struct AcceptedDetachedAttemptDispatcher;

    impl AgentTaskCookAttemptDispatcher for AcceptedDetachedAttemptDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-detached" }))
        }

        fn dispatch_attempt(
            &self,
            plan: AgentTaskPlan,
            run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "fixture-lab",
                    runner_job_id: "accepted-daemon-job",
                    remote_workspace: "/runner/workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RecordingDetachedAttemptDispatcher {
        dispatches: Arc<AtomicUsize>,
    }

    impl AgentTaskCookAttemptDispatcher for RecordingDetachedAttemptDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-recording-detached" }))
        }

        fn dispatch_attempt(
            &self,
            plan: AgentTaskPlan,
            run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "fixture-lab",
                    runner_job_id: "recording-daemon-job",
                    remote_workspace: "/runner/workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )?;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct UnusedExecutor;

    impl AgentTaskExecutorAdapter for UnusedExecutor {
        fn execute(
            &self,
            _request: crate::agent_task::AgentTaskRequest,
            _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
        ) -> crate::agent_task::AgentTaskOutcome {
            panic!("accepted detached attempts must remain daemon-owned")
        }
    }

    #[derive(Debug)]
    struct BatchAttemptDispatcher {
        barrier: Arc<Barrier>,
        entered: Arc<AtomicUsize>,
        fail: bool,
    }

    #[derive(Debug)]
    struct AdmissionFailingAttemptDispatcher {
        message: &'static str,
    }

    #[derive(Debug)]
    struct FlakyPreparationDispatcher {
        failures_remaining: AtomicUsize,
    }

    #[derive(Debug)]
    struct QueuedPreparationDispatcher {
        barrier: Arc<Barrier>,
        state: Arc<(Mutex<(bool, bool)>, Condvar)>,
        connections: Arc<AtomicUsize>,
    }

    impl AgentTaskCookAttemptDispatcher for FlakyPreparationDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-flaky-preparation" }))
        }

        fn prepare_for_cook(&self) -> Result<()> {
            if self.failures_remaining.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(Error::validation_invalid_argument(
                    "runner",
                    "fixture runner is unavailable",
                    None,
                    None,
                ));
            }
            Ok(())
        }

        fn dispatch_attempt(
            &self,
            plan: AgentTaskPlan,
            run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "fixture-lab",
                    runner_job_id: "accepted-daemon-job",
                    remote_workspace: "/runner/workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )
            .map(|_| ())
        }
    }

    impl AgentTaskCookAttemptDispatcher for QueuedPreparationDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-queued-preparation" }))
        }

        fn prepare_for_cook(&self) -> Result<()> {
            self.barrier.wait();
            let (state_mutex, ready) = &*self.state;
            let mut state = state_mutex.lock().expect("queued preparation state");
            if state.1 {
                return Ok(());
            }
            if state.0 {
                while !state.1 {
                    state = ready.wait(state).expect("queued preparation wait");
                }
                return Ok(());
            }
            state.0 = true;
            drop(state);

            self.connections.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(50));

            let mut state = state_mutex.lock().expect("queued preparation owner state");
            state.1 = true;
            ready.notify_all();
            Ok(())
        }

        fn dispatch_attempt(
            &self,
            _plan: AgentTaskPlan,
            _run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            panic!("transport preparation test does not dispatch a provider attempt")
        }
    }

    impl AgentTaskCookAttemptDispatcher for AdmissionFailingAttemptDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-admission-failure" }))
        }

        fn dispatch_attempt(
            &self,
            plan: AgentTaskPlan,
            run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            agent_task_lifecycle::submit_plan_with_runtime_admission(&plan, Some(run_id), || {
                Err::<Value, _>(Error::validation_invalid_argument(
                    "controller_admission",
                    self.message,
                    Some("fixture controller diagnostics".to_string()),
                    None,
                ))
            })?;
            Ok(())
        }
    }

    impl AgentTaskCookAttemptDispatcher for BatchAttemptDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-batch" }))
        }

        fn dispatch_attempt(
            &self,
            _plan: AgentTaskPlan,
            run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.barrier.wait();
            if self.fail {
                return Err(Error::validation_invalid_argument(
                    "dispatch",
                    "fixture dispatch failure",
                    None,
                    None,
                ));
            }
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "fixture-lab",
                    runner_job_id: "fixture-job",
                    remote_workspace: "/runner/workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )?;
            Ok(())
        }
    }

    fn batch_cook_options(
        cook_id: &str,
        dispatcher: Arc<dyn AgentTaskCookAttemptDispatcher>,
    ) -> AgentTaskCookServiceOptions {
        AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: format!("{cook_id}-run"),
            initial_plan: AgentTaskPlan::new(
                cook_id,
                vec![AgentTaskRequest {
                    schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "fixture".to_string(),
                        selector: None,
                        runtime_selection: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "complete the task".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    component_contracts: Vec::new(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    artifact_declarations: Vec::new(),
                    metadata: Value::Null,
                }],
            ),
            to_worktree: format!("fixture@{cook_id}"),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: VerifyGateOptions::default(),
            max_attempts: 1,
            no_finalize: true,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Batch cook".to_string(),
            commit_message: "test".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "test".to_string(),
            ai_model: None,
            ai_used_for: "test".to_string(),
            attempt_dispatcher: Some(dispatcher),
            harvest_context: Default::default(),
        }
    }

    #[test]
    fn cook_persists_controller_admission_timeout_before_provider_execution() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-admission-timeout";
            let run_id = "cook-admission-timeout-attempt-1";
            let mut options = batch_cook_options(
                cook_id,
                Arc::new(AdmissionFailingAttemptDispatcher {
                    message: "timed out waiting for controller generation admission",
                }),
            );
            options.provider_command = Some("fixture-provider".to_string());
            let result = run_cook(
                AgentTaskCookServiceOptions {
                    initial_run_id: run_id.to_string(),
                    ..options
                },
                UnusedExecutor,
            )
            .expect("cook returns the persisted dispatch failure");

            assert_eq!(result.exit_code, 1);
            assert_eq!(result.value.latest_run_id.as_deref(), Some(run_id));
            assert_eq!(result.value.history_run_ids, vec![run_id]);
            let record =
                agent_task_lifecycle::status(run_id).expect("returned attempt is resolvable");
            let logs =
                agent_task_lifecycle::logs(run_id).expect("failed attempt logs are resolvable");
            let retry = agent_task_lifecycle::retry(run_id, Some("cook-admission-timeout-retry"))
                .expect("failed admission attempt is retryable");

            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
            assert_eq!(
                record.metadata["pre_execution_failure"]["phase"],
                "controller_admission"
            );
            assert_eq!(
                record.metadata["pre_execution_failure"]["failure_code"],
                "controller_admission"
            );
            assert!(record.metadata["pre_execution_failure"]["message"]
                .as_str()
                .expect("failure message")
                .contains("timed out waiting for controller generation admission"));
            assert_eq!(
                record.metadata["pre_execution_failure"]["details"]["id"],
                "fixture controller diagnostics"
            );
            assert_eq!(
                record.metadata["pre_execution_failure"]["provider_executions_consumed"],
                0
            );
            assert_eq!(
                logs.events.last().map(|event| event.state),
                Some(AgentTaskState::Failed)
            );
            assert_eq!(retry.metadata["retry_of"], run_id);
            assert_eq!(
                retry.metadata["retry_origin"]["pre_execution_failure"]["phase"],
                "controller_admission"
            );
        });
    }

    #[test]
    fn cook_transport_preparation_failure_does_not_create_a_provider_attempt() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-runner-unavailable";
            let first_run_id = "cook-runner-unavailable-attempt-1";
            let mut options = batch_cook_options(
                cook_id,
                Arc::new(FlakyPreparationDispatcher {
                    failures_remaining: AtomicUsize::new(1),
                }),
            );
            options.provider_command = Some("fixture-provider".to_string());
            options.initial_run_id = first_run_id.to_string();
            options.max_attempts = 2;

            let error = run_cook(options, UnusedExecutor)
                .expect_err("transport preparation is outside the provider-attempt loop");

            assert!(error.message.contains("fixture runner is unavailable"));
            assert!(!agent_task_lifecycle::run_record_exists(first_run_id)
                .expect("transport failure does not materialize an attempt"));
            assert!(
                agent_task_lifecycle::cook_index(cook_id).is_err(),
                "transport failure must not consume a cook attempt"
            );
        });
    }

    #[test]
    fn cook_persists_materialization_failure_without_provider_execution() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("temp source root");
            let cook_id = "cook-materialization-failure";
            let run_id = "cook-materialization-failure-attempt-1";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.provider_command = Some("fixture-provider".to_string());
            options.initial_run_id = run_id.to_string();
            options.source_worktree_path = Some(temp.path().to_path_buf());

            let result =
                run_cook(options, UnusedExecutor).expect("cook records materialization failure");

            assert_eq!(result.value.status, "retries_exhausted");
            let record =
                agent_task_lifecycle::status(cook_id).expect("cook alias resolves failure");
            assert_eq!(record.run_id, run_id);
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
        });
    }

    #[test]
    fn cook_transport_preparation_failure_does_not_exhaust_cook_retries() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-runner-exhaustion";
            let mut options = batch_cook_options(
                cook_id,
                Arc::new(FlakyPreparationDispatcher {
                    failures_remaining: AtomicUsize::new(usize::MAX),
                }),
            );
            options.provider_command = Some("fixture-provider".to_string());
            options.initial_run_id = "cook-runner-exhaustion-attempt-1".to_string();
            options.max_attempts = 2;

            let error = run_cook(options, UnusedExecutor)
                .expect_err("transport preparation remains outside cook retries");

            assert!(error.message.contains("fixture runner is unavailable"));
            assert!(
                !agent_task_lifecycle::run_record_exists("cook-runner-exhaustion-attempt-1")
                    .expect("transport failure does not materialize an attempt")
            );
        });
    }

    #[test]
    fn concurrent_cooks_share_transport_readiness_before_first_provider_attempt() {
        const COOKS: usize = 6;
        let connections = Arc::new(AtomicUsize::new(0));
        let dispatcher = Arc::new(QueuedPreparationDispatcher {
            barrier: Arc::new(Barrier::new(COOKS)),
            state: Arc::new((Mutex::new((false, false)), Condvar::new())),
            connections: Arc::clone(&connections),
        });
        let preparations = (0..COOKS)
            .map(|_| {
                let dispatcher = Arc::clone(&dispatcher);
                std::thread::spawn(move || dispatcher.prepare_for_cook())
            })
            .collect::<Vec<_>>();

        for preparation in preparations {
            preparation
                .join()
                .expect("cook preparation thread")
                .expect("shared transport becomes ready");
        }
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cook_persists_controller_runtime_mismatch_before_provider_execution() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cook-runtime-mismatch-attempt-1";
            let mut options = batch_cook_options(
                "cook-runtime-mismatch",
                Arc::new(AdmissionFailingAttemptDispatcher {
                    message: "pinned controller executable hash mismatch: expected fixture, found replacement",
                }),
            );
            options.provider_command = Some("fixture-provider".to_string());
            let result = run_cook(
                AgentTaskCookServiceOptions {
                    initial_run_id: run_id.to_string(),
                    ..options
                },
                UnusedExecutor,
            )
            .expect("cook returns the persisted runtime mismatch");

            let record =
                agent_task_lifecycle::status(run_id).expect("runtime mismatch attempt exists");
            assert_eq!(result.exit_code, 1);
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
            assert!(record.metadata["pre_execution_failure"]["message"]
                .as_str()
                .expect("failure message")
                .contains("hash mismatch"));
        });
    }

    #[test]
    fn cook_does_not_retry_deterministic_pre_provider_input_failures() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cook-invalid-input-attempt-1";
            let mut options = batch_cook_options(
                "cook-invalid-input",
                Arc::new(AdmissionFailingAttemptDispatcher {
                    message: "invalid controller-owned Lab handoff input",
                }),
            );
            options.provider_command = Some("fixture-provider".to_string());
            options.initial_run_id = run_id.to_string();
            options.max_attempts = 2;

            let result = run_cook(options, UnusedExecutor)
                .expect("cook returns the persisted input failure");

            assert_eq!(result.exit_code, 1);
            assert_eq!(result.value.status, "policy_failure");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(result.value.history_run_ids, vec![run_id]);
            let record = agent_task_lifecycle::status(run_id).expect("attempt exists");
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
        });
    }

    #[test]
    fn cook_batch_preserves_order_concurrency_and_failure_isolation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let barrier = Arc::new(Barrier::new(2));
            let entered = Arc::new(AtomicUsize::new(0));
            let first = batch_cook_options(
                "first",
                Arc::new(BatchAttemptDispatcher {
                    barrier: Arc::clone(&barrier),
                    entered: Arc::clone(&entered),
                    fail: true,
                }),
            );
            let second = batch_cook_options(
                "second",
                Arc::new(BatchAttemptDispatcher {
                    barrier,
                    entered: Arc::clone(&entered),
                    fail: false,
                }),
            );
            // The batch owns concurrent dispatch, not concurrent controller
            // admission; materialize both durable run identities first.
            agent_task_lifecycle::submit_plan(&first.initial_plan, Some(&first.initial_run_id))
                .expect("submit first attempt");
            agent_task_lifecycle::submit_plan(&second.initial_plan, Some(&second.initial_run_id))
                .expect("submit second attempt");
            let result = run_cook_batch(
                AgentTaskCookBatchOptions {
                    batch_id: "fixture-batch".to_string(),
                    cooks: vec![first, second],
                    max_concurrency: 2,
                },
                UnusedExecutor,
            )
            .expect("batch completes despite an individual cook failure");

            assert_eq!(entered.load(Ordering::SeqCst), 2);
            assert_eq!(result.exit_code, 1);
            assert_eq!(result.value.status, "failed");
            assert_eq!(result.value.total, 2);
            assert_eq!(result.value.succeeded, 1);
            assert_eq!(result.value.failed, 1);
            assert_eq!(result.value.cooks[0].cook_id, "first");
            assert_eq!(result.value.cooks[0].exit_code, 1);
            assert_eq!(
                result.value.cooks[0]
                    .result
                    .as_ref()
                    .expect("failed cook report")
                    .status,
                "retries_exhausted"
            );
            assert_eq!(result.value.cooks[1].cook_id, "second");
            assert_eq!(result.value.cooks[1].exit_code, 0);
            assert_eq!(
                result.value.cooks[1]
                    .result
                    .as_ref()
                    .expect("successful cook report")
                    .status,
                "in_flight"
            );
        });
    }

    #[test]
    fn cook_returns_after_accepted_detached_attempt_without_waiting_for_daemon_completion() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cook-detached-attempt-1";
            let plan = AgentTaskPlan::new(
                "cook-detached",
                vec![AgentTaskRequest {
                    schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "fixture".to_string(),
                        selector: None,
                        runtime_selection: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "complete the task".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    component_contracts: Vec::new(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    artifact_declarations: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            let result = run_cook(
                AgentTaskCookServiceOptions {
                    cook_id: "cook-detached".to_string(),
                    initial_run_id: run_id.to_string(),
                    initial_plan: plan,
                    to_worktree: "fixture@detached".to_string(),
                    source_worktree_path: None,
                    // This test covers handoff only; an explicit transport
                    // intentionally bypasses configured-provider preflight.
                    provider_command: Some("fixture-promotion-provider".to_string()),
                    provider_invocation: None,
                    gates: VerifyGateOptions::default(),
                    max_attempts: 1,
                    no_finalize: true,
                    base: "main".to_string(),
                    task_base_sha: None,
                    head: None,
                    title: "Detached cook".to_string(),
                    commit_message: "test".to_string(),
                    source_refs: Vec::new(),
                    protected_branches: Vec::new(),
                    ai_tool: "test".to_string(),
                    ai_model: None,
                    ai_used_for: "test".to_string(),
                    attempt_dispatcher: Some(Arc::new(AcceptedDetachedAttemptDispatcher)),
                    harvest_context: Default::default(),
                },
                UnusedExecutor,
            )
            .expect("accepted detached cook returns");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.status, "in_flight");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(result.value.attempts[0].run_id, run_id);
            let record = agent_task_lifecycle::status(run_id).expect("detached attempt record");
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Running
            );
            assert_eq!(record.runner_id(), Some("fixture-lab"));
            assert_eq!(record.runner_job_id(), Some("accepted-daemon-job"));
        });
    }

    #[test]
    fn orphaned_recipe_materializes_once_and_rejects_changed_inputs() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-orphan-recovery";
            let run_id = "cook-orphan-recovery-attempt-1";
            let dispatches = Arc::new(AtomicUsize::new(0));
            let mut options = batch_cook_options(
                cook_id,
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::clone(&dispatches),
                }),
            );
            options.initial_run_id = run_id.to_string();
            options.provider_command = Some("fixture-provider".to_string());

            // Simulate interruption after the immutable recipe commit and before
            // the dispatcher creates the first run record.
            super::super::persist_initial_recipe(&options).expect("persist orphaned recipe");
            assert!(!agent_task_lifecycle::run_record_exists(run_id).expect("check orphan"));

            let recovered = run_cook(options.clone(), UnusedExecutor).expect("recover orphan");
            assert_eq!(recovered.value.status, "in_flight");
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);
            let record = agent_task_lifecycle::status(run_id).expect("materialized run record");
            assert_eq!(record.runner_job_id(), Some("recording-daemon-job"));

            let replayed = run_cook(options.clone(), UnusedExecutor).expect("idempotent replay");
            assert_eq!(replayed.value.status, "in_flight");
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);
            assert_eq!(agent_task_lifecycle::status(run_id).unwrap(), record);

            let mut changed = options;
            changed.title = "changed immutable finalization title".to_string();
            let error = run_cook(changed, UnusedExecutor).expect_err("changed recipe rejected");
            assert!(error
                .message
                .contains("durable cook recipe already exists with different execution inputs"));
        });
    }

    #[test]
    fn adoption_by_cook_id_materializes_the_exact_orphaned_recipe_attempt() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-adopt-orphan";
            let run_id = "cook-adopt-orphan-attempt-1";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = run_id.to_string();
            super::super::persist_initial_recipe(&options).expect("persist orphaned recipe");

            let (record, recipe) =
                resolve_adoption_target(cook_id).expect("adoption resolves orphaned cook");

            assert_eq!(recipe.cook_id, cook_id);
            assert_eq!(record.run_id, run_id);
            assert_eq!(record.metadata["cook_id"], cook_id);
            assert_eq!(
                record.metadata["pre_execution_failure"]["candidate_adoption_recovery"]["reason"],
                "pre_provider_transport_failure"
            );
            assert!(agent_task_lifecycle::run_record_exists(run_id).expect("record exists"));
        });
    }

    #[test]
    fn adoption_prefers_authenticated_preacceptance_recovery_over_failure_aggregate() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cook-adopt-preacceptance-recovery";
            let options = batch_cook_options(
                "cook-adopt-preacceptance",
                Arc::new(AcceptedDetachedAttemptDispatcher),
            );
            let plan = options.initial_plan;
            agent_task_lifecycle::record_lab_offload_phase(
                run_id,
                "homeboy-lab",
                "lab_handoff_preacceptance",
                None,
                None,
                None,
                Some(&plan),
            )
            .expect("record preacceptance phase");
            agent_task_lifecycle::record_pre_execution_failure(
                run_id,
                &plan,
                "lab_handoff_preacceptance",
                &Error::internal_unexpected("Lab handoff JSON was truncated"),
            )
            .expect("record failed preacceptance attempt");
            let record = agent_task_lifecycle::status(run_id).expect("failed attempt");
            assert!(record.aggregate_path.is_some());

            let (_source, source_path, recovery) =
                candidate_adoption_source(&record, &plan.tasks[0]).expect("recovery source");

            assert!(source_path.is_none());
            assert_eq!(
                recovery.expect("recovery provenance")["reason"],
                "pre_provider_transport_failure"
            );
        });
    }

    #[test]
    fn historical_orphan_recipe_adoption_uses_recorded_policy_without_provider_replay() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let source = temp.path().join("source");
            let target = temp.path().join("target");
            std::fs::create_dir(&source).expect("create source repository");
            let git = |cwd: &std::path::Path, args: &[&str]| {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(cwd)
                    .status()
                    .expect("run git")
                    .success());
            };
            let git_output = |cwd: &std::path::Path, args: &[&str]| {
                let output = Command::new("git")
                    .args(args)
                    .current_dir(cwd)
                    .output()
                    .expect("read git output");
                assert!(output.status.success());
                String::from_utf8(output.stdout)
                    .expect("UTF-8 git output")
                    .trim()
                    .to_string()
            };
            git(&source, &["init"]);
            git(&source, &["config", "user.email", "agent@example.test"]);
            git(&source, &["config", "user.name", "Agent"]);
            std::fs::write(source.join("lib.rs"), "base\n").expect("write base");
            git(&source, &["add", "lib.rs"]);
            git(&source, &["commit", "-m", "base"]);
            let base = git_output(&source, &["rev-parse", "HEAD"]);
            assert!(Command::new("git")
                .args(["clone", source.to_str().unwrap(), target.to_str().unwrap()])
                .status()
                .expect("clone target repository")
                .success());
            std::fs::write(source.join("lib.rs"), "candidate\n").expect("write candidate");
            git(&source, &["commit", "-am", "candidate"]);
            let candidate = git_output(&source, &["rev-parse", "HEAD"]);
            let provider = temp.path().join("promotion-provider.sh");
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncat >/dev/null\ngit -C {target} fetch origin {candidate}\ngit -C {target} checkout --detach FETCH_HEAD\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
                    target = target.display(),
                ),
            )
            .expect("write promotion provider");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mut permissions = std::fs::metadata(&provider)
                    .expect("provider metadata")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&provider, permissions).expect("make provider executable");
            }

            let cook_id = "cook-historical-adoption";
            let run_id = "cook-historical-adoption-attempt-1";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = run_id.to_string();
            options.source_worktree_path = Some(source.clone());
            options.task_base_sha = Some(base.clone());
            options.provider_command = Some(provider.display().to_string());
            options.gates.verify = vec!["test \"$(cat lib.rs)\" = candidate".to_string()];
            options.no_finalize = false;
            options.head = Some("fix/8058".to_string());
            options.ai_model = Some("openai/gpt-5.6-terra".to_string());
            let mut recipe =
                super::super::persist_initial_recipe(&options).expect("persist recipe");
            recipe.runtime_generation = "homeboy 0.291.2+96820fe8cc53".to_string();
            let recipe_path = homeboy_core::paths::homeboy_data()
                .expect("Homeboy data path")
                .join("agent-task-cooks")
                .join(cook_id)
                .join("recipe.json");
            std::fs::write(&recipe_path, serde_json::to_vec(&recipe).unwrap())
                .expect("persist historical runtime");

            let command = vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ];
            agent_task_lifecycle::record_lab_offload_planned(
                agent_task_lifecycle::LabOffloadProxyPlan {
                    run_id,
                    runner_id: "fixture-lab",
                    remote_workspace: "/runner/workspace",
                    remote_command: &command,
                    durable_plan: Some(&options.initial_plan),
                },
            )
            .expect("persist preacceptance handoff");
            agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect("link recipe attempt");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record
                    .lab_handoff
                    .as_mut()
                    .expect("typed handoff")
                    .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
            })
            .expect("expire handoff deadline");
            let expired =
                agent_task_lifecycle::status(run_id).expect("expire preacceptance handoff");
            assert_eq!(
                expired.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert!(expired.aggregate_path.is_none());
            assert!(expired.artifact_refs.is_empty());
            assert_eq!(expired.metadata["provider_executions_consumed"], 0);

            let invalid = adopt_cook_candidate(cook_id, &base)
                .expect_err("candidate validation remains active");
            assert!(invalid
                .message
                .contains("candidate revision must equal the recorded source worktree HEAD"));

            let mut backend = CaptureBackend {
                hydrate_run_id: Some(run_id.to_string()),
                ..Default::default()
            };
            let result = adopt_cook_candidate_with_dispatcher_and_backend(
                cook_id,
                &candidate,
                AgentTaskCandidateAdoptionOptions {
                    ai_model: Some("openai/gpt-5.6-sol".to_string()),
                },
                |_| Ok(None),
                &mut backend,
            )
            .expect("historical recipe adoption succeeds");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.status, "review_ready");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(
                result.value.attempts[0]
                    .promotion
                    .as_ref()
                    .unwrap()
                    .gate_results
                    .len(),
                1
            );
            assert_eq!(
                std::fs::read_to_string(target.join("lib.rs")).unwrap(),
                "candidate\n"
            );
            let promoted = agent_task_lifecycle::status(run_id).expect("adopted lifecycle record");
            assert_eq!(
                promoted.metadata["latest_promotion"]["provenance"]["adoption"]["candidate_ref"],
                candidate
            );
            assert_eq!(
                promoted.metadata["latest_promotion"]["provenance"]["adoption"]["recovery"]
                    ["provider_executions_consumed"],
                0
            );
            assert_eq!(
                promoted.metadata["latest_promotion"]["provenance"]["adoption"]["ai_model"],
                "openai/gpt-5.6-sol"
            );
            assert_eq!(
                promoted.metadata["latest_promotion"]["provenance"]["adoption"]["ai_model_source"],
                "candidate_input"
            );
            assert!(backend.body.contains("- **Tool(s):** test"));
            assert!(backend.body.contains("- **Model:** openai/gpt-5.6-sol"));
            assert!(backend.committed && backend.pushed && backend.created);
        });
    }

    #[test]
    fn adoption_rejects_missing_or_placeholder_candidate_model() {
        for model in ["", "not recorded", " unknown "] {
            let error = concrete_adoption_ai_model(model)
                .expect_err("adoption model must be a concrete identifier");
            assert_eq!(error.details["field"], "ai_model");
        }
    }

    #[test]
    fn adoption_rejects_aggregate_free_cancelled_runs_without_pre_provider_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-adopt-cancelled-without-evidence";
            let run_id = "cook-adopt-cancelled-without-evidence-attempt-1";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = run_id.to_string();
            super::super::persist_initial_recipe(&options).expect("persist recipe");
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect("link recipe attempt");
            let cancelled = agent_task_lifecycle::cancel_run(run_id, Some("fixture cancellation"))
                .expect("cancel attempt");
            assert!(cancelled.aggregate_path.is_none());

            let error = adopt_cook_candidate(cook_id, "candidate")
                .expect_err("cancelled run without recovery evidence is rejected");
            assert_eq!(error.code, homeboy_core::ErrorCode::ValidationInvalidJson);
        });
    }

    #[test]
    fn adoption_by_run_id_keeps_the_existing_lifecycle_record() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-adopt-existing-run";
            let run_id = "cook-adopt-existing-run-attempt-1";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = run_id.to_string();
            super::super::persist_initial_recipe(&options).expect("persist recipe");
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect("link cook attempt");

            let (record, recipe) =
                resolve_adoption_target(run_id).expect("adoption resolves existing run");

            assert_eq!(recipe.cook_id, cook_id);
            assert_eq!(record.run_id, run_id);
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
        });
    }

    #[test]
    fn adoption_by_cook_id_selects_the_existing_recipe_attempt_record() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-adopt-existing-attempt";
            let run_id = "cook-adopt-existing-attempt-1";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = run_id.to_string();
            super::super::persist_initial_recipe(&options).expect("persist recipe");
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect("link cook attempt");
            agent_task_lifecycle::cancel(run_id).expect("cancel recorded attempt");

            let (record, recipe) =
                resolve_adoption_target(cook_id).expect("adoption resolves recorded cook attempt");

            assert_eq!(recipe.cook_id, cook_id);
            assert_eq!(record.run_id, run_id);
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
        });
    }

    #[test]
    fn adoption_by_cook_id_uses_the_first_of_repeated_equivalent_recipe_attempts() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-adopt-equivalent-attempts";
            let first_run_id = "cook-adopt-equivalent-attempts-1";
            let second_run_id = "cook-adopt-equivalent-attempts-2";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = first_run_id.to_string();
            super::super::persist_initial_recipe(&options).expect("persist recipe");
            super::super::record_recipe_attempt(cook_id, 2, second_run_id, &options.initial_plan)
                .expect("persist second recipe attempt");
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(first_run_id))
                .expect("persist first lifecycle record");
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(second_run_id))
                .expect("persist second lifecycle record");

            let (record, recipe) = resolve_adoption_target(cook_id)
                .expect("equivalent attempts resolve deterministically");

            assert_eq!(recipe.cook_id, cook_id);
            assert_eq!(record.run_id, first_run_id);
        });
    }

    #[test]
    fn adoption_by_cook_id_rejects_conflicting_recipe_attempts_with_explicit_choices() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-adopt-conflicting-attempts";
            let first_run_id = "cook-adopt-conflicting-attempts-1";
            let second_run_id = "cook-adopt-conflicting-attempts-2";
            let mut options =
                batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
            options.initial_run_id = first_run_id.to_string();
            super::super::persist_initial_recipe(&options).expect("persist recipe");
            let mut conflicting_plan = options.initial_plan.clone();
            conflicting_plan.plan_id = "conflicting-plan".to_string();
            super::super::record_recipe_attempt(cook_id, 2, second_run_id, &conflicting_plan)
                .expect("persist conflicting second recipe attempt");

            let error = resolve_adoption_target(cook_id)
                .expect_err("conflicting recipe adoption requires an explicit run id");

            assert_eq!(error.details["field"], "cook_recipe.attempts");
            assert!(error.message.contains(first_run_id));
            assert!(error.message.contains(second_run_id));
            assert!(error
                .message
                .contains(&format!("homeboy agent-task adopt {first_run_id}")));

            let (record, recipe) = resolve_adoption_target(second_run_id)
                .expect("an exact orphaned attempt run id selects its recipe");
            assert_eq!(recipe.cook_id, cook_id);
            assert_eq!(record.run_id, second_run_id);
        });
    }

    #[test]
    fn adoption_rejects_unknown_run_or_cook_ids() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let error = resolve_adoption_target("unknown-adoption-target")
                .expect_err("unknown adoption target fails closed");

            assert_eq!(error.details["field"], "run_or_cook_id");
            assert!(error
                .message
                .contains("unknown agent-task run or durable cook id"));
        });
    }

    #[derive(Default)]
    struct CaptureBackend {
        body: String,
        committed: bool,
        pushed: bool,
        created: bool,
        hydrate_run_id: Option<String>,
    }

    impl AgentTaskPrFinalizationBackend for CaptureBackend {
        fn hydrate_run(&mut self, _run_id: &str) -> Result<RunLifecycleRecord> {
            if let Some(run_id) = self.hydrate_run_id.as_deref() {
                return RealAgentTaskPrFinalizationBackend.hydrate_run(run_id);
            }
            Ok(RunLifecycleRecord {
                execution: RunExecutionLifecycle {
                    state: RunExecutionState::Succeeded,
                    started_at: None,
                    finished_at: Some("2026-07-14T00:00:00Z".to_string()),
                    updated_at: None,
                },
                provider_runtime: vec![ProviderRuntimeLifecycle {
                    task_id: "task".to_string(),
                    backend: "opencode".to_string(),
                    state: ProviderRuntimeState::Succeeded,
                    stream_uri: None,
                    external_runtime_ids: Vec::new(),
                    metadata: serde_json::json!({"model": "openai/gpt-5.6-terra"}),
                }],
                ..RunLifecycleRecord::default()
            })
        }
        fn hydrate_gate_proof(&mut self, run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
            if self.hydrate_run_id.is_some() {
                return RealAgentTaskPrFinalizationBackend.hydrate_gate_proof(run_id);
            }
            Ok(AgentTaskPrDurableGateProof {
                run_id: run_id.to_string(),
                promotion: promotion(run_id),
            })
        }
        fn current_branch(&mut self, _path: &str) -> Result<String> {
            Ok("fix/8058".to_string())
        }
        fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
            Ok(vec!["src/lib.rs".to_string()])
        }
        fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
            self.committed = true;
            Ok(())
        }
        fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
            self.pushed = true;
            Ok(())
        }
        fn find_open_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<Option<AgentTaskPrRef>> {
            Ok(None)
        }
        fn create_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.created = true;
            self.body = body.to_string();
            Ok(AgentTaskPrRef {
                number: 8058,
                url: "https://github.com/Extra-Chill/homeboy/pull/8058".to_string(),
            })
        }
        fn update_pr(
            &mut self,
            _path: &str,
            _number: u64,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.body = body.to_string();
            unreachable!("test creates a PR")
        }
    }

    fn promotion(run_id: &str) -> AgentTaskPromotionReport {
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": {"kind": "aggregate", "task_id": "task", "run_id": run_id},
            "to_worktree": "homeboy@8058",
            "target": {"worktree": "homeboy@8058", "path": "/repo"},
            "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
            "changed_files": ["src/lib.rs"],
            "gate_results": [{"id": "gate", "name": "cargo test --locked agent_task_promotion --lib", "kind": "command", "status": "passed"}],
            "operator_notification": {"status": "completed", "message": "complete"},
            "verified_base": {"base": "main", "sha": "verified-base"},
            "provenance": {"worktree_path": "/repo"}
        })).unwrap()
    }

    #[test]
    fn restarted_cook_uses_only_its_exact_persisted_promotion() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let plan = AgentTaskPlan::new("cook-persisted", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some("run-persisted")).unwrap();
            agent_task_lifecycle::record_promotion(
                "run-persisted",
                serde_json::to_value(promotion("run-persisted")).unwrap(),
            )
            .unwrap();

            let restored = persisted_promotion_for_attempt("run-persisted")
                .unwrap()
                .expect("durable promotion");
            assert_eq!(restored.source.run_id.as_deref(), Some("run-persisted"));
            assert_eq!(restored.patch_artifact.id, "patch");
        });
    }

    #[test]
    fn persisted_promotion_from_another_attempt_is_rejected() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let plan = AgentTaskPlan::new("cook-persisted", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some("run-persisted")).unwrap();
            agent_task_lifecycle::record_promotion(
                "run-persisted",
                serde_json::to_value(promotion("different-run")).unwrap(),
            )
            .unwrap();

            let error = persisted_promotion_for_attempt("run-persisted").unwrap_err();
            assert!(error.message.contains("does not belong to this attempt"));
        });
    }

    #[test]
    fn cook_successful_concrete_attempt_publishes_reviewer_body() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cook-8058-attempt-1";
            let plan = AgentTaskPlan::new("cook-8058", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
            let options = AgentTaskCookServiceOptions {
                cook_id: "cook-8058".to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: AgentTaskPlan::new("cook-8058", Vec::new()),
                to_worktree: "homeboy@8058".to_string(),
                source_worktree_path: None,
                provider_command: None,
                provider_invocation: None,
                gates: VerifyGateOptions {
                    verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: Default::default(),
                },
                max_attempts: 1,
                no_finalize: false,
                base: "main".to_string(),
                task_base_sha: Some("task-candidate-base".to_string()),
                head: Some("fix/8058".to_string()),
                title: "Close #8058".to_string(),
                commit_message: "test".to_string(),
                source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/8058".to_string()],
                protected_branches: vec!["main".to_string()],
                ai_tool: "OpenCode".to_string(),
                ai_model: Some("openai/gpt-5.6-terra".to_string()),
                ai_used_for: "Drafted test coverage.".to_string(),
                attempt_dispatcher: None,
                harvest_context: crate::agent_task_scheduler::HarvestExecutionContext::default(),
            };
            let mut backend = CaptureBackend::default();
            finalize_cook_pr_with_backend(&options, run_id, &promotion(run_id), &mut backend)
                .unwrap();
            for section in [
                "## Summary",
                "## What changed",
                "## How to test",
                "## Compatibility",
                "## Evidence",
                "## AI assistance",
                "openai/gpt-5.6-terra",
                "Verified finalization base: main at verified-base",
                "1. Run `cargo test --locked agent_task_promotion --lib`; expect passes.",
            ] {
                assert!(
                    backend.body.contains(section),
                    "missing {section}: {}",
                    backend.body
                );
            }
            for forbidden in [
                "Publication intent",
                "homeboy/agent-task",
                "Changed files",
                "Final status",
            ] {
                assert!(
                    !backend.body.contains(forbidden),
                    "unexpected {forbidden}: {}",
                    backend.body
                );
            }
            assert!(backend.committed && backend.pushed && backend.created);
        });
    }

    #[test]
    fn follow_up_baseline_is_clean_and_preserves_binary_mode_and_untracked_candidate_state() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = &temp.path().join("repo");
        std::fs::create_dir(root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let target_head = git_output(root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("candidate.bin"), [0_u8, 1, 2, 255]).unwrap();
        std::fs::write(root.join("candidate.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(root.join("candidate.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(root.join("candidate.sh"), permissions).unwrap();
        assert!(Command::new("git")
            .args(["add", "--all"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let patch = Command::new("git")
            .args([
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--find-renames",
                "HEAD",
            ])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(patch.status.success());
        let patch_path = temp.path().join("candidate.patch");
        std::fs::write(&patch_path, patch.stdout).unwrap();
        assert!(Command::new("git")
            .args(["reset"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
            "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
            "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":target_head},
            "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path}, "changed_files":["candidate.bin", "candidate.sh"],
            "command_evidence":[], "deterministic_gates":[], "gate_results":[],
            "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
        })).unwrap();
        let baseline = materialize_follow_up_baseline(&report, "first-run").expect("baseline");
        assert!(git_output(&baseline.path, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read(baseline.path.join("candidate.bin")).unwrap(),
            [0_u8, 1, 2, 255]
        );
        assert!(
            baseline
                .path
                .join("candidate.sh")
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o111
                != 0
        );
        assert!(!baseline.capability.commit().is_empty());
        assert!(!baseline.capability.tree().is_empty());
        assert_eq!(
            baseline.artifact_provenance()["source_patch_artifact_sha256"],
            sha2::Sha256::digest(std::fs::read(&patch_path).unwrap())
                .to_vec()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }

    #[test]
    fn follow_up_baseline_refuses_when_promotion_target_head_has_advanced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "A"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        let head_a = git_output(&root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("advanced.txt"), "B\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "B"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        let patch_path = temp.path().join("candidate.patch");
        std::fs::write(&patch_path, "").unwrap();
        let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
            "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
            "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":head_a},
            "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path},
            "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
        }))
        .unwrap();

        let error = match materialize_follow_up_baseline(&report, "first-run") {
            Ok(_) => panic!("target advancement rejects the stale promotion baseline"),
            Err(error) => error,
        };

        assert!(
            error.message.contains("target HEAD changed"),
            "unexpected error: {}",
            error.message
        );
    }
}
