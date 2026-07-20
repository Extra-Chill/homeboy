//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use crate::agent_task::{AgentTaskExecutor, AgentTaskRequest};
use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::agent_task_dispatch_plan::build_dispatch_plan;
use crate::agent_task_dispatch_service::{self, AgentTaskDispatchCommand};
use crate::agent_task_finalization::{
    AgentTaskPrFinalizationBackend, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_gate::VerifyGateOptions;
use crate::agent_task_gate::{
    failure_fingerprint, run_gate_command_with_timeout, AgentTaskGateBaselineComparison,
    AgentTaskGateStatus,
};
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::resolve_candidate_revision;
use crate::agent_task_promotion::{
    promote_with_checkpoint, AgentTaskPromotionOptions, AgentTaskPromotionReport,
};
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
use super::cook_promotion::{
    attempt_needs_execution, cook_report, finalize_or_load_cook_pr,
    finalize_or_load_cook_pr_with_backend, persisted_promotion_for_attempt,
    promote_or_load_attempt, promotion_source,
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
    /// Preserves the lifecycle-owned failure boundary when cook stops before
    /// provider dispatch instead of collapsing it into an attempt-budget result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_failure_classification: Option<String>,
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
    let (adoption_ai_model, ai_model_source) = match adoption.ai_model {
        Some(model) => (concrete_adoption_ai_model(&model)?, "candidate_input"),
        None => (
            concrete_adoption_ai_model(options.ai_model.as_deref().unwrap_or_default())?,
            "recipe_finalization",
        ),
    };
    let source_worktree = options.source_worktree_path.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "candidate_ref",
            "candidate adoption requires the recorded source worktree",
            None,
            None,
        )
    })?;
    // Resolve the caller input to the commit object before durable ownership is
    // claimed, then use that immutable SHA for every subsequent operation.
    let candidate_sha = resolve_candidate_revision(source_worktree, candidate_ref)?;
    let gate_identity = if options.gates.verify.is_empty() {
        "promotion verification".to_string()
    } else {
        options.gates.verify.join(" && ")
    };
    if !options.gates.rerun_completed_gates
        && record.candidate_adoption.as_ref().is_some_and(|adoption| {
            adoption.state == "completed"
                && adoption.candidate_sha == candidate_sha
                && adoption.ai_model == adoption_ai_model
        })
    {
        let promotion = persisted_promotion_for_attempt(&record.run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate_ref",
                "completed candidate adoption is missing its persisted promotion result",
                Some(candidate_sha.clone()),
                None,
            )
        })?;
        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt: 1,
            max_attempts: options.max_attempts,
            source_run_id: Some(record.run_id.clone()),
            current_diff: String::new(),
            metadata: serde_json::json!({"adopted_candidate_ref": candidate_ref}),
        });
        let finalization = record.metadata.get("cook_finalization").cloned();
        let status = finalization
            .as_ref()
            .and_then(|value| value["status"].as_str())
            .unwrap_or("green_no_finalize")
            .to_string();
        return Ok(cook_report(
            cook_id.to_string(),
            &status,
            vec![AgentTaskCookAttemptReport {
                attempt: 1,
                run_id: record.run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path.clone(),
                promotion: Some(promotion),
                feedback: Some(feedback),
            }],
            finalization,
            Some("reused the completed candidate adoption result; set rerun_completed_gates to rerun its gates".to_string()),
            if status == "review_ready" || status == "green_no_finalize" { 0 } else { 1 },
        ));
    }
    agent_task_lifecycle::start_candidate_adoption_with_rerun_policy(
        &record.run_id,
        &candidate_sha,
        &adoption_ai_model,
        &gate_identity,
        options.gates.rerun_completed_gates,
    )?;
    let gate_run_id = record.run_id.clone();
    let promotion = crate::agent_task_promotion::with_gate_supervision(
        crate::agent_task_gate::GateSupervision {
            timeout: options.gates.gate_timeout(),
            heartbeat_interval: options.gates.gate_heartbeat_interval(),
            on_spawn: Arc::new({
                let run_id = gate_run_id.clone();
                move |pid, command| {
                    agent_task_lifecycle::start_candidate_adoption_gate(
                        &run_id,
                        command,
                        pid,
                        options.gates.gate_timeout_seconds,
                    )
                }
            }),
            on_heartbeat: Arc::new({
                let run_id = gate_run_id.clone();
                move |tail| agent_task_lifecycle::heartbeat_candidate_adoption_gate(&run_id, tail)
            }),
            is_cancelled: Arc::new(move || {
                agent_task_lifecycle::candidate_adoption_cancel_requested(&gate_run_id)
                    .unwrap_or(false)
            }),
        },
        || {
            promote_with_checkpoint(
                AgentTaskPromotionOptions {
                    source,
                    source_run_id: Some(record.run_id.clone()),
                    source_path,
                    source_worktree_path: options.source_worktree_path.clone(),
                    base_ref: Some(options.base.clone()),
                    task_base_sha: options.task_base_sha.clone(),
                    candidate_ref: Some(candidate_sha.clone()),
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
                    agent_task_lifecycle::checkpoint_candidate_adoption(
                        &record.run_id,
                        "post_apply_verification",
                        &gate_identity,
                    )?;
                    agent_task_lifecycle::record_promotion(&record.run_id, checkpoint).map(|_| ())
                },
            )
        },
    );
    let mut promotion = match promotion {
        Ok(promotion) => promotion,
        Err(error) => {
            agent_task_lifecycle::finish_candidate_adoption(
                &record.run_id,
                Some(error.message.clone()),
            )?;
            return Err(error);
        }
    };
    if agent_task_lifecycle::candidate_adoption_cancel_requested(&record.run_id)? {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "candidate adoption was cancelled before baseline verification",
            Some(record.run_id.clone()),
            None,
        ));
    }
    let task_base_sha = options.task_base_sha.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "task_base_sha",
            "candidate adoption requires the recorded immutable task base for baseline-aware verification",
            None,
            None,
        )
    })?;
    compare_adoption_gate_failures_to_base(
        &mut promotion,
        source_worktree,
        task_base_sha,
        &record.run_id,
    )?;
    if agent_task_lifecycle::candidate_adoption_cancel_requested(&record.run_id)? {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "candidate adoption was cancelled before promotion could finalize",
            Some(record.run_id.clone()),
            None,
        ));
    }
    // The adopted candidate did not run through this cook's provider lifecycle.
    // Bind its declared model to the authenticated promotion instead of inferring
    // one from the immutable execution plan.
    options.ai_model = Some(adoption_ai_model.clone());
    promotion.provenance["adoption"] = serde_json::json!({
        "source_run_id": record.run_id,
        "candidate_ref": candidate_sha,
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
        agent_task_lifecycle::finish_candidate_adoption(
            &record.run_id,
            Some("adopted candidate did not pass the original deterministic gates".to_string()),
        )?;
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
        agent_task_lifecycle::finish_candidate_adoption(&record.run_id, None)?;
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
    agent_task_lifecycle::checkpoint_candidate_adoption(
        &record.run_id,
        "finalization",
        "finalize pull request",
    )?;
    let finalization = match finalize_or_load_cook_pr_with_backend(
        &options,
        &record.run_id,
        &promotion,
        backend,
    ) {
        Ok(finalization) => finalization,
        Err(error) => {
            agent_task_lifecycle::finish_candidate_adoption(
                &record.run_id,
                Some(error.message.clone()),
            )?;
            return Err(error);
        }
    };
    agent_task_lifecycle::finish_candidate_adoption(&record.run_id, None)?;
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

/// Candidate adoption can prove that an otherwise-red broad command is
/// inherited only by executing the identical command at the recorded base.
/// A changed failure fingerprint is deliberately still a hard gate failure.
fn compare_adoption_gate_failures_to_base(
    promotion: &mut AgentTaskPromotionReport,
    source_worktree: &std::path::Path,
    task_base_sha: &str,
    run_id: &str,
) -> Result<()> {
    if !promotion.status.gate_failed() {
        return Ok(());
    }
    let baseline_root = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create candidate-adoption gate baseline".to_string()),
        )
    })?;
    let baseline_path = baseline_root.path().join("base");
    let output = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&baseline_path)
        .arg(task_base_sha)
        .current_dir(source_worktree)
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("materialize candidate-adoption gate baseline".to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_io(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
            Some("materialize immutable candidate-adoption gate baseline".to_string()),
        ));
    }
    let baseline_result = (|| -> Result<bool> {
        // Match promotion's dependency and isolated-runtime setup before the
        // base command is allowed to serve as comparison evidence.
        homeboy_core::hygiene::materialize_worktree_dependencies(&baseline_path)?;
        let mut all_failures_inherited = true;
        let failed_gate_count = promotion
            .deterministic_gates
            .iter()
            .filter(|gate| gate.status == AgentTaskGateStatus::Failed)
            .count();
        if failed_gate_count == 0 {
            return Ok(false);
        }
        let mut compared = 0;
        for (index, gate) in promotion.deterministic_gates.iter_mut().enumerate() {
            if gate.status != AgentTaskGateStatus::Failed {
                continue;
            }
            compared += 1;
            agent_task_lifecycle::checkpoint_candidate_adoption(
                run_id,
                "baseline_verification",
                &format!("baseline gate {compared}/{failed_gate_count}"),
            )?;
            let command = gate.command.last().cloned().unwrap_or_default();
            let baseline_run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
            let baseline_runtime = homeboy_core::engine::invocation::InvocationGuard::acquire(
                &baseline_run_dir,
                &homeboy_core::engine::invocation::InvocationRequirements::default(),
            )?;
            let baseline = run_gate_command_with_timeout(
                &baseline_path,
                index + 1,
                &command,
                gate.visibility,
                gate.reveal_policy,
                &baseline_runtime.context().tmp_dir,
                std::time::Duration::from_secs(5 * 60),
            )?;
            let candidate_fingerprint = failure_fingerprint(&gate.stdout, &gate.stderr);
            let baseline_fingerprint = failure_fingerprint(&baseline.stdout, &baseline.stderr);
            let matches = baseline.status == AgentTaskGateStatus::Failed
                && candidate_fingerprint == baseline_fingerprint;
            gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                base_ref: task_base_sha.to_string(),
                exit_code: baseline.exit_code,
                failure_fingerprint: baseline_fingerprint,
                matches_candidate_failure: matches,
            });
            all_failures_inherited &= matches;
            if matches {
                gate.accept_inherited_failure();
            }
        }
        Ok(all_failures_inherited)
    })();
    let cleanup = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&baseline_path)
        .current_dir(source_worktree)
        .status()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("remove candidate-adoption gate baseline".to_string()),
            )
        })?;
    if !cleanup.success() {
        return Err(Error::internal_io(
            "git worktree remove failed".to_string(),
            Some("remove candidate-adoption gate baseline".to_string()),
        ));
    }
    let all_failures_inherited = baseline_result?;
    if all_failures_inherited {
        promotion.status = crate::agent_task_promotion::AgentTaskPromotionStatus::Applied;
        for result in &mut promotion.gate_results {
            if result.status == homeboy_core::gate::HomeboyGateStatus::Failed {
                result.status = homeboy_core::gate::HomeboyGateStatus::Passed;
                result.summary = "candidate failure matches the immutable baseline; no candidate regression detected".to_string();
                result.retryable = Some(false);
            }
        }
    }
    Ok(())
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

