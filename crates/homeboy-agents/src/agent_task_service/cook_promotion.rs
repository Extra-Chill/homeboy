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
    candidate_fingerprint, promote_with_checkpoint, resume_promoted_patch,
    AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewEvidence, AgentTaskReviewTestStep,
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

pub(crate) fn is_moving_base_finalization_error(error: &Error) -> bool {
    error.code == homeboy_core::ErrorCode::ValidationInvalidArgument
        && error
            .message
            .contains("HEAD is behind or diverged from resolved base")
}

/// A controller-only continuation for a candidate whose declared destination
/// base advanced after its original deterministic gates completed green.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MovingBaseCookRecovery {
    pub schema: String,
    pub cook_id: String,
    pub run_id: String,
    pub promotion: AgentTaskPromotionReport,
    pub prior_verified_base: String,
    pub passed_gates: Value,
    pub blocker: String,
    pub continuation: String,
    #[serde(default)]
    pub base_movements: u32,
}

pub(crate) fn moving_base_recovery_for_run(run_id: &str) -> Result<Option<MovingBaseCookRecovery>> {
    agent_task_lifecycle::status(run_id)?
        .metadata
        .get("cook_moving_base_recovery")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "cook_moving_base_recovery",
                format!("invalid durable moving-base recovery: {error}"),
                Some(run_id.to_string()),
                None,
            )
        })
}

pub(crate) fn next_moving_base_recovery(
    mut recovery: MovingBaseCookRecovery,
    blocker: String,
) -> MovingBaseCookRecovery {
    recovery.base_movements = recovery.base_movements.saturating_add(1);
    // The exact authenticated destination changed outside this controller. It
    // is not a moving-base retry; retain the proof but never retry or rebase it.
    if blocker.contains("differs from the exact promoted candidate") {
        recovery.base_movements = 3;
    }
    recovery.blocker = blocker;
    recovery
}

pub(crate) fn moving_base_recovery_from_promotion(
    cook_id: &str,
    run_id: &str,
    promotion: AgentTaskPromotionReport,
) -> MovingBaseCookRecovery {
    MovingBaseCookRecovery {
        schema: "homeboy/agent-task-cook-moving-base-recovery/v1".to_string(),
        cook_id: cook_id.to_string(),
        run_id: run_id.to_string(),
        prior_verified_base: promotion
            .verified_base
            .as_ref()
            .map(|base| base.sha.clone())
            .unwrap_or_default(),
        passed_gates: serde_json::to_value(&promotion.gate_results).unwrap_or(Value::Null),
        promotion,
        blocker: String::new(),
        continuation: "homeboy agent-task run-next".to_string(),
        base_movements: 0,
    }
}

pub(crate) fn refreshed_moving_base_recovery(
    mut recovery: MovingBaseCookRecovery,
    promotion: &AgentTaskPromotionReport,
) -> MovingBaseCookRecovery {
    recovery.prior_verified_base = promotion
        .verified_base
        .as_ref()
        .map(|base| base.sha.clone())
        .unwrap_or_default();
    recovery.passed_gates = serde_json::to_value(&promotion.gate_results).unwrap_or(Value::Null);
    recovery.promotion = promotion.clone();
    recovery
}

pub(crate) fn moving_base_recovery_report(
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    recovery: MovingBaseCookRecovery,
    continuation_queued: bool,
    invocation_latest_run_id: Option<&str>,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let stop_reason = if recovery.base_movements >= 3 {
        Some(format!("moving-base recovery exhausted after {} refreshed base observations: {}; inspect the retained recovery evidence and reconcile the destination before retrying", recovery.base_movements, recovery.blocker))
    } else if !continuation_queued {
        Some(format!(
            "moving-base recovery stopped: {}; inspect the retained recovery evidence before retrying",
            recovery.blocker
        ))
    } else {
        Some(format!(
            "{}; continuation is queued without provider dispatch: {}",
            recovery.blocker, recovery.continuation
        ))
    };
    let mut report = cook_report(
        cook_id,
        "candidate_recoverable",
        attempts,
        None,
        stop_reason,
        1,
        invocation_latest_run_id,
    );
    report.value.moving_base_recovery = Some(recovery);
    report
}

