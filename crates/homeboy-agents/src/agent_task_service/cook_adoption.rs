//! Agent-task cook candidate adoption.
//!
//! Extracted from `cook.rs`: the `adopt_cook_candidate*` family that admits an
//! externally prepared immutable commit into a durable cook, plus the adoption
//! resolution helpers (`resolve_cook_adoption_attempt_in_store`/`resolve_adoption_target`/
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
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::resolve_candidate_revision;
use crate::agent_task_promotion::{
    promote_with_checkpoint_in_observation_store, AgentTaskPromotionOptions,
    AgentTaskPromotionReport,
};
use crate::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use crate::agent_task_scheduler::SharedAgentTaskExecutor;
use homeboy_core::cook_status::CookDisposition;
use homeboy_core::{Error, Result};

use super::cook::{
    dispatch_cook_follow_up, gate_feedback_current_diff, AgentTaskCandidateAdoptionOptions,
    AgentTaskCookAttemptDispatcher, AgentTaskCookAttemptReport, AgentTaskCookReport,
    CookFollowUpBudgetScope, CookFollowUpDispatch,
};
use super::cook_promotion::{
    cook_report, finalize_or_load_cook_pr_with_backend_with_stores,
    persisted_promotion_for_attempt_in_store, promotion_source_in_store, CookReportInput,
};
use super::cook_recipe::CookRecipeStore;
use super::AgentTaskRunResult;

#[derive(serde::Serialize, serde::Deserialize)]
struct CandidateAdoptionTerminalResult {
    status: String,
    stop_reason: Option<String>,
}

