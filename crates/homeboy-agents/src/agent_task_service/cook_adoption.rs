//! Agent-task cook candidate adoption.
//!
//! Extracted from `cook.rs`: the `adopt_cook_candidate*` family that admits an
//! externally prepared immutable commit into a durable cook, plus the adoption
//! resolution helpers (`resolve_cook_adoption_attempt`/`resolve_adoption_target`/
//! `candidate_adoption_source`/`concrete_adoption_ai_model`) and gate-failure
//! comparison (`compare_adoption_gate_failures_to_base`). Adoption never replays
//! provider work — it replaces provider artifact harvesting while the source
//! recipe stays authoritative for repository, base, gates, and finalization.
//! This is the cluster the recent adoption-gate and candidate-recovery fixes
//! kept touching; grouping it keeps the adoption boundary in one place.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_task::AgentTaskRequest;
use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopStatus,
};
use crate::agent_task_finalization::{
    AgentTaskPrFinalizationBackend, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_gate::{
    failure_fingerprint, run_gate_command_with_timeout, AgentTaskGateBaselineComparison,
    AgentTaskGateStatus,
};
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::resolve_candidate_revision;
use crate::agent_task_promotion::{
    promote_with_checkpoint, AgentTaskPromotionOptions, AgentTaskPromotionReport,
};
use crate::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy_core::{Error, Result};

use super::cook::{
    dispatch_cook_follow_up, gate_feedback_current_diff, AgentTaskCandidateAdoptionOptions,
    AgentTaskCookAttemptDispatcher, AgentTaskCookAttemptReport, AgentTaskCookReport,
    CookFollowUpDispatch,
};
use super::cook_promotion::{
    cook_report, finalize_or_load_cook_pr_with_backend, persisted_promotion_for_attempt,
    promotion_source,
};
use super::AgentTaskRunResult;

#[derive(serde::Serialize, serde::Deserialize)]
struct CandidateAdoptionTerminalResult {
    status: String,
    stop_reason: Option<String>,
}

fn persist_adoption_terminal_result(run_id: &str, report: &AgentTaskCookReport) -> Result<()> {
    agent_task_lifecycle::record_candidate_adoption_result(
        run_id,
        serde_json::to_value(CandidateAdoptionTerminalResult {
            status: report.status.clone(),
            stop_reason: report.stop_reason.clone(),
        })
        .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
}

/// Read the AI-authored review form off an adopted candidate's terminal
/// outcome. The candidate was produced by an earlier cook attempt, so any form
/// the original agent emitted is recorded on its aggregate. Absent/invalid here
/// re-triggers the review-form gate exactly as it would for a fresh cook.
fn adopted_review_form(
    run_id: &str,
    allow_missing_aggregate: bool,
) -> Result<Option<crate::agent_task_review_dossier::AiFilledReviewForm>> {
    let aggregate = match agent_task_lifecycle::read_aggregate(run_id) {
        Ok(aggregate) => aggregate,
        Err(_) if allow_missing_aggregate => return Ok(None),
        Err(error) => return Err(error),
    };
    aggregate
        .outcomes
        .last()
        .map(|outcome| {
            crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(
                &outcome.outputs,
            )
        })
        .transpose()
        .map(Option::flatten)
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
    adopt_cook_candidate_with_options_dispatcher_and_executor(
        cook_or_run_id,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

/// Adopt a candidate and retain the normal Cook execution boundary for any
/// remediation requested by its deterministic feedback.
pub fn adopt_cook_candidate_with_options_dispatcher_and_executor<E>(
    cook_or_run_id: &str,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: crate::agent_task_scheduler::AgentTaskExecutorAdapter + Clone,
{
    adopt_cook_candidate_with_dispatcher_and_backend(
        cook_or_run_id,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        executor,
        &mut RealAgentTaskPrFinalizationBackend,
    )
}

pub(crate) fn adopt_cook_candidate_with_dispatcher_and_backend<
    E: crate::agent_task_scheduler::AgentTaskExecutorAdapter + Clone,
    B: AgentTaskPrFinalizationBackend,
>(
    cook_or_run_id: &str,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: E,
    backend: &mut B,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let (record, recipe) = resolve_adoption_target(cook_or_run_id)?;
    let cook_id = &recipe.cook_id;
    let mut options = super::reconstruct_adoption_options(&recipe)?;
    let run_id = record.run_id.clone();
    let plan = agent_task_lifecycle::load_plan(&run_id)?;
    let recipe_attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "adopted run is not declared by the durable cook recipe",
                Some(run_id.clone()),
                None,
            )
        })?;
    let adopted_attempt = recipe_attempt.attempt;
    options.initial_run_id = run_id.clone();
    options.initial_plan = recipe_attempt.plan.clone();
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
    let source_worktree = options.source_worktree_path.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "candidate_ref",
            "candidate adoption requires the recorded source worktree",
            None,
            None,
        )
    })?;
    // Resolve the caller input to the commit object before durable ownership is
    // claimed, then use that immutable SHA for every subsequent operation.
    let candidate_sha = resolve_candidate_revision(&source_worktree, candidate_ref)?;
    let gate_identity = if options.gates.verify.is_empty() {
        "promotion verification".to_string()
    } else {
        options.gates.verify.join(" && ")
    };
    if !options.gates.rerun_completed_gates
        && record.candidate_adoption.as_ref().is_some_and(|adoption| {
            adoption.candidate_sha == candidate_sha
                && adoption.ai_model == adoption_ai_model
                && (adoption.state == "completed" || adoption.result.is_some())
        })
    {
        if let Some(result) = record
            .candidate_adoption
            .as_ref()
            .and_then(|adoption| adoption.result.clone())
        {
            let result: CandidateAdoptionTerminalResult = serde_json::from_value(result)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            let exit_code =
                if matches!(result.status.as_str(), "review_ready" | "green_no_finalize") {
                    0
                } else {
                    1
                };
            return Ok(cook_report(
                cook_id.to_string(),
                &result.status,
                Vec::new(),
                None,
                result.stop_reason,
                exit_code,
            ));
        }
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
            attempt: adopted_attempt,
            max_attempts: options.max_attempts,
            source_run_id: Some(record.run_id.clone()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: adopted_review_form(&record.run_id, recovery.is_some())?,
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
    let attempt_dispatcher =
        reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
    options.attempt_dispatcher = attempt_dispatcher;
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
        &source_worktree,
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
        attempt: adopted_attempt,
        max_attempts: options.max_attempts,
        source_run_id: Some(record.run_id.clone()),
        current_diff: gate_feedback_current_diff(&promotion),
        require_review_form: true,
        review_form: adopted_review_form(&record.run_id, recovery.is_some())?,
        metadata: serde_json::json!({"adopted_candidate_ref": candidate_ref}),
    });
    let attempt = AgentTaskCookAttemptReport {
        attempt: adopted_attempt,
        run_id: record.run_id.clone(),
        run_state: format!("{:?}", record.state),
        aggregate_path: record.aggregate_path.clone(),
        promotion: Some(promotion.clone()),
        feedback: Some(feedback.clone()),
    };
    if feedback.status == AgentTaskCookLoopStatus::RetryRequested {
        let Some(mut follow_up_request) = feedback.follow_up_request.clone() else {
            agent_task_lifecycle::finish_candidate_adoption(
                &record.run_id,
                Some(
                    "candidate adoption feedback requested retry without a follow-up request"
                        .to_string(),
                ),
            )?;
            let report = cook_report(
                cook_id.to_string(),
                "policy_failure",
                vec![attempt],
                None,
                Some(
                    "candidate adoption feedback requested retry without a follow-up request"
                        .to_string(),
                ),
                1,
            );
            persist_adoption_terminal_result(&record.run_id, &report.value)?;
            return Ok(report);
        };
        // An authenticated pre-provider recovery has no aggregate executor
        // evidence. The concrete adopted model is the authority for the
        // remediation request and makes its same-provider budget category
        // explicit rather than inferred from an absent provider execution.
        follow_up_request.executor.model = Some(adoption_ai_model.clone());
        let budget = plan.options.execution_budget.clone();
        let mut remediation_usage = Default::default();
        let aggregate = match agent_task_lifecycle::read_aggregate(&record.run_id) {
            Ok(aggregate) => aggregate,
            Err(_) if recovery.is_some() => crate::agent_task_scheduler::AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed,
                totals: crate::agent_task_scheduler::AgentTaskAggregateTotals {
                    failed: 1,
                    ..Default::default()
                },
                outcomes: Vec::new(),
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
            Err(error) => return Err(error),
        };
        let dispatch = dispatch_cook_follow_up(
            &options,
            executor.clone(),
            cook_id,
            adopted_attempt,
            &record.run_id,
            &plan,
            &aggregate,
            &promotion,
            follow_up_request,
            true,
            &budget,
            super::cook_budget::execution_budget_usage(&aggregate),
            &mut remediation_usage,
        )?;
        return match dispatch {
            CookFollowUpDispatch::Dispatched { run_id } => {
                agent_task_lifecycle::finish_candidate_adoption(&record.run_id, None)?;
                options.initial_plan = super::load_recipe(cook_id)?
                    .attempts
                    .into_iter()
                    .find(|attempt| attempt.run_id == run_id)
                    .map(|attempt| attempt.plan)
                    .ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "cook_recipe.attempts",
                            "dispatched candidate remediation is missing from the durable cook recipe",
                            Some(run_id.clone()),
                            None,
                        )
                    })?;
                options.initial_run_id = run_id;
                let mut result = super::cook::run_cook_with_finalizer(
                    options,
                    executor,
                    |options, run_id, promotion| {
                        finalize_or_load_cook_pr_with_backend(options, run_id, promotion, backend)
                    },
                )?;
                result.value.attempts.insert(0, attempt);
                Ok(result)
            }
            CookFollowUpDispatch::BudgetExhausted { reason } => {
                let report = cook_report(
                    cook_id.to_string(),
                    "execution_budget_exhausted",
                    vec![attempt],
                    None,
                    Some(format!(
                        "provider execution stopped because {reason} was exhausted"
                    )),
                    1,
                );
                persist_adoption_terminal_result(&record.run_id, &report.value)?;
                agent_task_lifecycle::finish_candidate_adoption(
                    &record.run_id,
                    Some("candidate remediation budget exhausted".to_string()),
                )?;
                Ok(report)
            }
            CookFollowUpDispatch::PolicyFailure { reason } => {
                let report = cook_report(
                    cook_id.to_string(),
                    "policy_failure",
                    vec![attempt],
                    None,
                    Some(reason.clone()),
                    1,
                );
                persist_adoption_terminal_result(&record.run_id, &report.value)?;
                agent_task_lifecycle::finish_candidate_adoption(&record.run_id, Some(reason))?;
                Ok(report)
            }
        };
    }
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
            let baseline = match (|| {
                let baseline_runtime = homeboy_core::engine::invocation::InvocationGuard::acquire(
                    &baseline_run_dir,
                    &homeboy_core::engine::invocation::InvocationRequirements::default(),
                )?;
                run_gate_command_with_timeout(
                    &baseline_path,
                    index + 1,
                    &command,
                    gate.visibility,
                    gate.reveal_policy,
                    &baseline_runtime.context().tmp_dir,
                    std::time::Duration::from_secs(5 * 60),
                )
            })() {
                Ok(baseline) => baseline,
                Err(error) => {
                    baseline_run_dir.finish(false);
                    return Err(error);
                }
            };
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
            baseline_run_dir.finish(true);
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

pub(crate) fn candidate_adoption_source(
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

pub(crate) fn concrete_adoption_ai_model(value: &str) -> Result<String> {
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
pub(crate) fn resolve_adoption_target(
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