/// Continue only the controller-owned half of a green Cook: authenticate the
/// original promoted candidate, pin a fresh destination base, rebase it, then
/// rebuild promotion/gate proof without returning to a provider.
pub(crate) fn recover_moving_base_cook_candidate(
    options: &AgentTaskCookServiceOptions,
    recovery: &MovingBaseCookRecovery,
) -> Result<AgentTaskPromotionReport> {
    if recovery.base_movements >= 3 {
        return Err(Error::validation_invalid_argument("base", "moving-base recovery budget is exhausted; inspect the retained recovery evidence before retrying", None, None));
    }
    let path = recovery
        .promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "moving-base recovery requires the authenticated promotion destination",
                None,
                None,
            )
        })?;
    if recovery.promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "moving-base recovery requires an applied promotion with green gates",
            None,
            None,
        ));
    }
    let expected = recovery
        .promotion
        .provenance
        .get("candidate")
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.candidate",
                "moving-base recovery requires the exact promoted candidate fingerprint",
                None,
                None,
            )
        })?;
    let expected = serde_json::from_value(expected).map_err(|_| {
        Error::validation_invalid_argument(
            "promotion.provenance.candidate",
            "moving-base recovery candidate fingerprint is invalid",
            None,
            None,
        )
    })?;
    if candidate_fingerprint(path)? != expected {
        return Err(Error::validation_invalid_argument("path", "moving-base recovery destination differs from the exact promoted candidate; refusing to rebase divergent content", Some(path.to_string()), None));
    }
    let fresh_base = observe_and_fetch_base(path, &options.base)?;
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !dirty.status.success() {
        return Err(Error::git_command_failed(
            "could not inspect moving-base recovery destination".to_string(),
        ));
    }
    if !dirty.stdout.is_empty() {
        let add = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !add.status.success() {
            return Err(Error::git_command_failed(
                String::from_utf8_lossy(&add.stderr).trim().to_string(),
            ));
        }
        let commit = homeboy_core::git::commit_at(
            None,
            Some(&options.commit_message),
            homeboy_core::git::CommitOptions::default(),
            Some(path),
        )?;
        if !commit.success {
            return Err(Error::git_command_failed(commit.stderr));
        }
    }
    let rebase = std::process::Command::new("git")
        .args(["rebase", &fresh_base])
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !rebase.status.success() {
        let _ = std::process::Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(path)
            .output();
        return Err(Error::validation_invalid_argument("base", format!("moving-base recovery rebase onto `{fresh_base}` failed; destination was left unchanged: {}", String::from_utf8_lossy(&rebase.stderr).trim()), None, None));
    }
    let mut checkpoint = serde_json::to_value(&recovery.promotion)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    checkpoint["status"] = serde_json::json!("verification_pending");
    checkpoint["verified_base"] = serde_json::json!({ "base": options.base, "sha": fresh_base });
    checkpoint["provenance"]["candidate"] = serde_json::to_value(candidate_fingerprint(path)?)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    checkpoint["provenance"]["resume_inputs"] = serde_json::json!({ "base_ref": options.base, "task_base_sha": options.task_base_sha, "candidate_ref": null });
    let (source, source_path) = promotion_source(&recovery.run_id)?;
    let refreshed = resume_promoted_patch(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some(recovery.run_id.clone()),
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
        std::path::Path::new(path),
        &checkpoint,
    )?;
    Ok(refreshed)
}