fn persist_adoption_terminal_result(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    report: &AgentTaskCookReport,
) -> Result<()> {
    lifecycle_store.record_candidate_adoption_result(
        run_id,
        serde_json::to_value(CandidateAdoptionTerminalResult {
            status: report.status.clone(),
            stop_reason: report.stop_reason.clone(),
        })
        .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
}

fn legacy_adoption_budget_failure(
    recipe: &super::AgentTaskCookRecipe,
    source_run_id: &str,
    result: Option<&Value>,
) -> bool {
    result.is_some_and(|result| result["status"] == "execution_budget_exhausted")
        && !recipe.attempts.iter().any(|attempt| {
            attempt.plan.tasks.iter().any(|task| {
                task.inputs["cook_loop"]["artifact_provenance"]["source_run_id"].as_str()
                    == Some(source_run_id)
                    && task.inputs["cook_loop"]["execution_budget_authority"]["kind"]
                        == "candidate_adoption_review"
            })
        })
}

/// Read the AI-authored review form off an adopted candidate's terminal
/// outcome. The candidate was produced by an earlier cook attempt, so any form
/// the original agent emitted is recorded on its aggregate. Absent/invalid here
/// re-triggers the review-form gate exactly as it would for a fresh cook.
fn adopted_review_form(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    allow_missing_aggregate: bool,
) -> Result<Option<crate::agent_task_review_dossier::AiFilledReviewForm>> {
    let aggregate = match lifecycle_store.read_aggregate(run_id) {
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
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

/// Adopt a candidate and retain the normal Cook execution boundary for any
/// remediation requested by its deterministic feedback.
pub fn adopt_cook_candidate_with_options_dispatcher_and_executor(
    cook_or_run_id: &str,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    adopt_cook_candidate_with_options_dispatcher_and_executor_for_attempt(
        cook_or_run_id,
        None,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        executor,
    )
}

/// Adopt a candidate against an explicit numbered Cook attempt.
pub fn adopt_cook_candidate_with_options_dispatcher_and_executor_for_attempt(
    cook_or_run_id: &str,
    attempt: Option<u32>,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    adopt_cook_candidate_with_dispatcher_and_backend_for_attempt(
        cook_or_run_id,
        attempt,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        executor,
        &mut RealAgentTaskPrFinalizationBackend,
    )
}

pub(crate) fn adopt_cook_candidate_with_dispatcher_and_backend<
    B: AgentTaskPrFinalizationBackend,
>(
    cook_or_run_id: &str,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: SharedAgentTaskExecutor,
    backend: &mut B,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    adopt_cook_candidate_with_dispatcher_and_backend_for_attempt(
        cook_or_run_id,
        None,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        executor,
        backend,
    )
}

pub(crate) fn adopt_cook_candidate_with_dispatcher_and_backend_for_attempt<
    B: AgentTaskPrFinalizationBackend,
>(
    cook_or_run_id: &str,
    attempt: Option<u32>,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: SharedAgentTaskExecutor,
    backend: &mut B,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    adopt_cook_candidate_with_dispatcher_and_backend_for_attempt_with_stores(
        &recipe_store,
        &lifecycle_store,
        cook_or_run_id,
        attempt,
        candidate_ref,
        adoption,
        reconstruct_dispatcher,
        executor,
        backend,
    )
}

pub(crate) fn adopt_cook_candidate_with_dispatcher_and_backend_for_attempt_with_stores<
    B: AgentTaskPrFinalizationBackend,
>(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_or_run_id: &str,
    attempt: Option<u32>,
    candidate_ref: &str,
    adoption: AgentTaskCandidateAdoptionOptions,
    reconstruct_dispatcher: impl FnOnce(
        &Value,
    ) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    executor: SharedAgentTaskExecutor,
    backend: &mut B,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    super::cook::validate_cook_follow_up_stores(recipe_store, lifecycle_store)?;
    let (record, recipe) = resolve_adoption_target_with_attempt_in_stores(
        recipe_store,
        lifecycle_store,
        cook_or_run_id,
        attempt,
    )?;
    let cook_id = &recipe.cook_id;
    let mut options = super::reconstruct_adoption_options(&recipe)?;
    // Adoption is a separate explicit authorization. The durable recipe keeps
    // its historical policy, while this invocation may accept only normalized
    // immutable-baseline matches produced below.
    options.gates.accept_inherited_failures = adoption.accept_inherited_failures;
    let run_id = record.run_id.clone();
    let plan = lifecycle_store.read_controller_plan(&run_id)?;
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
    let (source, source_path, recovery) =
        candidate_adoption_source_in_store(lifecycle_store, &record, &source_request)?;
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
    let persisted_adoption_result = record
        .candidate_adoption
        .as_ref()
        .and_then(|adoption| adoption.result.as_ref());
    if !options.gates.rerun_completed_gates
        && record.candidate_adoption.as_ref().is_some_and(|adoption| {
            adoption.candidate_sha == candidate_sha
                && adoption.ai_model == adoption_ai_model
                && (adoption.state == "completed" || adoption.result.is_some())
        })
        // Pre-authority budget failures may enter the repaired path once. The
        // failed attempt is archived when the new adoption starts.
        && !legacy_adoption_budget_failure(&recipe, &run_id, persisted_adoption_result)
    {
        let persisted_promotion =
            persisted_promotion_for_attempt_in_store(lifecycle_store, &record.run_id)?;
        let inherited_failure_requires_authorization =
            persisted_promotion.as_ref().is_some_and(|promotion| {
                promotion.deterministic_gates.iter().any(|gate| {
                    gate.status
                        == crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
                })
            });
        if let Some(result) = record
            .candidate_adoption
            .as_ref()
            .and_then(|adoption| adoption.result.clone())
        {
            let result: CandidateAdoptionTerminalResult = serde_json::from_value(result)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            if inherited_failure_requires_authorization && !adoption.accept_inherited_failures {
                return Ok(cook_report(CookReportInput {
                    cook_id: cook_id.to_string(),
                    status: "baseline_red",
                    disposition: CookDisposition::Terminal,
                    attempts: vec![AgentTaskCookAttemptReport {
                        attempt: 1,
                        run_id: record.run_id.clone(),
                        run_state: format!("{:?}", record.state),
                        aggregate_path: record.aggregate_path.clone(),
                        promotion: persisted_promotion,
                        feedback: None,
                    }],
                    finalization: None,
                    stop_reason: Some(
                        "completed adoption contains accepted inherited baseline-red gate evidence; rerun adopt with --accept-inherited-failures to reauthorize it"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(record.run_id.as_str()),
                }));
            }
            let exit_code = if matches!(
                result.status.as_str(),
                "review_ready" | "draft_published" | "green_no_finalize"
            ) {
                0
            } else {
                1
            };
            // Replaying an already-completed adoption is exactly the path an
            // orchestrator hits when it re-polls a Cook it already ran, so it
            // is the path that must not fall back to the cross-invocation Cook
            // index. Every sibling return in this file already reports the
            // adopted record's run id; this one did not.
            return Ok(cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status: &result.status,
                disposition: CookDisposition::Terminal,
                attempts: vec![AgentTaskCookAttemptReport {
                    attempt: adopted_attempt,
                    run_id: record.run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path.clone(),
                    promotion: persisted_promotion,
                    feedback: None,
                }],
                finalization: None,
                stop_reason: result.stop_reason,
                exit_code,
                invocation_latest_run_id: Some(record.run_id.as_str()),
            }));
        }
        let promotion = persisted_promotion_for_attempt_in_store(lifecycle_store, &record.run_id)?
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "candidate_ref",
                    "completed candidate adoption is missing its persisted promotion result",
                    Some(candidate_sha.clone()),
                    None,
                )
            })?;
        if inherited_failure_requires_authorization && !adoption.accept_inherited_failures {
            return Ok(cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status: "baseline_red",
                disposition: CookDisposition::Terminal,
                attempts: vec![AgentTaskCookAttemptReport {
                    attempt: 1,
                    run_id: record.run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path.clone(),
                    promotion: Some(promotion),
                    feedback: None,
                }],
                finalization: None,
                stop_reason: Some(
                    "completed adoption contains accepted inherited baseline-red gate evidence; rerun adopt with --accept-inherited-failures to reauthorize it"
                        .to_string(),
                ),
                exit_code: 1,
                invocation_latest_run_id: Some(record.run_id.as_str()),
            }));
        }
        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt: adopted_attempt,
            max_attempts: options.max_attempts,
            source_run_id: Some(record.run_id.clone()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: adopted_review_form(lifecycle_store, &record.run_id, recovery.is_some())?,
            metadata: serde_json::json!({"adopted_candidate_ref": candidate_ref}),
        });
        let finalization = record.metadata.get("cook_finalization").cloned();
        let status = finalization
            .as_ref()
            .and_then(|value| value["status"].as_str())
            .unwrap_or("green_no_finalize")
            .to_string();
        return Ok(cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: &status,
            disposition: CookDisposition::Terminal,
            attempts: vec![AgentTaskCookAttemptReport {
                attempt: adopted_attempt,
                run_id: record.run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path.clone(),
                promotion: Some(promotion),
                feedback: Some(feedback),
            }],
            finalization,
            stop_reason: Some(
                "reused the completed candidate adoption result; set \
     rerun_completed_gates to rerun its gates"
                    .to_string(),
            ),
            exit_code: if matches!(
                status.as_str(),
                "review_ready" | "draft_published" | "green_no_finalize"
            ) {
                0
            } else {
                1
            },
            invocation_latest_run_id: Some(record.run_id.as_str()),
        }));
    }
    let attempt_dispatcher =
        reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
    options.attempt_dispatcher = attempt_dispatcher;
    lifecycle_store.start_candidate_adoption_with_policy(
        &record.run_id,
        &candidate_sha,
        &adoption_ai_model,
        &gate_identity,
        options.gates.rerun_completed_gates,
        adoption.replace_interrupted,
    )?;
    let gate_run_id = record.run_id.clone();
    let gate_lifecycle_store = lifecycle_store.clone();
    let promotion =
        match reusable_applied_adoption_promotion(lifecycle_store, &record, &candidate_sha) {
            Some(promotion) => promotion,
            None => crate::agent_task_promotion::with_gate_supervision(
                crate::agent_task_gate::GateSupervision {
                    timeout: options.gates.gate_timeout(),
                    no_progress_timeout: options.gates.gate_no_progress_timeout(),
                    heartbeat_interval: options.gates.gate_heartbeat_interval(),
                    on_spawn: Arc::new({
                        let run_id = gate_run_id.clone();
                        let lifecycle_store = gate_lifecycle_store.clone();
                        move |pid, command| {
                            lifecycle_store.start_candidate_adoption_gate(
                                &run_id,
                                command,
                                pid,
                                options.gates.gate_timeout_seconds,
                            )
                        }
                    }),
                    on_heartbeat: Arc::new({
                        let run_id = gate_run_id.clone();
                        let lifecycle_store = gate_lifecycle_store.clone();
                        move |status| {
                            lifecycle_store.heartbeat_candidate_adoption_gate(
                                &run_id,
                                status.visibility,
                                status.reveal_policy,
                                status,
                            )
                        }
                    }),
                    is_cancelled: Arc::new({
                        let lifecycle_store = gate_lifecycle_store.clone();
                        move || {
                            lifecycle_store
                                .candidate_adoption_cancel_requested(&gate_run_id)
                                .unwrap_or(false)
                        }
                    }),
                },
                || {
                    let observation_store = lifecycle_store.open_observation_initialized()?;
                    promote_with_checkpoint_in_observation_store(
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
                        &observation_store,
                        |checkpoint| {
                            let checkpoint = serde_json::to_value(checkpoint).map_err(|error| {
                                Error::internal_json(
                                    error.to_string(),
                                    Some("serialize adopted candidate checkpoint".to_string()),
                                )
                            })?;
                            lifecycle_store.checkpoint_candidate_adoption(
                                &record.run_id,
                                "post_apply_verification",
                                &gate_identity,
                            )?;
                            lifecycle_store
                                .record_promotion(&record.run_id, checkpoint)
                                .map(|_| ())
                        },
                    )
                },
            ),
        };
    let mut promotion = match promotion {
        Ok(promotion) => promotion,
        Err(error) => {
            lifecycle_store
                .finish_candidate_adoption(&record.run_id, Some(error.message.clone()))?;
            return Err(error);
        }
    };
    if lifecycle_store.candidate_adoption_cancel_requested(&record.run_id)? {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "candidate adoption was cancelled before baseline verification",
            Some(record.run_id.clone()),
            None,
        ));
    }
    let candidate_base_sha = promotion.provenance["base_ref"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.base_ref",
                "candidate adoption requires the resolved immutable candidate base for baseline-aware verification",
                None,
                None,
            )
        })?;
    super::cook_baseline::compare_gate_failures_to_verified_base(
        &mut promotion,
        &source_worktree,
        &candidate_base_sha,
        options.gates.gate_timeout(),
        |compared, total| {
            lifecycle_store.checkpoint_candidate_adoption(
                &record.run_id,
                "baseline_verification",
                &format!("baseline gate {compared}/{total}"),
            )
        },
    )?;
    if lifecycle_store.candidate_adoption_cancel_requested(&record.run_id)? {
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
        "candidate_base": candidate_base_sha,
        "recovery": recovery,
        "ai_model": adoption_ai_model,
        "ai_model_source": ai_model_source,
    });
    let promotion_value = serde_json::to_value(&promotion)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    lifecycle_store.record_promotion(&record.run_id, promotion_value)?;
    let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
        source_request,
        promotion_report: promotion.clone(),
        attempt: adopted_attempt,
        max_attempts: options.max_attempts,
        source_run_id: Some(record.run_id.clone()),
        current_diff: gate_feedback_current_diff(&promotion),
        require_review_form: true,
        review_form: adopted_review_form(lifecycle_store, &record.run_id, recovery.is_some())?,
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
            lifecycle_store.finish_candidate_adoption(
                &record.run_id,
                Some(
                    "candidate adoption feedback requested retry without a follow-up request"
                        .to_string(),
                ),
            )?;
            let report = cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status: "policy_failure",
                disposition: CookDisposition::Terminal,
                attempts: vec![attempt],
                finalization: None,
                stop_reason: Some(
                    "candidate adoption feedback requested retry without a follow-up request"
                        .to_string(),
                ),
                exit_code: 1,
                invocation_latest_run_id: Some(record.run_id.as_str()),
            });
            persist_adoption_terminal_result(lifecycle_store, &record.run_id, &report.value)?;
            return Ok(report);
        };
        // An authenticated pre-provider recovery has no aggregate executor
        // evidence. The concrete adopted model is the authority for the
        // remediation request and makes its same-provider budget category
        // explicit rather than inferred from an absent provider execution.
        follow_up_request.executor.model = Some(adoption_ai_model.clone());
        let budget = plan.options.execution_budget.clone();
        let mut remediation_usage = Default::default();
        let aggregate = match lifecycle_store.read_aggregate(&record.run_id) {
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
        if !lifecycle_store.matches_current_environment()? {
            let message = "candidate adoption remediation requires the explicit lifecycle store to match the current Cook runtime root";
            lifecycle_store.finish_candidate_adoption(&record.run_id, Some(message.to_string()))?;
            return Err(Error::validation_invalid_argument(
                "stores",
                message,
                Some(lifecycle_store.data_root().display().to_string()),
                None,
            ));
        }
        let dispatch = dispatch_cook_follow_up(
            (recipe_store, lifecycle_store),
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
            CookFollowUpBudgetScope::CandidateAdoptionReview,
            &budget,
            super::cook_budget::execution_budget_usage(&aggregate),
            &mut remediation_usage,
        )?;
        return match dispatch {
            CookFollowUpDispatch::Dispatched { run_id } => {
                lifecycle_store.finish_candidate_adoption(&record.run_id, None)?;
                options.initial_plan = recipe_store.load_recipe(cook_id)?
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
                let mut result = super::cook::run_cook_with_finalizer_with_store(
                    recipe_store,
                    options,
                    executor,
                    |options, run_id, promotion| {
                        finalize_or_load_cook_pr_with_backend_with_stores(
                            recipe_store,
                            lifecycle_store,
                            options,
                            run_id,
                            promotion,
                            backend,
                        )
                    },
                )?;
                result.value.attempts.insert(0, attempt);
                Ok(result)
            }
            CookFollowUpDispatch::BudgetExhausted { reason } => {
                let report = cook_report(CookReportInput {
                    cook_id: cook_id.to_string(),
                    status: "execution_budget_exhausted",
                    disposition: CookDisposition::Terminal,
                    attempts: vec![attempt],
                    finalization: None,
                    stop_reason: Some(super::cook::exhausted_budget_guidance(
                        options.max_attempts,
                        &budget,
                        &reason,
                        true,
                    )),
                    exit_code: 1,
                    invocation_latest_run_id: Some(record.run_id.as_str()),
                });
                persist_adoption_terminal_result(lifecycle_store, &record.run_id, &report.value)?;
                lifecycle_store.finish_candidate_adoption(
                    &record.run_id,
                    Some("candidate remediation budget exhausted".to_string()),
                )?;
                Ok(report)
            }
            CookFollowUpDispatch::PolicyFailure { reason } => {
                let report = cook_report(CookReportInput {
                    cook_id: cook_id.to_string(),
                    status: "policy_failure",
                    disposition: CookDisposition::Terminal,
                    attempts: vec![attempt],
                    finalization: None,
                    stop_reason: Some(reason.clone()),
                    exit_code: 1,
                    invocation_latest_run_id: Some(record.run_id.as_str()),
                });
                persist_adoption_terminal_result(lifecycle_store, &record.run_id, &report.value)?;
                lifecycle_store.finish_candidate_adoption(&record.run_id, Some(reason))?;
                Ok(report)
            }
        };
    }
    if feedback.status == AgentTaskCookLoopStatus::BaselineRed {
        if options.gates.accept_inherited_failures && promotion.finalization_eligible(true) {
            // Continue directly to finalization below; this adoption already has
            // durable baseline proof and must not spend another provider attempt.
        } else {
            let reason = "candidate and immutable baseline failed the same required gate; repair the inherited infrastructure or gate environment before retrying adoption";
            lifecycle_store.finish_candidate_adoption(&record.run_id, Some(reason.to_string()))?;
            return Ok(cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status: "baseline_red",
                disposition: CookDisposition::Terminal,
                attempts: vec![attempt],
                finalization: None,
                stop_reason: Some(reason.to_string()),
                exit_code: 1,
                invocation_latest_run_id: Some(record.run_id.as_str()),
            }));
        }
    }
    if feedback.status != AgentTaskCookLoopStatus::GreenCompleted
        && !(feedback.status == AgentTaskCookLoopStatus::BaselineRed
            && options.gates.accept_inherited_failures
            && promotion.finalization_eligible(true))
    {
        lifecycle_store.finish_candidate_adoption(
            &record.run_id,
            Some("adopted candidate did not pass the original deterministic gates".to_string()),
        )?;
        return Ok(cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "gate_failed",
            disposition: CookDisposition::Terminal,
            attempts: vec![attempt],
            finalization: None,
            stop_reason: Some(
                "adopted candidate did not pass the original deterministic gates".to_string(),
            ),
            exit_code: 1,
            invocation_latest_run_id: Some(record.run_id.as_str()),
        }));
    }
    if options.no_finalize {
        lifecycle_store.finish_candidate_adoption(&record.run_id, None)?;
        return Ok(cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "green_no_finalize",
            disposition: CookDisposition::Terminal,
            attempts: vec![attempt],
            finalization: None,
            stop_reason: Some(
                "adopted candidate passed deterministic gates; recipe skips finalization"
                    .to_string(),
            ),
            exit_code: 0,
            invocation_latest_run_id: Some(record.run_id.as_str()),
        }));
    }
    lifecycle_store.checkpoint_candidate_adoption(
        &record.run_id,
        "finalization",
        "finalize pull request",
    )?;
    let mut finalization = match finalize_or_load_cook_pr_with_backend_with_stores(
        recipe_store,
        lifecycle_store,
        &options,
        &record.run_id,
        &promotion,
        backend,
    ) {
        Ok(finalization) => finalization,
        Err(error) => {
            lifecycle_store
                .finish_candidate_adoption(&record.run_id, Some(error.message.clone()))?;
            return Err(error);
        }
    };
    project_execution_placement(&mut finalization, &record.metadata);
    lifecycle_store.finish_candidate_adoption(&record.run_id, None)?;
    let status = finalization["status"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let exit_code = if matches!(status.as_str(), "review_ready" | "draft_published") {
        0
    } else {
        1
    };
    Ok(cook_report(CookReportInput {
        cook_id: cook_id.to_string(),
        status: &status,
        disposition: CookDisposition::Terminal,
        attempts: vec![attempt],
        finalization: Some(finalization),
        stop_reason: None,
        exit_code,
        invocation_latest_run_id: Some(record.run_id.as_str()),
    }))
}

