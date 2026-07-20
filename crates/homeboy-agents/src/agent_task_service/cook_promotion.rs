//! Agent-task cook promotion & finalization.
//!
//! Extracted from `cook.rs`: promotion-source resolution
//! (`promotion_source`/`source_spec_path`/`source_worktree_path`), the durable
//! promote-or-load boundary (`promote_attempt`/`promote_or_load_attempt`/
//! `persisted_promotion_for_attempt`), PR finalization
//! (`finalize_or_load_cook_pr*`/`finalize_cook_pr_with_backend`), the
//! `cook_report` builder, and small spec helpers. These sit downstream of a
//! terminal provider result and publish controller-owned state; grouping them
//! keeps the promote → finalize boundary in one place.

use serde_json::Value;
use std::path::PathBuf;

use crate::agent_task_finalization::{
    finalize_pr_with_backend, AgentTaskPrEvidence, AgentTaskPrFinalizationBackend,
    AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship,
    AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{
    promote_with_checkpoint, AgentTaskPromotionOptions, AgentTaskPromotionReport,
    AgentTaskPromotionStatus,
};
use crate::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewTestStep,
};
use homeboy_core::{config, Error, Result};

use super::cook::{AgentTaskCookAttemptReport, AgentTaskCookReport, AgentTaskCookServiceOptions};
use super::AgentTaskRunResult;

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

pub(crate) fn promote_attempt(
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
pub(crate) fn promote_or_load_attempt(
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

pub(crate) fn persisted_promotion_for_attempt(
    run_id: &str,
) -> Result<Option<AgentTaskPromotionReport>> {
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

pub(crate) fn attempt_needs_execution(run_id: &str) -> bool {
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
pub(crate) fn finalize_or_load_cook_pr(
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

pub(crate) fn finalize_or_load_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
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

pub(crate) fn finalize_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
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

pub(crate) fn cook_report(
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
            terminal_phase: None,
            terminal_failure_classification: None,
        },
        exit_code,
    }
}

pub(crate) fn source_spec_path(spec: &str) -> Option<PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(PathBuf::from(spec.strip_prefix('@').unwrap_or(spec)))
}