/// Persist the controller-owned initial attempt before transport preparation so
/// runner eligibility failures remain addressable through the cook alias.
fn materialize_initial_cook_attempt(options: &AgentTaskCookServiceOptions) -> Result<()> {
    if agent_task_lifecycle::run_record_exists(&options.initial_run_id)? {
        return Ok(());
    }
    match agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id)) {
        Ok(_) => {
            agent_task_lifecycle::record_cook_attempt(&options.cook_id, 1, &options.initial_run_id)
                .map(|_| ())
        }
        Err(error) => {
            // `submit_plan` persists admission failures before returning them.
            if agent_task_lifecycle::run_record_exists(&options.initial_run_id)? {
                agent_task_lifecycle::record_cook_attempt(
                    &options.cook_id,
                    1,
                    &options.initial_run_id,
                )?;
            }
            Err(error)
        }
    }
}

fn retryable_pre_execution_failure(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    record.metadata["pre_execution_failure"]["retryable"] == Value::Bool(true)
}

#[derive(Debug)]
struct PreExecutionFailureDetails {
    retryable: bool,
    phase: Option<String>,
    classification: Option<String>,
}

fn with_pre_execution_phase(mut error: Error, phase: &str) -> Error {
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["pre_execution_phase"] = Value::String(phase.to_string());
    error
}