/// PR/finalization evidence is a projection of the durable decision, never a
/// fresh interpretation of runner or environment metadata.
fn project_execution_placement(finalization: &mut Value, metadata: &Value) {
    if let Some(decision) = metadata.get("execution_placement_decision") {
        finalization["execution_placement_decision"] = decision.clone();
    }
    if let Some(outcome) = metadata.get("execution_placement_outcome") {
        finalization["execution_placement_outcome"] = outcome.clone();
    }
}

fn reusable_applied_adoption_promotion(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    candidate_sha: &str,
) -> Option<Result<AgentTaskPromotionReport>> {
    let promotion = match persisted_promotion_for_attempt_in_store(lifecycle_store, &record.run_id)
    {
        Ok(Some(promotion))
            if promotion.status
                == crate::agent_task_promotion::AgentTaskPromotionStatus::Applied =>
        {
            promotion
        }
        Ok(_) => return None,
        Err(error) => return Some(Err(error)),
    };
    if record
        .candidate_adoption
        .as_ref()
        .is_none_or(|adoption| adoption.candidate_sha != candidate_sha)
    {
        return None;
    }
    let Some(worktree_path) = promotion.target.path.as_deref() else {
        return Some(Err(Error::validation_invalid_argument(
            "latest_promotion.target.path",
            "applied candidate adoption has no controller-recorded destination path",
            Some(record.run_id.clone()),
            None,
        )));
    };
    let baseline = promotion.provenance.get("gate_feedback_baseline").cloned();
    let Some(mut baseline) = baseline else {
        return Some(Err(Error::validation_invalid_argument(
            "latest_promotion",
            "applied candidate adoption has no authenticated gate-feedback baseline",
            Some(record.run_id.clone()),
            None,
        )));
    };
    if baseline
        .get("current_diff")
        .and_then(Value::as_str)
        .is_some_and(|diff| diff.trim().is_empty())
    {
        return Some(
            crate::agent_task_candidate_baseline::validate_immutable_candidate_tree(
                std::path::Path::new(worktree_path),
                candidate_sha,
            )
            .map(|_| promotion),
        );
    }
    // The checkpoint records the complete candidate diff; bind it to the exact
    // promoted artifact before handing it to the shared dirty-destination check.
    baseline["patch_artifact"] = match serde_json::to_value(&promotion.patch_artifact) {
        Ok(artifact) => artifact,
        Err(error) => {
            return Some(Err(Error::internal_json(
                error.to_string(),
                Some("serialize persisted promotion artifact baseline".to_string()),
            )));
        }
    };
    Some(
        crate::agent_task_candidate_baseline::validate_gate_feedback_candidate_baseline(
            std::path::Path::new(worktree_path),
            &baseline,
        )
        .map(|_| promotion),
    )
}