fn observe_and_fetch_base(path: &str, base: &str) -> Result<String> {
    let observed = std::process::Command::new("git")
        .args([
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{base}"),
        ])
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let sha = String::from_utf8_lossy(&observed.stdout)
        .split_whitespace()
        .next()
        .filter(|sha| !sha.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "base",
                format!("could not observe destination base `{base}` for moving-base recovery"),
                None,
                None,
            )
        })?
        .to_string();
    let fetched = std::process::Command::new("git")
        .args([
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "origin",
            &sha,
        ])
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !fetched.status.success() {
        return Err(Error::validation_invalid_argument(
            "base",
            format!("could not materialize refreshed destination base `{base}` at {sha}"),
            None,
            None,
        ));
    }
    Ok(sha)
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
                changed_public_contracts: Vec::new(),
                public_contract_evidence: None,
                lifecycle: crate::agent_task_lifecycle::status(successful_run_id)
                    .ok()
                    .map(|record| record.lifecycle),
            },
            ai_used_for: options.ai_used_for.clone(),
            review_dossier: cook_review_dossier(options, promotion, successful_run_id)?,
            review_profile: resolve_review_profile(&path)?,
            manual_finalization: false,
            protected_branches: options.protected_branches.clone(),
        },
        backend,
    )?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn cook_review_dossier(
    options: &AgentTaskCookServiceOptions,
    promotion: &AgentTaskPromotionReport,
    successful_run_id: &str,
) -> Result<AgentTaskReviewDossier> {
    let changed_files = promotion.changed_files.join(", ");
    let changed_file_count = promotion.changed_files.len();
    let gate_count = promotion.gate_results.len();
    let task_summary = options
        .initial_plan
        .tasks
        .iter()
        .find_map(|task| {
            task.instructions
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
        })
        .unwrap_or("No single-line task objective was retained in durable task evidence.");
    let adoption = promotion.provenance.get("adoption").is_some();
    let how_to_test = options
        .gates
        .verify
        .iter()
        .map(|command| {
            let matched = promotion.deterministic_gates.iter().any(|gate| {
                gate.status == crate::agent_task_gate::AgentTaskGateStatus::Succeeded
                    && gate.visibility == homeboy_core::gate::HomeboyGateVisibility::Visible
                    && gate.command.as_slice() == ["sh", "-lc", command]
            });
            if !matched {
                return Err(Error::validation_invalid_argument(
                    "verification",
                    "Cook cannot publish a test command without matching successful visible durable gate evidence",
                    Some(command.clone()),
                    None,
                ));
            }
            if !crate::agent_task_review_dossier::reviewer_runnable_command(command) {
                return Err(Error::validation_invalid_argument(
                    "verification",
                    "Cook cannot publish a test command containing an operator-only reference",
                    Some(command.clone()),
                    None,
                ));
            }
            Ok(AgentTaskReviewTestStep {
                command: command.clone(),
                expected: "passes as recorded by Cook's deterministic gate".to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // The AI-authored form is the sole source of the non-deterministic prose
    // (summary / what changed / compatibility / used_for). Finalization is only
    // reached after the cook loop's form gate accepted a valid form, so its
    // absence here is a hard invariant violation, not a soft fallback. Read it
    // after the deterministic gate-evidence validations so those failure modes
    // still surface first.
    let form = review_form_for_finalization(successful_run_id)?;
    Ok(AgentTaskReviewDossier {
        schema: "homeboy/agent-task-review-dossier/v1".to_string(),
        // Non-deterministic prose: authored by the AI form.
        summary: form.summary.clone(),
        what_changed: form.what_changed.clone(),
        how_to_test,
        compatibility: form.compatibility.clone(),
        // Deterministic evidence: orchestrator-owned. The task objective, scope,
        // gate count, and adoption provenance are factual records, not prose the
        // AI restates.
        evidence: vec![
            AgentTaskReviewEvidence {
                summary: format!("Task objective: {task_summary}"),
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: format!(
                    "Verified candidate scope: {changed_file_count} changed file(s): {changed_files}."
                ),
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: format!(
                    "Cook deterministic verification: {gate_count} gate(s) completed green."
                ),
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: if adoption {
                    "Candidate adoption provenance: an immutable candidate was adopted through the recorded Cook workflow and passed the recorded gates.".to_string()
                } else {
                    "Candidate adoption provenance: the candidate was promoted from the recorded Cook task execution.".to_string()
                },
                url: None,
            },
        ],
        changed_public_contracts: Vec::new(),
        public_contract_evidence: None,
        ai_assistance: AgentTaskReviewAiAssistance {
            // Deterministic: the orchestrator knows whether/what tool+model ran,
            // and attributes Homeboy as the harness that drove the change.
            used: true,
            tool: crate::agent_task_review_dossier::homeboy_tool_disclosure(&options.ai_tool),
            model: options
                .ai_model
                .clone()
                .unwrap_or_else(|| "not recorded".to_string()),
            // Non-deterministic: the AI's self-reflective process description.
            used_for: form.used_for.clone(),
        },
        source_relationships: Vec::new(),
        overrides: Vec::new(),
    })
}

/// Load and validate the AI-authored review form for a finalizing run.
///
/// The cook loop's review-form gate guarantees a valid form before finalization
/// is reached; this re-reads it from the terminal outcome as the single source
/// of the reviewer-facing prose. Its absence/invalidity here is an invariant
/// violation (the gate would have looped), surfaced as a hard error rather than
/// silently falling back to machine-templated prose.
fn review_form_for_finalization(
    run_id: &str,
) -> Result<crate::agent_task_review_dossier::AiFilledReviewForm> {
    let aggregate = crate::agent_task_lifecycle::read_aggregate(run_id)?;
    let form = aggregate
        .outcomes
        .last()
        .map(|outcome| {
            crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(
                &outcome.outputs,
            )
        })
        .transpose()?
        .flatten()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "review_form",
                format!(
                    "cook finalization requires an AI-authored review form on run {run_id}; none was recorded. {}",
                    crate::agent_task_review_dossier::AiFilledReviewForm::requirement_feedback()
                ),
                None,
                None,
            )
        })?;
    form.validate()?;
    Ok(form)
}

pub(crate) fn cook_report(
    cook_id: String,
    status: &str,
    attempts: Vec<AgentTaskCookAttemptReport>,
    finalization: Option<Value>,
    stop_reason: Option<String>,
    exit_code: i32,
    invocation_latest_run_id: Option<&str>,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let history_run_ids = agent_task_lifecycle::cook_index(&cook_id)
        .map(|index| {
            index
                .attempts
                .into_iter()
                .map(|attempt| attempt.run_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let latest_run_id = invocation_latest_run_id.map(str::to_string).or_else(|| {
        agent_task_lifecycle::cook_index(&cook_id)
            .ok()
            .map(|index| index.latest_run_id)
    });
    let invocation_run_ids: Vec<String> = attempts
        .iter()
        .map(|attempt| attempt.run_id.clone())
        .collect();
    AgentTaskRunResult {
        value: AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id,
            latest_run_id,
            history_run_ids,
            invocation_run_ids,
            status: status.to_string(),
            attempts,
            finalization,
            stop_reason,
            terminal_phase: None,
            terminal_failure_classification: None,
            moving_base_recovery: None,
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