fn pre_execution_failure_phase<'a>(
    error: &'a Error,
    dispatcher: Option<&dyn AgentTaskCookAttemptDispatcher>,
) -> &'a str {
    error
        .details
        .get("pre_execution_phase")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            dispatcher
                .map(|dispatcher| dispatcher.pre_execution_failure_phase())
                .unwrap_or("cook_pre_execution")
        })
}

fn pre_execution_failure_details(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    error: &Error,
) -> PreExecutionFailureDetails {
    let failure = record.and_then(|record| record.metadata.get("pre_execution_failure"));
    PreExecutionFailureDetails {
        retryable: failure
            .and_then(|failure| failure.get("retryable"))
            .and_then(Value::as_bool)
            .unwrap_or(error.retryable == Some(true)),
        phase: failure
            .and_then(|failure| failure.get("phase"))
            .and_then(Value::as_str)
            .map(str::to_string),
        classification: failure
            .and_then(|failure| failure.get("failure_classification"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn pre_execution_failure_report(
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    failure: PreExecutionFailureDetails,
    error: Error,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let phase = failure.phase.as_deref().unwrap_or("cook_pre_execution");
    let classification = failure.classification.as_deref().unwrap_or("unknown");
    let mut report = cook_report(
        cook_id,
        "pre_execution_failure",
        attempts,
        None,
        Some(format!(
            "pre-provider failure in phase `{phase}` classified as `{classification}`: {error}"
        )),
        1,
    );
    report.value.terminal_phase = failure.phase;
    report.value.terminal_failure_classification = failure.classification;
    report
}

/// Pre-execution failures happen before a provider can receive work. Persist a
/// normal terminal run so the Cook alias can expose its complete retry history.
fn record_pre_execution_failure(
    plan: &AgentTaskPlan,
    run_id: &str,
    error: &Error,
    phase: &str,
) -> Result<()> {
    if !agent_task_lifecycle::run_record_exists(run_id)? {
        agent_task_lifecycle::submit_plan(plan, Some(run_id))?;
    }
    agent_task_lifecycle::record_pre_execution_failure(run_id, plan, phase, error)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::super::cook_baseline::git_output;
    use super::super::cook_promotion::{
        finalize_cook_pr_with_backend, persisted_promotion_for_attempt,
    };
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
    use sha2::Digest;
    use std::process::Command;
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

    #[derive(Clone)]
    struct SucceedingExecutor;

    impl AgentTaskExecutorAdapter for SucceedingExecutor {
        fn execute(
            &self,
            request: crate::agent_task::AgentTaskRequest,
            _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
        ) -> crate::agent_task::AgentTaskOutcome {
            let root = std::path::PathBuf::from(
                request
                    .workspace
                    .root
                    .as_deref()
                    .expect("provider receives attempt workspace"),
            );
            std::fs::write(root.join("provider.txt"), "completed\n")
                .expect("write provider change");
            let git = |args: &[&str]| {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&root)
                    .status()
                    .expect("run provider git")
                    .success());
            };
            git(&["add", "provider.txt"]);
            git(&[
                "-c",
                "user.name=Homeboy",
                "-c",
                "user.email=homeboy@localhost",
                "commit",
                "-m",
                "provider change",
            ]);
            crate::agent_task::AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("fixture provider succeeded".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
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
    struct RetryableTransportFailingAttemptDispatcher {
        dispatches: Arc<AtomicUsize>,
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
                )
                .with_retryable(true));
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

    impl AgentTaskCookAttemptDispatcher for RetryableTransportFailingAttemptDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-retryable-transport-failure" }))
        }

        fn dispatch_attempt(
            &self,
            _plan: AgentTaskPlan,
            _run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            Err(Error::internal_unexpected("fixture transport disconnected").with_retryable(true))
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
                logs.events.last().map(|event| event.status),
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
    fn retry_after_admission_failure_rebuilds_clean_initial_candidate_baseline() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("temp source root");
            let source = temp.path().join("source");
            std::fs::create_dir(&source).expect("create source");
            let git = |args: &[&str]| {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("run git")
                    .success());
            };
            git(&["init"]);
            git(&["config", "user.email", "agent@example.test"]);
            git(&["config", "user.name", "Agent"]);
            std::fs::write(source.join("fixture.txt"), "base\n").expect("write base");
            git(&["add", "fixture.txt"]);
            git(&["commit", "-m", "base"]);
            std::fs::write(source.join("fixture.txt"), "dirty candidate\n")
                .expect("write dirty candidate");

            let run_id = "cook-admission-retry-attempt-1";
            let mut options = batch_cook_options(
                "cook-admission-retry",
                Arc::new(AdmissionFailingAttemptDispatcher {
                    message: "controller generation is held by another cook",
                }),
            );
            options.initial_run_id = run_id.to_string();
            options.source_worktree_path = Some(source.clone());
            options.provider_command = Some("fixture-provider".to_string());
            options.initial_plan.tasks[0].workspace.root = Some(source.display().to_string());

            run_cook(options, UnusedExecutor).expect("persist admission failure");
            let failed_plan = agent_task_lifecycle::load_plan(run_id).expect("failed plan");
            let transient_root = std::path::PathBuf::from(
                failed_plan.tasks[0]
                    .workspace
                    .root
                    .as_deref()
                    .expect("baseline root"),
            );
            assert!(!transient_root.exists(), "initial baseline was cleaned up");

            let retry = agent_task_lifecycle::retry(run_id, Some("cook-admission-retry-2"))
                .expect("retry rematerializes source workspace");
            let retry_plan = agent_task_lifecycle::load_plan(&retry.run_id).expect("retry plan");
            assert_eq!(
                retry_plan.tasks[0].workspace.root.as_deref(),
                Some(source.to_str().expect("UTF-8 source path"))
            );

            let result = crate::agent_task_service::execution::run_submitted(
                retry.run_id,
                SucceedingExecutor,
            )
            .expect("retry reaches a real Git workspace");
            assert_eq!(result.exit_code, 0, "{:#?}", result.value);
        });
    }

    #[test]
    fn cook_transport_preparation_failure_is_durable_and_resumes_after_runner_recovery() {
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

            let error = run_cook(options.clone(), UnusedExecutor)
                .expect_err("transport preparation is outside the provider-attempt loop");

            assert!(error.message.contains("fixture runner is unavailable"));
            let blocked = agent_task_lifecycle::status(cook_id)
                .expect("cook alias exposes the preflight-blocked attempt");
            assert_eq!(blocked.run_id, first_run_id);
            assert_eq!(
                blocked.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                blocked.metadata["pre_execution_failure"]["retryable"],
                Value::Bool(true)
            );

            let resumed = run_cook(options, UnusedExecutor)
                .expect("repaired runner resumes the immutable cook attempt");
            assert_eq!(resumed.value.status, "in_flight");
            assert_eq!(
                agent_task_lifecycle::status(cook_id)
                    .expect("resumed cook alias")
                    .runner_job_id(),
                Some("accepted-daemon-job")
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
            options.max_attempts = 3;

            let result =
                run_cook(options, UnusedExecutor).expect("cook records materialization failure");

            assert_eq!(result.value.status, "pre_execution_failure");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(
                result.value.terminal_phase.as_deref(),
                Some("materialize_initial_candidate_baseline")
            );
            assert_eq!(
                result.value.terminal_failure_classification.as_deref(),
                Some("invalid_input")
            );
            let record =
                agent_task_lifecycle::status(cook_id).expect("cook alias resolves failure");
            assert_eq!(record.run_id, run_id);
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
        });
    }

    #[cfg(unix)]
    #[test]
    fn cook_claims_its_durable_attempt_before_slow_baseline_materialization() {
        homeboy_core::test_support::with_isolated_home(|_| {
            use std::os::unix::fs::PermissionsExt;

            let temp = tempfile::tempdir().expect("temp source root");
            let source = temp.path().join("source");
            std::fs::create_dir(&source).expect("create source repository");
            for args in [
                vec!["init"],
                vec!["config", "user.email", "agent@example.test"],
                vec!["config", "user.name", "Agent"],
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("run git")
                    .success());
            }
            std::fs::write(source.join("lib.rs"), "base\n").expect("write base");
            for args in [vec!["add", "lib.rs"], vec!["commit", "-m", "base"]] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("run git")
                    .success());
            }
            std::fs::write(source.join("lib.rs"), "candidate\n").expect("dirty candidate");

            let entered = temp.path().join("baseline-entered");
            let release = temp.path().join("baseline-release");
            let wrapper = temp.path().join("git");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nif test \"$1\" = status; then touch \"{}\"; while ! test -f \"{}\"; do sleep 0.01; done; fi\nexec /usr/bin/git \"$@\"\n",
                    entered.display(),
                    release.display(),
                ),
            )
            .expect("write slow git wrapper");
            let mut permissions = std::fs::metadata(&wrapper)
                .expect("wrapper metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");
            let previous_path = std::env::var_os("PATH");
            std::env::set_var(
                "PATH",
                format!(
                    "{}:{}",
                    temp.path().display(),
                    previous_path
                        .as_deref()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
            );

            let dispatches = Arc::new(AtomicUsize::new(0));
            let mut options = batch_cook_options(
                "cook-slow-baseline",
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::clone(&dispatches),
                }),
            );
            options.initial_run_id = "cook-slow-baseline-attempt-1".to_string();
            options.provider_command = Some("fixture-provider".to_string());
            options.source_worktree_path = Some(source);
            let resume_options = options.clone();
            let controller = std::thread::spawn(move || run_cook(options, UnusedExecutor));
            let entered_staging = (0..500).any(|_| {
                if entered.exists() {
                    true
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    false
                }
            });
            let durable = entered_staging.then(|| {
                agent_task_lifecycle::status("cook-slow-baseline-attempt-1")
                    .expect("staging attempt is durable before controller completion")
            });
            std::fs::write(&release, "release").expect("release baseline staging");
            let result = controller
                .join()
                .expect("controller thread")
                .expect("accepted detached attempt");
            match previous_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }

            assert!(entered_staging, "baseline materialization did not block");
            let durable = durable.expect("durable record while staging was blocked");
            assert_eq!(
                durable.state,
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
            assert!(agent_task_lifecycle::load_plan(&durable.run_id).is_ok());
            assert_eq!(result.value.status, "in_flight");
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);

            let resumed =
                run_cook(resume_options, UnusedExecutor).expect("resume accepted handoff");
            assert_eq!(resumed.value.status, "in_flight");
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);
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
            let record = agent_task_lifecycle::status("cook-runner-exhaustion")
                .expect("transport failure remains inspectable");
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
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
            assert_eq!(result.value.status, "pre_execution_failure");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(result.value.history_run_ids, vec![run_id]);
            assert_eq!(
                result.value.terminal_phase.as_deref(),
                Some("controller_admission")
            );
            assert_eq!(
                result.value.terminal_failure_classification.as_deref(),
                Some("invalid_input")
            );
            let record = agent_task_lifecycle::status(run_id).expect("attempt exists");
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
        });
    }

    #[test]
    fn cook_retries_retryable_pre_provider_transport_failures_within_attempt_budget() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dispatches = Arc::new(AtomicUsize::new(0));
            let cook_id = "cook-retryable-transport";
            let mut options = batch_cook_options(
                cook_id,
                Arc::new(RetryableTransportFailingAttemptDispatcher {
                    dispatches: Arc::clone(&dispatches),
                }),
            );
            options.provider_command = Some("fixture-provider".to_string());
            options.initial_run_id = "cook-retryable-transport-attempt-1".to_string();
            options.max_attempts = 2;

            let result = run_cook(options, UnusedExecutor).expect("cook records transport retries");

            assert_eq!(result.exit_code, 1);
            assert_eq!(result.value.status, "retries_exhausted");
            assert_eq!(result.value.attempts.len(), 2);
            assert_eq!(dispatches.load(Ordering::SeqCst), 2);
            assert_eq!(result.value.history_run_ids.len(), 2);
            assert_eq!(
                result.value.history_run_ids[0],
                "cook-retryable-transport-attempt-1"
            );
            assert!(
                result.value.history_run_ids[1].starts_with("cook-retryable-transport-attempt-2-")
            );
            for run_id in &result.value.history_run_ids {
                let record = agent_task_lifecycle::status(run_id).expect("retry attempt exists");
                assert!(record.provider_handles.is_empty());
                assert_eq!(record.metadata["provider_executions_consumed"], 0);
                assert_eq!(record.metadata["pre_execution_failure"]["retryable"], true);
                assert_eq!(
                    record.metadata["pre_execution_failure"]["failure_classification"],
                    "transient"
                );
            }
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
                "pre_execution_failure"
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
            let provider_started = temp.path().join("provider-started");
            let provider_release = temp.path().join("provider-release");
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncat >/dev/null\ntouch {provider_started}\nwhile ! test -f {provider_release}; do sleep 0.01; done\ngit -C {target} fetch origin {candidate}\ngit -C {target} checkout --detach FETCH_HEAD\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
                    target = target.display(),
                    provider_started = provider_started.display(),
                    provider_release = provider_release.display(),
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

            let candidate_for_thread = candidate.clone();
            let adoption = std::thread::spawn(move || {
                let mut backend = CaptureBackend {
                    hydrate_run_id: Some(run_id.to_string()),
                    ..Default::default()
                };
                let result = adopt_cook_candidate_with_dispatcher_and_backend(
                    cook_id,
                    &candidate_for_thread,
                    AgentTaskCandidateAdoptionOptions {
                        ai_model: Some("openai/gpt-5.6-sol".to_string()),
                    },
                    |_| Ok(None),
                    &mut backend,
                );
                (result, backend)
            });
            let provider_started_in_time = (0..500).any(|_| {
                if provider_started.exists() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            });
            let running = provider_started_in_time
                .then(|| agent_task_lifecycle::status(run_id))
                .transpose();
            // Always release and join before asserting so a regression cannot
            // strand the fake provider and hang the test process.
            std::fs::write(&provider_release, "release").expect("release provider");
            let adoption_result = adoption.join();
            assert!(provider_started_in_time, "promotion provider did not start");
            let running = running
                .expect("blocked adoption status")
                .expect("provider started before status capture");
            let active = running.candidate_adoption.expect("active adoption attempt");
            assert_eq!(active.state, "verification_running");
            assert_eq!(active.phase, "verification");
            assert_eq!(active.active_gate, "test \"$(cat lib.rs)\" = candidate");
            assert_eq!(active.candidate_sha, candidate);
            assert_eq!(active.ai_model, "openai/gpt-5.6-sol");
            assert_eq!(active.owner_pid, std::process::id());
            assert!(!active.heartbeat_at.is_empty());
            let (result, backend) = adoption_result.expect("adoption thread completes");
            let result = result.expect("historical recipe adoption succeeds");

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
            let adoption = promoted
                .candidate_adoption
                .expect("terminal adoption status");
            assert_eq!(adoption.state, "completed");
            assert_eq!(adoption.candidate_sha, candidate);
            assert_eq!(adoption.ai_model, "openai/gpt-5.6-sol");
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
            "deterministic_gates": [{"id": "gate", "visibility": "visible", "reveal_policy": "full_evidence", "status": "succeeded", "command": ["sh", "-lc", "cargo test --locked agent_task_promotion --lib"], "exit_code": 0}],
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
                    ..Default::default()
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
                "Verified candidate changed 1 file(s): src/lib.rs.",
                "Cook completed 1 deterministic verification gate(s) before finalization.",
                "1. Run `cargo test --locked agent_task_promotion --lib`; expect passes as recorded by Cook's deterministic gate.",
                "Compatibility impact is unknown from durable task and promotion evidence.",
                "Verified candidate scope: 1 changed file(s): src/lib.rs.",
                "Cook deterministic verification: 1 gate(s) completed green.",
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
    fn cook_rejects_test_claim_without_matching_durable_gate() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cook-8058-mismatch";
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
                    verify: vec!["cargo test unsupported".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: Default::default(),
                    ..VerifyGateOptions::default()
                },
                max_attempts: 1,
                no_finalize: false,
                base: "main".to_string(),
                task_base_sha: Some("task-candidate-base".to_string()),
                head: Some("fix/8058".to_string()),
                title: "Close #8058".to_string(),
                commit_message: "test".to_string(),
                source_refs: Vec::new(),
                protected_branches: vec!["main".to_string()],
                ai_tool: "OpenCode".to_string(),
                ai_model: Some("openai/gpt-5.6-terra".to_string()),
                ai_used_for: "Drafted test coverage.".to_string(),
                attempt_dispatcher: None,
                harvest_context: crate::agent_task_scheduler::HarvestExecutionContext::default(),
            };
            let error = finalize_cook_pr_with_backend(
                &options,
                run_id,
                &promotion(run_id),
                &mut CaptureBackend::default(),
            )
            .expect_err("unsupported test claim is rejected");
            assert!(error
                .message
                .contains("matching successful visible durable gate"));
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