pub(crate) fn candidate_adoption_source(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    source_request: &AgentTaskRequest,
) -> Result<(String, Option<PathBuf>, Option<Value>)> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    candidate_adoption_source_in_store(&lifecycle_store, record, source_request)
}

fn candidate_adoption_source_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
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
    if let Ok((source, path)) = lifecycle_store.aggregate_source_exact(&record.run_id) {
        return Ok((source, Some(path), None));
    }
    let (source, path) = promotion_source_in_store(lifecycle_store, &record.run_id)?;
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
    resolve_adoption_target_with_attempt(cook_or_run_id, None)
}

/// Resolve an adoption target, optionally selecting a numbered attempt from a
/// durable Cook recipe. The selector is needed when attempt one shares its ID
/// with the logical Cook and later attempts have different policies.
pub(crate) fn resolve_adoption_target_with_attempt(
    cook_or_run_id: &str,
    selected_attempt: Option<u32>,
) -> Result<(
    agent_task_lifecycle::AgentTaskRunRecord,
    super::AgentTaskCookRecipe,
)> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_or_run_id,
        selected_attempt,
    )
}

/// Visible to the rest of `agent_task_service` so Cook's tests can resolve an
/// adoption target through explicit roots. The ambient wrapper above is the
/// only production caller; widening this to `pub(super)` keeps the rooted seam
/// reachable without giving tests a second, ambient-resolving entry point
/// (#7505).
pub(super) fn resolve_adoption_target_with_attempt_in_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_or_run_id: &str,
    selected_attempt: Option<u32>,
) -> Result<(
    agent_task_lifecycle::AgentTaskRunRecord,
    super::AgentTaskCookRecipe,
)> {
    let declared_attempt_recipe = recipe_store.load_recipe_for_attempt(cook_or_run_id)?;
    // A durable Cook id names its immutable recipe, not whichever attempt the
    // mutable Cook index most recently observed. Resolve recipes before run
    // records so a later failed attempt cannot steal adoption ownership from
    // the original equivalent source attempt.
    if recipe_store.recipe_exists(cook_or_run_id) {
        let recipe = recipe_store.load_recipe(cook_or_run_id)?;
        if let Some(attempt_recipe) = &declared_attempt_recipe {
            if attempt_recipe.cook_id != recipe.cook_id {
                return Err(Error::validation_invalid_argument(
                    "run_or_cook_id",
                    format!(
                        "identifier is both durable Cook id `{}` and declared attempt run id of Cook `{}`; select an unambiguous Cook or attempt id",
                        recipe.cook_id, attempt_recipe.cook_id
                    ),
                    Some(cook_or_run_id.to_string()),
                    None,
                ));
            }
        }
        let attempt = match selected_attempt {
            Some(attempt_number) => recipe
                .attempts
                .iter()
                .find(|attempt| attempt.attempt == attempt_number)
                .ok_or_else(|| {
                    let eligible = recipe
                        .attempts
                        .iter()
                        .map(|attempt| attempt.attempt.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Error::validation_invalid_argument(
                        "attempt",
                        format!(
                            "candidate adoption Cook has no attempt {attempt_number}; eligible attempts: {eligible}"
                        ),
                        Some(attempt_number.to_string()),
                        None,
                    )
                })?,
            None => resolve_cook_adoption_attempt_in_store(lifecycle_store, &recipe)?,
        };
        if lifecycle_store.record_exists(&attempt.run_id)? {
            return Ok((lifecycle_store.read_record(&attempt.run_id)?, recipe));
        }
        let run_id = attempt.run_id.clone();
        return materialize_adoption_attempt_in_stores(
            recipe_store,
            lifecycle_store,
            recipe,
            run_id,
        );
    }

    // Runner-side lifecycle projection can omit the controller's `cook_id`
    // metadata. Resolve an explicit attempt through durable recipe membership
    // before falling back to record metadata, otherwise the actionable exact
    // run ID emitted for an ambiguous Cook is misread as a recipe directory.
    if let Some(recipe) = declared_attempt_recipe {
        let attempt = match selected_attempt {
            Some(attempt_number) => recipe
                .attempts
                .iter()
                .find(|attempt| attempt.attempt == attempt_number)
                .ok_or_else(|| {
                    let eligible = recipe
                        .attempts
                        .iter()
                        .map(|attempt| attempt.attempt.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Error::validation_invalid_argument(
                        "attempt",
                        format!(
                            "candidate adoption Cook has no attempt {attempt_number}; eligible attempts: {eligible}"
                        ),
                        Some(attempt_number.to_string()),
                        None,
                    )
                })?,
            None => recipe
                .attempts
                .iter()
                .find(|attempt| attempt.run_id == cook_or_run_id)
                .expect("attempt lookup returns a recipe that declares the run id"),
        };
        if lifecycle_store.record_exists(&attempt.run_id)? {
            return Ok((lifecycle_store.read_record(&attempt.run_id)?, recipe));
        }
        let run_id = attempt.run_id.clone();
        return materialize_adoption_attempt_in_stores(
            recipe_store,
            lifecycle_store,
            recipe,
            run_id,
        );
    }

    if selected_attempt.is_some() {
        return Err(Error::validation_invalid_argument(
            "attempt",
            "candidate adoption --attempt requires a durable Cook id or its declared attempt run id",
            selected_attempt.map(|attempt| attempt.to_string()),
            None,
        ));
    }

    if lifecycle_store.record_exists(cook_or_run_id)? {
        let record = lifecycle_store.read_record(cook_or_run_id)?;
        let cook_id = record
            .metadata
            .get("cook_id")
            .and_then(Value::as_str)
            .unwrap_or(cook_or_run_id)
            .to_string();
        return Ok((record, recipe_store.load_recipe(&cook_id)?));
    }

    Err(Error::validation_invalid_argument(
        "run_or_cook_id",
        "unknown agent-task run or durable cook id",
        Some(cook_or_run_id.to_string()),
        None,
    ))
}

fn materialize_adoption_attempt_in_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: super::AgentTaskCookRecipe,
    run_id: String,
) -> Result<(
    agent_task_lifecycle::AgentTaskRunRecord,
    super::AgentTaskCookRecipe,
)> {
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .expect("selected adoption attempt remains in its recipe");
    let record =
        super::cook_pre_execution::CookExecutionPreparation::new(recipe_store, lifecycle_store)
            .recover_for_adoption_with_runtime(
                &recipe.cook_id,
                &attempt.run_id,
                Some(&|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok()),
                agent_task_lifecycle::execution_runner_id(),
                super::cook_pre_execution::production_runtime_admission(lifecycle_store),
                |cook_id| {
                    super::cook_pre_execution::reconcile_reserved_cancellation_in_store(
                        lifecycle_store,
                        cook_id,
                    )
                },
            )?;
    Ok((record, recipe))
}

// The ambient `resolve_cook_adoption_attempt()` shim that used to sit above
// this resolved a root and delegated straight here. It had no callers, so it
// was a resolution point that existed for nobody (#7505).

/// A retried cook may have several lifecycle attempts for the same immutable
/// plan. The earliest is the stable target; different plans require an explicit
/// run ID so a candidate is never attached to the wrong policy.
///
/// The candidate selection this reads and the recipe attempt it returns must
/// name one installation: selecting from an ambient store and then binding the
/// adopted attempt into an injected one silently attaches a candidate to the
/// wrong policy without failing.
fn resolve_cook_adoption_attempt_in_store<'a>(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &'a super::AgentTaskCookRecipe,
) -> Result<&'a super::AgentTaskCookRecipeAttempt> {
    if let Ok(selection) = lifecycle_store.select_cook_candidate(&recipe.cook_id) {
        if selection.reason
            != "no_substantive_candidate_evidence_preserve_latest_attempt_compatibility"
        {
            if let Some(attempt) = recipe
                .attempts
                .iter()
                .find(|attempt| attempt.run_id == selection.run_id)
            {
                return Ok(attempt);
            }
        }
    }
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
        .map(|attempt| attempt_adoption_policy_summary(recipe, attempt))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::validation_invalid_argument(
        "cook_recipe.attempts",
        format!(
            "candidate adoption by cook id is ambiguous because durable attempt plans differ ({attempts}); select the candidate's owning policy explicitly, for example `homeboy agent-task adopt {} --attempt {}`",
            recipe.cook_id, first.attempt
        ),
        Some(recipe.cook_id.clone()),
        Some(vec![
            "Pass --attempt N with the Cook id to select the candidate's exact recorded policy."
                .to_string(),
        ]),
    ))
}

