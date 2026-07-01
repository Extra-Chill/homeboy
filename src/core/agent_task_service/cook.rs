//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use std::path::PathBuf;

use crate::core::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::core::agent_task_finalization::{
    finalize_pr, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails,
    AgentTaskPrSourceRelationship, AgentTaskPrVerification,
};
use crate::core::agent_task_gate::VerifyGateOptions;
use crate::core::agent_task_lifecycle;
use crate::core::agent_task_promotion::{
    promote, AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::core::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};
use crate::core::{config, Error, Result};

use super::execution::run_loaded_plan;
use super::AgentTaskRunResult;

#[derive(Debug, Clone)]
pub struct AgentTaskCookServiceOptions {
    pub cook_id: String,
    pub initial_run_id: String,
    pub to_worktree: String,
    pub provider_command: Option<String>,
    /// Shared deterministic verification gate fields, factored out of the
    /// per-field duplication that previously spanned the loop/promote types.
    pub gates: VerifyGateOptions,
    pub max_attempts: u32,
    pub no_finalize: bool,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub source_refs: Vec<String>,
    pub protected_branches: Vec<String>,
    pub ai_tool: String,
    pub ai_model: Option<String>,
    pub ai_used_for: String,
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
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut run_id = options.initial_run_id.clone();
    let cook_id = options.cook_id.clone();

    for attempt in 1..=max_attempts {
        agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
        let record = agent_task_lifecycle::status(&run_id)?;
        let plan = agent_task_lifecycle::load_plan(&run_id)?;
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

        let promotion = match promote_attempt(&options, &run_id) {
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
                let finalization = finalize_cook_pr(&options, &cook_id, &promotion)?;
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
                    ));
                };
                let next_run_id = agent_task_lifecycle::cook_attempt_run_id(&cook_id, attempt + 1);
                let follow_up_plan = AgentTaskPlan::new(
                    format!("{cook_id}-cook-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                run_loaded_plan(follow_up_plan, Some(&next_run_id), executor.clone())?;
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
    promote(AgentTaskPromotionOptions {
        source,
        source_run_id: Some(run_id.to_string()),
        source_path,
        to_worktree: options.to_worktree.clone(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: options.gates.clone(),
        provider_command: options.provider_command.clone(),
    })
}

fn finalize_cook_pr(
    options: &AgentTaskCookServiceOptions,
    cook_id: &str,
    promotion: &AgentTaskPromotionReport,
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
            "homeboy://agent-task/run/{cook_id}"
        )))
        .collect();
    let artifact_refs = std::iter::once(promotion.patch_artifact.path.clone()).collect();
    let report = finalize_pr(AgentTaskPrFinalizationOptions {
        path,
        run_id: cook_id.to_string(),
        base: options.base.clone(),
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
            lifecycle: crate::core::agent_task_lifecycle::status(cook_id)
                .ok()
                .map(|record| record.lifecycle),
        },
        ai_used_for: options.ai_used_for.clone(),
        protected_branches: options.protected_branches.clone(),
    })?;
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