/// Render only the fixed policy fields an operator needs to choose an attempt.
/// Executor config and gate commands can contain credentials, so diagnostics
/// report their semantics rather than their raw values.
fn attempt_adoption_policy_summary(
    recipe: &super::AgentTaskCookRecipe,
    attempt: &super::AgentTaskCookRecipeAttempt,
) -> String {
    let finalization = &recipe.finalization;
    let destination = compact_policy_value(finalization.get("to_worktree"));
    let base = compact_policy_value(finalization.get("base"));
    let head = compact_policy_value(finalization.get("head"));
    let task_base = compact_policy_value(finalization.get("task_base_sha"));
    let public_gates = recipe
        .gate_policy
        .get("verify")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let private_gates = recipe
        .gate_policy
        .get("private_verify")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let publication = if finalization
        .get("no_finalize")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "none".to_string()
    } else {
        format!(
            "review-ready/protected:{}",
            finalization
                .get("protected_branches")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        )
    };
    let provider_summaries = attempt
        .plan
        .tasks
        .iter()
        .take(3)
        .map(|task| {
            format!(
                "{}/{}@{}",
                compact_text(&task.executor.backend),
                compact_optional_text(task.executor.selector.as_deref()),
                compact_optional_text(task.executor.model.as_deref())
            )
        })
        .collect::<Vec<_>>();
    let providers = if attempt.plan.tasks.len() > provider_summaries.len() {
        format!(
            "{}+{} more",
            provider_summaries.join("+"),
            attempt.plan.tasks.len() - provider_summaries.len()
        )
    } else {
        provider_summaries.join("+")
    };
    let task_policy = attempt
        .plan
        .tasks
        .first()
        .map(|task| {
            format!(
                "{}/{}/{}",
                compact_text(&task.policy.read),
                compact_text(&task.policy.write),
                compact_text(&task.policy.apply)
            )
        })
        .unwrap_or_else(|| "none".to_string());
    homeboy_core::redaction::redact_string(&format!(
        "attempt {}: {} (plan {}; destination={destination}; base={base}; head={head}; task-base={task_base}; gates=public:{public_gates}/private:{private_gates}; provider/model={providers}; review/publication={publication}; task-policy={task_policy})",
        attempt.attempt,
        compact_text(&attempt.run_id),
        compact_text(&attempt.plan.plan_id),
    ))
}

fn compact_policy_value(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(compact_text)
        .unwrap_or_else(|| "none".to_string())
}

fn compact_optional_text(value: Option<&str>) -> String {
    value
        .map(compact_text)
        .unwrap_or_else(|| "default".to_string())
}

fn compact_text(value: &str) -> String {
    const MAX_CHARS: usize = 96;
    let mut compact = value.chars();
    let prefix = compact.by_ref().take(MAX_CHARS).collect::<String>();
    if compact.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn finalization_projects_the_exact_durable_placement_decision_and_outcome_ids() {
        let decision = serde_json::json!({
            "decision_id": "epd-durable-decision",
            "runner": { "runner_id": "fixture-lab", "source": "policy" }
        });
        let outcome = serde_json::json!({
            "decision_id": "epd-durable-decision",
            "effective": "lab",
            "runner_id": "fixture-lab"
        });
        let metadata = serde_json::json!({
            "execution_placement_decision": decision,
            "execution_placement_outcome": outcome
        });
        let mut finalization = serde_json::json!({ "status": "review_ready" });

        project_execution_placement(&mut finalization, &metadata);

        assert_eq!(
            finalization["execution_placement_decision"],
            metadata["execution_placement_decision"]
        );
        assert_eq!(
            finalization["execution_placement_outcome"],
            metadata["execution_placement_outcome"]
        );
        assert_eq!(
            finalization["execution_placement_decision"]["decision_id"],
            finalization["execution_placement_outcome"]["decision_id"]
        );
    }
}
