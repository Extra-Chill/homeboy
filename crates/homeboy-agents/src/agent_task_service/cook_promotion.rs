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

use serde_json::{json, Value};
use std::path::PathBuf;

use homeboy_core::cook_status::{CookDisposition, CookStatus};
use homeboy_core::engine::canonical_json::canonical_json_bytes;
use homeboy_engine_primitives::content_hash;
use homeboy_engine_primitives::shell::quote_args;

use crate::agent_task_finalization::{
    finalize_pr_with_backend, finalize_pr_with_backend_in_store, preflight_pr_with_backend,
    validate_publication_intent, AgentTaskPrEvidence, AgentTaskPrFinalizationBackend,
    AgentTaskPrFinalizationOptions, AgentTaskPrFinalizationReport, AgentTaskPrRuntimeGuardrails,
    AgentTaskPrSourceRelationship, AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{
    candidate_fingerprint, canonical_recoverable_patch_artifacts,
    canonical_recoverable_patch_artifacts_in_observation_store,
    promote_with_checkpoint_in_observation_store, resume_promoted_patch_in_observation_store,
    AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewEvidence, AgentTaskReviewOverride, AgentTaskReviewTestStep,
};
use homeboy_core::{config, Error, Result};

use super::cook::{
    canonical_candidate_finalization, canonical_cook_candidate, cook_finalization_is_pr_receipt,
    review_form_attempt_is_ready_for_cook_continuation, AgentTaskCookAttemptReport,
    AgentTaskCookReport, AgentTaskCookServiceOptions,
};
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

pub(crate) fn promotion_source_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<(String, Option<PathBuf>)> {
    lifecycle_store
        .aggregate_source_exact(run_id)
        .map(|(raw, path)| (raw, Some(path)))
}

pub(crate) fn promote_attempt(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    promote_attempt_in_store(&lifecycle_store, options, run_id)
}

pub(crate) fn promote_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let (source, source_path) = promotion_source_in_store(lifecycle_store, run_id)?;
    let selected_task_id = selected_candidate_task_id_in_store(lifecycle_store, run_id)?;
    let artifact_id = match continuation_artifact_id_in_store(lifecycle_store, run_id)? {
        Some(artifact_id) => Some(artifact_id),
        None => canonical_cook_patch_artifact_id_in_store(lifecycle_store, options, run_id)?,
    };
    let observation_store = lifecycle_store.open_observation_initialized()?;
    promote_with_checkpoint_in_observation_store(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some(run_id.to_string()),
            source_path,
            source_worktree_path: options.source_worktree_path.clone(),
            base_ref: Some(options.base.clone()),
            task_base_sha: options.task_base_sha.clone(),
            candidate_ref: None,
            to_worktree: options.to_worktree.clone(),
            task_id: selected_task_id,
            artifact_id,
            dry_run: false,
            gates: options.gates.clone(),
            provider_command: options.provider_command.clone(),
            provider_invocation: options.provider_invocation.clone(),
        },
        &observation_store,
        |checkpoint| {
            lifecycle_store.record_promotion(
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

/// Cook owns selection across provider rotations. Collapse equivalent artifact
/// aliases before promotion, but require an explicit operator choice for patches
/// that remain distinct after normalized content and provenance comparison.
pub(crate) fn canonical_cook_patch_artifact_id(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<Option<String>> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    canonical_cook_patch_artifact_id_in_store(&lifecycle_store, options, run_id)
}

fn canonical_cook_patch_artifact_id_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<Option<String>> {
    let (source, source_path) = promotion_source_in_store(lifecycle_store, run_id)?;
    let task_id = selected_candidate_task_id_in_store(lifecycle_store, run_id)?;
    let aggregate = lifecycle_store.read_aggregate(run_id)?;
    let Some(outcome) = aggregate
        .outcomes
        .iter()
        .find(|outcome| task_id.as_deref().is_none_or(|id| outcome.task_id == id))
    else {
        return Ok(None);
    };
    let promotion_options = AgentTaskPromotionOptions {
        source,
        source_run_id: Some(run_id.to_string()),
        source_path,
        source_worktree_path: options.source_worktree_path.clone(),
        base_ref: Some(options.base.clone()),
        task_base_sha: options.task_base_sha.clone(),
        candidate_ref: None,
        to_worktree: options.to_worktree.clone(),
        task_id,
        artifact_id: None,
        dry_run: false,
        gates: options.gates.clone(),
        provider_command: options.provider_command.clone(),
        provider_invocation: options.provider_invocation.clone(),
    };
    let observation_store = lifecycle_store.open_observation_initialized()?;
    let canonical = canonical_recoverable_patch_artifacts_in_observation_store(
        outcome,
        &promotion_options,
        &observation_store,
    )?;
    match canonical.artifacts.as_slice() {
        [] => Ok(None),
        [artifact] => Ok(Some(artifact.id.clone())),
        artifacts => {
            let choices = artifacts
                .iter()
                .map(|artifact| {
                    json!({
                        "artifact_id": artifact.id,
                        "sha256": artifact.sha256,
                        "command": cook_promotion_command(options, run_id, &outcome.task_id, &artifact.id),
                    })
                })
                .collect::<Vec<_>>();
            Err(Error::new(
                homeboy_core::ErrorCode::ValidationInvalidArgument,
                "Cook found distinct canonical patch candidates; select one before promotion",
                json!({
                    "field": "artifact_id",
                    "state": "selection_required",
                    "selection_required": true,
                    "choices": choices,
                }),
            ))
        }
    }
}

/// Render an explicit promotion command from Cook's durable execution contract.
/// Every gate and provider field is included so a manual choice cannot silently
/// fall back to CLI defaults that differ from the original Cook.
pub(crate) fn cook_promotion_command(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
) -> String {
    quote_args(&cook_promotion_argv(options, run_id, task_id, artifact_id))
}

/// The exact CLI argv required to reproduce Cook's promotion contract.
pub(crate) fn cook_promotion_argv(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
) -> Vec<String> {
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "promote".to_string(),
        run_id.to_string(),
        "--to-worktree".to_string(),
        options.to_worktree.clone(),
        "--base".to_string(),
        options.base.clone(),
        "--task-id".to_string(),
        task_id.to_string(),
        "--artifact-id".to_string(),
        artifact_id.to_string(),
    ];
    for (flag, values) in [
        ("--verify", &options.gates.verify),
        ("--private-verify", &options.gates.private_verify),
    ] {
        for value in values {
            command.extend([flag.to_string(), value.clone()]);
        }
    }
    let gates = serde_json::to_value(&options.gates).unwrap_or(Value::Null);
    for (key, flag) in [
        ("private_gate_reveal", "--private-gate-reveal"),
        ("execution_policy", "--gate-execution-policy"),
        ("gate_timeout_seconds", "--gate-timeout-seconds"),
        (
            "gate_heartbeat_interval_seconds",
            "--gate-heartbeat-interval-seconds",
        ),
        (
            "gate_no_progress_timeout_seconds",
            "--gate-no-progress-timeout-seconds",
        ),
    ] {
        if let Some(value) = gates.get(key) {
            let value = value
                .as_str()
                .map(|value| value.replace('_', "-"))
                .or_else(|| value.as_u64().map(|value| value.to_string()));
            if let Some(value) = value {
                command.extend([flag.to_string(), value]);
            }
        }
    }
    for (key, flag) in [
        ("rerun_completed_gates", "--rerun-completed-gates"),
        ("accept_inherited_failures", "--accept-inherited-failures"),
    ] {
        if gates.get(key).and_then(Value::as_bool) == Some(true) {
            command.push(flag.to_string());
        }
    }
    if let Some(environment) = gates.get("gate_environment") {
        if let Some(mode) = environment.get("mode").and_then(Value::as_str) {
            command.extend([
                "--gate-environment-mode".to_string(),
                mode.replace('_', "-"),
            ]);
        }
        for (key, flag) in [("variables", "--gate-env"), ("preserve", "--gate-env-from")] {
            if let Some(values) = environment.get(key).and_then(Value::as_object) {
                for (name, value) in values {
                    if let Some(value) = value.as_str() {
                        command.extend([flag.to_string(), format!("{name}={value}")]);
                    }
                }
            }
        }
        for (key, flag) in [
            ("isolate_home", "--isolate-gate-home"),
            ("isolate_xdg", "--isolate-gate-xdg"),
        ] {
            if let Some(value) = environment.get(key).and_then(Value::as_bool) {
                command.push(format!("{flag}={value}"));
            }
        }
        if let Some(value) = environment
            .get("shared_cargo_target")
            .and_then(Value::as_bool)
        {
            command.push(if value {
                "--gate-shared-cargo-target".to_string()
            } else {
                "--no-gate-shared-cargo-target".to_string()
            });
        }
        if let Some(inputs) = environment
            .get("extension_inputs")
            .and_then(Value::as_array)
        {
            for input in inputs {
                if let Ok(input) = serde_json::to_string(input) {
                    command.extend(["--gate-extension-input".to_string(), input]);
                }
            }
        }
    }
    for toolchain in &options.gates.gate_toolchains {
        if toolchain.probe_arguments == ["--version"] {
            command.extend(["--gate-toolchain".to_string(), toolchain.command.clone()]);
        } else if let Ok(toolchain) = serde_json::to_string(toolchain) {
            command.extend(["--gate-toolchain-spec".to_string(), toolchain]);
        }
    }
    for artifact in &options.gates.gate_package_artifacts {
        if let Ok(artifact) = serde_json::to_string(artifact) {
            command.extend(["--gate-package-artifact".to_string(), artifact]);
        }
    }
    if let Some(provider) = &options.provider_command {
        command.extend(["--provider-command".to_string(), provider.clone()]);
    }
    if let Some(invocation) = &options.provider_invocation {
        for argument in &invocation.argv {
            command.push(format!("--provider-argv={argument}"));
        }
    }
    command
}

/// Cook only promotes the candidate selected by the scheduler. A single-task
/// aggregate has no selection projection and retains the historical behavior.
pub(crate) fn selected_candidate_task_id(run_id: &str) -> Result<Option<String>> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    selected_candidate_task_id_in_store(&lifecycle_store, run_id)
}

fn selected_candidate_task_id_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let aggregate = lifecycle_store.read_aggregate(run_id)?;
    Ok(aggregate
        .selected_outcome()
        .or_else(|| {
            (aggregate.outcomes.len() == 1)
                .then(|| aggregate.outcomes.first())
                .flatten()
        })
        .map(|outcome| outcome.task_id.clone()))
}

/// Promotion is the durable boundary between a terminal provider result and
/// controller-owned gates. Reconciliation must reuse this exact report rather
/// than apply the selected artifact again.
pub(crate) fn promote_or_load_attempt(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    promote_or_load_attempt_in_store(&lifecycle_store, options, run_id)
}

pub(crate) fn promote_or_load_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    if let Some(promotion) = persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)? {
        if promotion.status == AgentTaskPromotionStatus::VerificationPending {
            let target_path = promotion.target.path.as_deref().or_else(|| {
                promotion.provenance.get("worktree_path").and_then(Value::as_str)
            }).map(PathBuf::from).or_else(|| {
                homeboy_core::worktree::resolve_if_present(&promotion.to_worktree)
                    .ok()
                    .flatten()
                    .map(|record| PathBuf::from(record.worktree_path))
            }).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion.target.path",
                    "verification-pending promotion has no durable candidate worktree path or registered worktree handle",
                    Some(run_id.to_string()),
                    None,
                )
            })?;
            let (source, source_path) = promotion_source_in_store(lifecycle_store, run_id)?;
            let observation_store = lifecycle_store.open_observation_initialized()?;
            let resumed = resume_promoted_patch_in_observation_store(
                AgentTaskPromotionOptions {
                    source,
                    source_run_id: Some(run_id.to_string()),
                    source_path,
                    source_worktree_path: options.source_worktree_path.clone(),
                    base_ref: Some(options.base.clone()),
                    task_base_sha: options.task_base_sha.clone(),
                    candidate_ref: None,
                    to_worktree: options.to_worktree.clone(),
                    // A resumed verification must retain the scheduler-selected
                    // candidate rather than falling back to aggregate outcome order.
                    task_id: selected_candidate_task_id_in_store(lifecycle_store, run_id)?,
                    // The checkpoint already authenticated this exact artifact;
                    // retain its identity rather than selecting an equivalent alias.
                    artifact_id: Some(promotion.patch_artifact.id.clone()),
                    dry_run: false,
                    gates: options.gates.clone(),
                    provider_command: options.provider_command.clone(),
                    provider_invocation: options.provider_invocation.clone(),
                },
                &target_path,
                &serde_json::to_value(&promotion)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
                &observation_store,
            )?;
            lifecycle_store.record_promotion(
                run_id,
                serde_json::to_value(&resumed)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
            )?;
            return Ok(resumed);
        }
        return Ok(promotion);
    }
    let promotion = promote_attempt_in_store(lifecycle_store, options, run_id)?;
    lifecycle_store.record_promotion(
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

/// A selector is accepted only from the route authority written by
/// `cook-continue`; normal Cook promotion retains automatic selection.
fn continuation_artifact_id(run_id: &str) -> Result<Option<String>> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    continuation_artifact_id_in_store(&lifecycle_store, run_id)
}

fn continuation_artifact_id_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record(run_id)?;
    let Some(route) = record.metadata.get("cook_continue_route") else {
        return Ok(None);
    };
    let artifact_id = route
        .get("artifact_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string);
    Ok(artifact_id)
}

pub(crate) fn persisted_promotion_for_attempt(
    run_id: &str,
) -> Result<Option<AgentTaskPromotionReport>> {
    let record = agent_task_lifecycle::status(run_id)?;
    persisted_promotion_from_record(run_id, record)
}

fn persisted_promotion_from_record(
    requested_run_id: &str,
    record: agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<AgentTaskPromotionReport>> {
    let Some(value) = record.metadata.get("latest_promotion") else {
        return Ok(None);
    };
    let mut promotion: AgentTaskPromotionReport =
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "latest_promotion",
                format!("persisted cook promotion is invalid: {error}"),
                Some(requested_run_id.to_string()),
                None,
            )
        })?;
    // `status` resolves a Cook alias to its immutable latest attempt. Validate
    // against that concrete owner so aliases and exact durable IDs share the
    // same persisted-promotion contract.
    if promotion.source.run_id.as_deref() != Some(record.run_id.as_str()) {
        let mut error = Error::validation_invalid_argument(
            "latest_promotion.source.run_id",
            "persisted cook promotion does not belong to this attempt",
            Some(requested_run_id.to_string()),
            None,
        );
        error.details["requested_run_id"] = serde_json::json!(requested_run_id);
        error.details["resolved_run_id"] = serde_json::json!(record.run_id);
        error.details["promotion_run_id"] = serde_json::json!(promotion.source.run_id);
        return Err(error);
    }
    restore_gate_feedback_baseline(&record, &mut promotion)?;
    promotion.normalize_gate_outcome();
    Ok(Some(promotion))
}

pub(crate) fn persisted_promotion_for_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<AgentTaskPromotionReport>> {
    let record = lifecycle_store.read_record(run_id)?;
    persisted_promotion_from_record(run_id, record)
}

/// Records green verification produced after an infrastructure-invalid gate
/// failure without rewriting the original promotion evidence. The replacement
/// must describe the exact already-applied candidate, so recovery never
/// re-applies a patch or silently broadens its verification scope.
pub fn record_replacement_gate_proof(
    run_id: &str,
    mut replacement: AgentTaskPromotionReport,
    external_authorization: Option<String>,
) -> Result<AgentTaskPromotionReport> {
    let original = persisted_promotion_for_attempt(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion",
            "replacement gate proof requires a persisted failed promotion",
            Some(run_id.to_string()),
            None,
        )
    })?;
    if original.status == AgentTaskPromotionStatus::Applied
        && original.provenance.get("replacement_gate_proof").is_some()
        && replacement.source == original.source
        && replacement.target == original.target
        && replacement.to_worktree == original.to_worktree
        && replacement.patch_artifact == original.patch_artifact
        && replacement.changed_files == original.changed_files
        && replacement.verified_base == original.verified_base
        && replacement.deterministic_gates == original.deterministic_gates
        && replacement.command_evidence == original.command_evidence
    {
        return Ok(original);
    }
    if original.status != AgentTaskPromotionStatus::GateFailed || !original.status.patch_promoted()
    {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.status",
            "replacement gate proof is only valid for an already-applied candidate whose original gates failed",
            Some(run_id.to_string()),
            None,
        ));
    }
    if external_authorization
        .as_deref()
        .is_none_or(|authorization| authorization.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "authorize_external_proof",
            "externally produced replacement gate proof requires explicit operator authorization",
            Some(run_id.to_string()),
            None,
        ));
    }
    replacement.normalize_gate_outcome();
    if replacement.status != AgentTaskPromotionStatus::Applied
        || replacement.deterministic_gates.is_empty()
        || replacement.verified_base.is_none()
        || replacement.deterministic_gates.iter().any(|gate| {
            gate.command.is_empty()
                || gate.exit_code != 0
                || gate.candidate_checkout.is_none()
                || !replacement
                    .command_evidence
                    .iter()
                    .any(|evidence| evidence.exit_code == 0 && evidence.command == gate.command)
        })
    {
        return Err(Error::validation_invalid_argument(
            "replacement_gate_proof",
            "replacement proof requires every green gate to have matching zero-exit command evidence, plus candidate checkout and verified base",
            Some(run_id.to_string()),
            None,
        ));
    }
    let same_candidate = replacement.provenance.get("candidate")
        == original.provenance.get("candidate")
        && replacement.provenance.get("candidate_checkout")
            == original.provenance.get("candidate_checkout");
    if replacement.source.run_id.as_deref() != Some(run_id)
        || replacement.target != original.target
        || replacement.to_worktree != original.to_worktree
        || replacement.patch_artifact != original.patch_artifact
        || replacement.changed_files != original.changed_files
        || replacement.verified_base != original.verified_base
        || !same_candidate
    {
        return Err(Error::validation_invalid_argument(
            "replacement_gate_proof",
            "replacement proof drifted from the exact failed promotion candidate, base, target, artifact, or scope",
            Some(run_id.to_string()),
            None,
        ));
    }
    let record = agent_task_lifecycle::status(run_id)?;
    let original_history_index = record
        .metadata
        .get("promotions")
        .and_then(Value::as_array)
        .and_then(|promotions| {
            record
                .metadata
                .get("latest_promotion")
                .and_then(|latest| promotions.iter().rposition(|promotion| promotion == latest))
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "latest_promotion",
                "replacement gate proof requires the original immutable promotion in history",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let original_history = &record.metadata["promotions"][original_history_index];
    let original_digest =
        content_hash::sha256_hex(&canonical_json_bytes(original_history).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize original promotion history".to_string()),
            )
        })?);
    replacement.provenance["replacement_gate_proof"] = serde_json::json!({
        "schema": "homeboy/agent-task-replacement-gate-proof/v1",
        "original_history": {
            "run_id": record.run_id,
            "metadata_key": "promotions",
            "index": original_history_index,
            "status": original.status,
            "deterministic_gate_count": original.deterministic_gates.len(),
            "sha256": original_digest,
        },
        "reason": "infrastructure_invalid_original_gates",
        "operator_authorization": external_authorization,
        "externally_produced": true,
        // `Inherit` serializes as the default in gate reports; recording the
        // resolved policy here keeps that valid standard policy explicit.
        "environment_policy": replacement.deterministic_gates.iter().map(|gate| serde_json::json!({
            "gate_id": gate.id,
            "environment": gate.environment,
        })).collect::<Vec<_>>(),
    });
    agent_task_lifecycle::record_promotion(
        run_id,
        serde_json::to_value(&replacement)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )?;
    Ok(replacement)
}

/// Older applied reports lost the post-apply baseline when the final gate
/// report replaced its checkpoint. Recover it only from one controller-owned
/// checkpoint for this exact promotion; any conflicting or incomplete evidence
/// leaves the destination unavailable for dirty-worktree reuse.
fn restore_gate_feedback_baseline(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    promotion: &mut AgentTaskPromotionReport,
) -> Result<()> {
    if promotion
        .provenance
        .pointer("/gate_feedback_baseline/current_diff")
        .and_then(Value::as_str)
        .is_some_and(|diff| !diff.trim().is_empty())
    {
        return Ok(());
    }
    let Some(events) = record.metadata.get("promotions").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut baselines = events
        .iter()
        .filter(|event| promotion_checkpoint_matches(promotion, event))
        .filter_map(|event| {
            event
                .pointer("/provenance/gate_feedback_baseline")
                .filter(|baseline| {
                    baseline.get("schema").and_then(Value::as_str)
                        == Some("homeboy/agent-task-gate-feedback-baseline/v1")
                        && baseline
                            .get("current_diff")
                            .and_then(Value::as_str)
                            .is_some_and(|diff| !diff.trim().is_empty())
                })
                .cloned()
        })
        .collect::<Vec<_>>();
    baselines.dedup();
    match baselines.len() {
        0 => Ok(()),
        1 => {
            promotion.provenance["gate_feedback_baseline"] = baselines.remove(0);
            Ok(())
        }
        _ => Err(Error::validation_invalid_argument(
            "latest_promotion",
            "persisted promotion has ambiguous controller checkpoint gate-feedback baselines",
            Some(record.run_id.clone()),
            None,
        )),
    }
}

fn promotion_checkpoint_matches(promotion: &AgentTaskPromotionReport, checkpoint: &Value) -> bool {
    checkpoint.get("status").and_then(Value::as_str) == Some("verification_pending")
        && checkpoint.pointer("/provenance/post_apply") == Some(&Value::Bool(true))
        && checkpoint.pointer("/source/run_id").and_then(Value::as_str)
            == promotion.source.run_id.as_deref()
        && checkpoint
            .pointer("/source/task_id")
            .and_then(Value::as_str)
            == Some(promotion.source.task_id.as_str())
        && checkpoint.get("to_worktree").and_then(Value::as_str)
            == Some(promotion.to_worktree.as_str())
        && checkpoint
            .pointer("/target/worktree")
            .and_then(Value::as_str)
            == Some(promotion.target.worktree.as_str())
        && checkpoint.pointer("/target/path")
            == promotion
                .target
                .path
                .as_ref()
                .map(|path| Value::String(path.clone()))
                .as_ref()
        && checkpoint
            .pointer("/patch_artifact/id")
            .and_then(Value::as_str)
            == Some(promotion.patch_artifact.id.as_str())
        && checkpoint
            .pointer("/patch_artifact/kind")
            .and_then(Value::as_str)
            == Some(promotion.patch_artifact.kind.as_str())
        && checkpoint
            .pointer("/patch_artifact/sha256")
            .and_then(Value::as_str)
            == promotion.patch_artifact.sha256.as_deref()
        && checkpoint.pointer("/provenance/candidate") == promotion.provenance.get("candidate")
}

pub(crate) fn attempt_needs_execution(run_id: &str) -> bool {
    agent_task_lifecycle::status(run_id)
        .map(|record| run_record_needs_execution(&record))
        .unwrap_or(true)
}

pub(crate) fn attempt_needs_execution_with_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> bool {
    if lifecycle_store
        .matches_current_environment()
        .unwrap_or(false)
    {
        return attempt_needs_execution(run_id);
    }
    lifecycle_store
        .read_record(run_id)
        .map(|record| run_record_needs_execution(&record))
        .unwrap_or(true)
}

fn run_record_needs_execution(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    !matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialFailure
            | agent_task_lifecycle::AgentTaskRunState::Failed
            | agent_task_lifecycle::AgentTaskRunState::Cancelled
    )
}

pub(crate) fn retryable_provider_discovery_failure(run_id: &str) -> bool {
    agent_task_lifecycle::status(run_id)
        .is_ok_and(|record| record.state == agent_task_lifecycle::AgentTaskRunState::Failed)
        && agent_task_lifecycle::read_aggregate(run_id).is_ok_and(|aggregate| {
            !aggregate.outcomes.is_empty()
                && aggregate.outcomes.iter().all(|outcome| {
                    outcome
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.class == "agent_task.provider_missing")
                })
        })
}

pub(crate) fn retryable_provider_discovery_failure_with_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> bool {
    if lifecycle_store
        .matches_current_environment()
        .unwrap_or(false)
    {
        return retryable_provider_discovery_failure(run_id);
    }
    lifecycle_store
        .read_record(run_id)
        .is_ok_and(|record| record.state == agent_task_lifecycle::AgentTaskRunState::Failed)
        && lifecycle_store
            .read_aggregate(run_id)
            .is_ok_and(|aggregate| {
                !aggregate.outcomes.is_empty()
                    && aggregate.outcomes.iter().all(|outcome| {
                        outcome
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.class == "agent_task.provider_missing")
                    })
            })
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
    let recipe_store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    moving_base_recovery_for_run_with_stores(&recipe_store, &lifecycle_store, run_id)
}

pub(crate) fn moving_base_recovery_for_run_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<MovingBaseCookRecovery>> {
    let record = lifecycle_store.read_record(run_id)?;
    record
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
        .and_then(|recovery: Option<MovingBaseCookRecovery>| {
            let Some(mut recovery) = recovery else {
                return Ok(None);
            };
            if recovery.run_id != run_id
                || record.metadata.get("cook_id").and_then(Value::as_str)
                    != Some(recovery.cook_id.as_str())
            {
                return Err(Error::validation_invalid_argument(
                    "cook_moving_base_recovery",
                    "durable moving-base recovery does not match its immutable Cook attempt",
                    Some(run_id.to_string()),
                    None,
                ));
            }
            let recipe = store.load_recipe(&recovery.cook_id)?;
            if !recipe
                .attempts
                .iter()
                .any(|attempt| attempt.run_id == run_id)
            {
                return Err(Error::validation_invalid_argument(
                    "cook_moving_base_recovery",
                    "durable moving-base recovery run is not declared by its Cook recipe",
                    Some(run_id.to_string()),
                    None,
                ));
            }
            // Recoveries written before scoped continuation used `run-next`.
            // The run ID remains authoritative, so expose the safe command
            // without requiring an unsafe migration of historical records.
            recovery.continuation = super::cook_continue_command(None, run_id, false, None);
            Ok(Some(recovery))
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
        // This recovery belongs to one immutable Cook attempt. `run-next` is a
        // global scheduler operation and must never be offered as its recovery.
        continuation: super::cook_continue_command(None, run_id, false, None),
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
    let mut report = cook_report(CookReportInput {
        cook_id,
        status: "candidate_recoverable",
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason,
        exit_code: 1,
        invocation_latest_run_id,
    });
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
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    recover_moving_base_cook_candidate_in_store(&lifecycle_store, options, recovery)
}

pub(crate) fn recover_moving_base_cook_candidate_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
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
    apply_immutable_candidate_to_base(
        path,
        &expected,
        &recovery.prior_verified_base,
        &recovery.promotion.changed_files,
        &fresh_base,
    )?;
    let mut checkpoint = serde_json::to_value(&recovery.promotion)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    checkpoint["status"] = serde_json::json!("verification_pending");
    checkpoint["verified_base"] = serde_json::json!({ "base": options.base, "sha": fresh_base });
    checkpoint["provenance"]["candidate"] = serde_json::to_value(candidate_fingerprint(path)?)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    checkpoint["provenance"]["resume_inputs"] = serde_json::json!({ "base_ref": options.base, "task_base_sha": options.task_base_sha, "candidate_ref": null });
    checkpoint["provenance"]["resume_contract"] = serde_json::json!({
        "kind": "moving_base_recovery",
        "source_candidate": expected,
        "source_verified_base": recovery.prior_verified_base,
        "resolved_base": fresh_base,
        "previous": recovery.promotion.provenance.get("resume_contract"),
    });
    let (source, source_path) = promotion_source_in_store(lifecycle_store, &recovery.run_id)?;
    let observation_store = lifecycle_store.open_observation_initialized()?;
    let refreshed = resume_promoted_patch_in_observation_store(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some(recovery.run_id.clone()),
            source_path,
            source_worktree_path: options.source_worktree_path.clone(),
            base_ref: Some(options.base.clone()),
            task_base_sha: options.task_base_sha.clone(),
            candidate_ref: None,
            to_worktree: options.to_worktree.clone(),
            task_id: selected_candidate_task_id_in_store(lifecycle_store, &recovery.run_id)?,
            // Moving-base recovery re-verifies the original promoted candidate.
            artifact_id: Some(recovery.promotion.patch_artifact.id.clone()),
            dry_run: false,
            gates: options.gates.clone(),
            provider_command: options.provider_command.clone(),
            provider_invocation: options.provider_invocation.clone(),
        },
        std::path::Path::new(path),
        &checkpoint,
        &observation_store,
    )?;
    Ok(refreshed)
}

/// Re-materialize an authenticated dirty candidate on a newer base. The
/// temporary index proves both applicability and candidate-owned file scope
/// before the destination is reset, so intervening base changes never become a
/// candidate commit or dirty worktree content.
fn apply_immutable_candidate_to_base(
    path: &str,
    candidate: &crate::agent_task_promotion::AgentTaskPromotionCandidate,
    prior_verified_base: &str,
    recorded_changed_files: &[String],
    fresh_base: &str,
) -> Result<()> {
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } = candidate
    else {
        return Err(Error::validation_invalid_argument(
            "promotion.provenance.candidate",
            "moving-base recovery requires a Git candidate fingerprint",
            None,
            None,
        ));
    };
    if prior_verified_base.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "promotion.verified_base",
            "moving-base recovery requires the immutable verified base recorded for the candidate",
            None,
            None,
        ));
    }
    if !git_status(
        path,
        &[
            "merge-base",
            "--is-ancestor",
            prior_verified_base,
            &fingerprint.head,
        ],
    )? {
        return Err(Error::validation_invalid_argument(
            "promotion.provenance.candidate",
            "moving-base recovery candidate is not descended from its immutable verified base",
            None,
            None,
        ));
    }
    let candidate_files = git_changed_files(path, prior_verified_base, &fingerprint.tree)?;
    if candidate_files != normalized_changed_files(recorded_changed_files) {
        return Err(Error::validation_invalid_argument(
            "promotion.changed_files",
            "moving-base recovery candidate scope differs from the durable reviewer file list",
            None,
            None,
        ));
    }
    let patch = tempfile::NamedTempFile::new()
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    let diff = git_output_bytes(
        path,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--find-renames",
            prior_verified_base,
            &fingerprint.tree,
        ],
    )?;
    if diff.iter().all(u8::is_ascii_whitespace) {
        return Err(Error::validation_invalid_argument(
            "promotion.provenance.candidate",
            "moving-base recovery candidate has no immutable delta from its recorded HEAD",
            None,
            None,
        ));
    }
    std::fs::write(patch.path(), diff)
        .map_err(|error| Error::internal_io(error.to_string(), None))?;

    let index = tempfile::NamedTempFile::new()
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    git_with_index(path, &["read-tree", fresh_base], index.path())?;
    git_with_index(
        path,
        &[
            "apply",
            "--cached",
            "--check",
            "--binary",
            patch.path().to_str().unwrap_or_default(),
        ],
        index.path(),
    )?;
    git_with_index(
        path,
        &[
            "apply",
            "--cached",
            "--binary",
            patch.path().to_str().unwrap_or_default(),
        ],
        index.path(),
    )?;
    let projected_tree = git_with_index(path, &["write-tree"], index.path())?;
    let projected_files = git_changed_files(path, fresh_base, &projected_tree)?;
    if projected_files != candidate_files {
        return Err(Error::validation_invalid_argument(
            "promotion.provenance.candidate",
            "moving-base recovery projection changes files outside the authenticated candidate scope",
            None,
            None,
        ));
    }
    if &candidate_fingerprint(path)? != candidate {
        return Err(Error::validation_invalid_argument(
            "path",
            "moving-base recovery destination changed while projecting the authenticated candidate",
            Some(path.to_string()),
            None,
        ));
    }

    git(path, &["reset", "--hard", fresh_base])?;
    remove_untracked_candidate_paths(path, &candidate_files)?;
    git(
        path,
        &[
            "apply",
            "--whitespace=nowarn",
            "--binary",
            patch.path().to_str().unwrap_or_default(),
        ],
    )?;
    Ok(())
}

fn remove_untracked_candidate_paths(path: &str, candidate_files: &[String]) -> Result<()> {
    if candidate_files.is_empty() {
        return Ok(());
    }
    let output = std::process::Command::new("git")
        .arg("clean")
        .arg("-f")
        .arg("--")
        .args(candidate_files)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "path",
        format!(
            "moving-base recovery could not remove authenticated untracked candidate paths: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        None,
        None,
    ))
}

fn normalized_changed_files(files: &[String]) -> Vec<String> {
    let mut files = files.to_vec();
    files.sort();
    files.dedup();
    files
}

fn git_changed_files(path: &str, base: &str, tree: &str) -> Result<Vec<String>> {
    Ok(normalized_changed_files(
        &git_output(path, &["diff", "--name-only", "--no-renames", base, tree])?
            .lines()
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
    ))
}

fn git_status(path: &str, args: &[&str]) -> Result<bool> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        return Ok(true);
    }
    // `merge-base --is-ancestor` returns one for a valid negative result.
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(Error::validation_invalid_argument(
        "base",
        format!(
            "moving-base recovery Git operation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        None,
        None,
    ))
}

fn git(path: &str, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "base",
        format!(
            "moving-base recovery Git operation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        None,
        None,
    ))
}

fn git_output(path: &str, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_output_bytes(path, args)?)
        .trim()
        .to_string())
}

fn git_output_bytes(path: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(Error::validation_invalid_argument(
        "base",
        format!(
            "moving-base recovery Git operation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        None,
        None,
    ))
}

fn git_with_index(path: &str, args: &[&str], index: &std::path::Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(Error::validation_invalid_argument(
        "base",
        format!(
            "moving-base recovery candidate conflicts with resolved base: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        None,
        None,
    ))
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

#[cfg(test)]
mod moving_base_tests {
    use super::*;

    fn git(path: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn moving_base_overlap_is_rejected_before_destination_mutation() {
        let temp = tempfile::tempdir().expect("temporary repositories");
        let remote = temp.path().join("origin.git");
        let seed = temp.path().join("seed");
        let destination = temp.path().join("destination");
        let upstream = temp.path().join("upstream");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "--initial-branch=main"]);
        std::fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.com"]);
        std::fs::write(seed.join("candidate.txt"), "base\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "base"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);
        git(
            temp.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        std::fs::write(destination.join("candidate.txt"), "candidate\n").unwrap();
        let expected = candidate_fingerprint(destination.to_str().unwrap()).unwrap();

        git(
            temp.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                upstream.to_str().unwrap(),
            ],
        );
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["config", "user.email", "test@example.com"]);
        std::fs::write(upstream.join("candidate.txt"), "upstream\n").unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-m", "overlapping base change"]);
        git(&upstream, &["push", "origin", "main"]);
        let fresh_base = git(&upstream, &["rev-parse", "HEAD"]);
        git(&destination, &["fetch", "origin", &fresh_base]);

        let error = apply_immutable_candidate_to_base(
            destination.to_str().unwrap(),
            &expected,
            &git(&seed, &["rev-parse", "HEAD"]),
            &["candidate.txt".to_string()],
            &fresh_base,
        )
        .expect_err("overlapping candidate must fail before mutation");
        assert!(error.message.contains("conflicts with resolved base"));
        assert_eq!(
            candidate_fingerprint(destination.to_str().unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            git(&destination, &["status", "--porcelain"]),
            "M candidate.txt"
        );
    }

    #[test]
    fn moving_base_projection_preserves_committed_and_dirty_candidate_changes() {
        let temp = tempfile::tempdir().expect("temporary repositories");
        let remote = temp.path().join("origin.git");
        let seed = temp.path().join("seed");
        let destination = temp.path().join("destination");
        let upstream = temp.path().join("upstream");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "--initial-branch=main"]);
        std::fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.com"]);
        std::fs::write(seed.join("base.txt"), "base\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "base"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);
        let prior_verified_base = git(&seed, &["rev-parse", "HEAD"]);
        git(
            temp.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        git(&destination, &["config", "user.name", "Test"]);
        git(&destination, &["config", "user.email", "test@example.com"]);
        std::fs::write(destination.join("committed.txt"), "committed\n").unwrap();
        git(&destination, &["add", "committed.txt"]);
        git(&destination, &["commit", "-m", "candidate commit"]);
        std::fs::write(destination.join("dirty.txt"), "dirty\n").unwrap();
        let candidate = candidate_fingerprint(destination.to_str().unwrap()).unwrap();

        git(
            temp.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                upstream.to_str().unwrap(),
            ],
        );
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["config", "user.email", "test@example.com"]);
        std::fs::write(upstream.join("upstream.txt"), "upstream\n").unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-m", "advance base"]);
        git(&upstream, &["push", "origin", "main"]);
        let fresh_base = git(&upstream, &["rev-parse", "HEAD"]);
        git(&destination, &["fetch", "origin", &fresh_base]);

        apply_immutable_candidate_to_base(
            destination.to_str().unwrap(),
            &candidate,
            &prior_verified_base,
            &["committed.txt".to_string(), "dirty.txt".to_string()],
            &fresh_base,
        )
        .expect("project complete candidate lineage");

        assert_eq!(
            git(&destination, &["status", "--porcelain"]),
            "?? committed.txt\n?? dirty.txt"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("committed.txt")).unwrap(),
            "committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("dirty.txt")).unwrap(),
            "dirty\n"
        );
    }
}

/// Finalization publishes controller-owned state. Persist its completed report
/// on the attempt so a restarted continuation cannot open a second PR.
pub(crate) fn finalize_or_load_cook_pr(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<Value> {
    let store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    finalize_or_load_cook_pr_with_stores(
        &store,
        &lifecycle_store,
        options,
        successful_run_id,
        promotion,
    )
}

pub(crate) fn finalize_or_load_cook_pr_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<Value> {
    finalize_or_load_cook_pr_with_backend_with_stores(
        store,
        lifecycle_store,
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
    let store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    finalize_or_load_cook_pr_with_backend_with_stores(
        &store,
        &lifecycle_store,
        options,
        successful_run_id,
        promotion,
        backend,
    )
}

pub(crate) fn finalize_or_load_cook_pr_with_backend_with_stores<
    B: AgentTaskPrFinalizationBackend,
>(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    let finalization = finalize_cook_pr_with_backend_with_stores(
        store,
        lifecycle_store,
        options,
        successful_run_id,
        promotion,
        backend,
    )?;
    lifecycle_store.record_cook_finalization(successful_run_id, finalization.clone())?;
    Ok(finalization)
}

pub(crate) fn finalize_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    let store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    finalize_cook_pr_with_backend_with_stores(
        &store,
        &lifecycle_store,
        options,
        successful_run_id,
        promotion,
        backend,
    )
}

pub(crate) fn finalize_cook_pr_with_backend_with_stores<B: AgentTaskPrFinalizationBackend>(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    let mut promotion = promotion.clone();
    promotion.normalize_gate_outcome();
    if !promotion.finalization_eligible(options.gates.accept_inherited_failures) {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "agent-task cook finalization requires green gates or explicitly accepted inherited baseline failures",
            None,
            None,
        ));
    }
    let finalization = cook_finalization_options_with_stores(
        store,
        lifecycle_store,
        options,
        successful_run_id,
        &promotion,
        Vec::new(),
    )?;
    lifecycle_store.record_promotion(
        successful_run_id,
        serde_json::to_value(&promotion).unwrap_or(Value::Null),
    )?;
    finalize_pr_with_backend_in_store(finalization, backend, lifecycle_store)
        .map(|report| serde_json::to_value(report).unwrap_or(Value::Null))
}

pub(crate) fn cook_finalization_options(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    overrides: Vec<AgentTaskReviewOverride>,
) -> Result<AgentTaskPrFinalizationOptions> {
    let store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    cook_finalization_options_with_stores(
        &store,
        &lifecycle_store,
        options,
        successful_run_id,
        promotion,
        overrides,
    )
}

pub(crate) fn cook_finalization_options_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    overrides: Vec<AgentTaskReviewOverride>,
) -> Result<AgentTaskPrFinalizationOptions> {
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
    let mut review_dossier = cook_review_dossier_with_stores(
        store,
        lifecycle_store,
        options,
        promotion,
        successful_run_id,
    )?;
    review_dossier.overrides = overrides;
    // A non-empty option is an explicit operator disclosure. Otherwise retain
    // the validated review form's process statement as durable PR provenance.
    let ai_used_for = if options.ai_used_for.trim().is_empty() {
        review_dossier.ai_assistance.used_for.clone()
    } else {
        options.ai_used_for.clone()
    };
    review_dossier.ai_assistance.used_for = ai_used_for.clone();
    let targeted_checks_run = review_dossier
        .how_to_test
        .iter()
        .map(|step| step.command.clone())
        .collect();
    let inherited_failure_count = promotion
        .deterministic_gates
        .iter()
        .filter(|gate| {
            gate.status == crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
        })
        .count();
    let attempt_summary = if inherited_failure_count == 0 {
        format!(
            "{} deterministic cook gate attempt(s) completed green",
            promotion.deterministic_gates.len()
        )
    } else {
        format!(
            "{} deterministic cook gate attempt(s) completed; {inherited_failure_count} inherited baseline-red failure(s) were reproduced on the immutable base and explicitly accepted",
            promotion.deterministic_gates.len()
        )
    };
    Ok(AgentTaskPrFinalizationOptions {
        path: path.clone(),
        run_id: successful_run_id.to_string(),
        base: options.base.clone(),
        verified_base_sha: Some(verified_base.sha.clone()),
        head: options.head.clone(),
        title: options.title.clone(),
        commit_message: options.commit_message.clone(),
        gate_results: Vec::new(),
        normalized_gate_results: promotion.gate_results.clone(),
        accept_inherited_failures: options.gates.accept_inherited_failures,
        changed_files: promotion.changed_files.clone(),
        evidence: AgentTaskPrEvidence {
            source_refs,
            artifact_refs,
            attempt_summary,
            ai_tool: options.ai_tool.clone(),
            ai_model: options.ai_model.clone(),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification {
                targeted_checks_run,
                targeted_checks_unavailable: None,
                ci_expected: vec!["Homeboy CI after push".to_string()],
                manual_reviewer_check: None,
            },
            runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
            changed_public_contracts: Vec::new(),
            public_contract_evidence: None,
            lifecycle: lifecycle_store
                .read_record(successful_run_id)
                .ok()
                .map(|record| record.lifecycle),
        },
        ai_used_for,
        review_dossier,
        review_profile: resolve_review_profile(&path)?,
        manual_finalization: false,
        expected_candidate_sha: None,
        protected_branches: options.protected_branches.clone(),
        draft_pr: options.draft_pr,
    })
}

/// Persist only a controller-validated manual preflight dossier for recovery.
pub fn persist_manual_finalization_intent(
    run_id: &str,
    report: &AgentTaskPrFinalizationReport,
) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord> {
    validate_manual_preflight_report(report, run_id)?;
    agent_task_lifecycle::record_manual_finalization_intent(
        run_id,
        serde_json::to_value(report).expect("finalization report serializes"),
    )
}

/// Resolve the sole durable identity contract for explicit manual publication.
/// A Cook ID selects its newest attempt, which must be failed; an exact existing
/// ID must also be a failed attempt. An unused ID is reserved as a durable
/// manual-finalization record so validated intent and publication receipt have
/// an audit home.
pub fn prepare_manual_finalization_identity(requested_id: &str) -> Result<String> {
    if super::cook_recipe::recipe_exists(requested_id)? {
        let recipe = super::cook_recipe::load_recipe(requested_id)?;
        let run_id = recipe
            .attempts
            .last()
            .map(|attempt| attempt.run_id.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "run_id",
                    "manual finalization Cook identity has no durable attempts",
                    Some(requested_id.to_string()),
                    None,
                )
            })?;
        return require_manual_finalization_run(&run_id);
    }

    if crate::agent_task_lifecycle::run_record_exists(requested_id)? {
        return require_manual_finalization_run(requested_id);
    }

    let plan = crate::agent_task_scheduler::AgentTaskPlan::new(
        format!("manual-finalization-{requested_id}"),
        Vec::new(),
    );
    crate::agent_task_lifecycle::submit_plan(&plan, Some(requested_id))?;
    crate::agent_task_lifecycle::record_metadata_value(
        requested_id,
        "manual_finalization_identity",
        serde_json::json!(true),
    )?;
    crate::agent_task_lifecycle::record_metadata_value(
        requested_id,
        "manual_finalization_identity_version",
        serde_json::json!(1),
    )?;
    Ok(requested_id.to_string())
}

fn require_manual_finalization_run(run_id: &str) -> Result<String> {
    let record = crate::agent_task_lifecycle::persisted_status(run_id)?;
    if record.metadata["manual_finalization_identity"] == true {
        return Ok(run_id.to_string());
    }
    if record.lifecycle.execution.state
        != homeboy_core::run_lifecycle_record::RunExecutionState::Failed
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "manual finalization accepts an existing failed attempt or an unused ID for a new durable manual-finalization record",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(run_id.to_string())
}

/// Persist only a controller-published manual receipt bound to its preflight dossier.
pub fn persist_manual_finalization_receipt(
    run_id: &str,
    report: &AgentTaskPrFinalizationReport,
) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord> {
    let record = agent_task_lifecycle::status(run_id)?;
    let intent = manual_finalization_intent_for_run(&record, run_id)?;
    if !valid_manual_finalization_receipt(report, &intent, &record, run_id, false) {
        return Err(Error::validation_invalid_argument(
            "cook_finalization",
            "manual finalization receipt failed controller validation",
            Some(run_id.to_string()),
            None,
        ));
    }
    agent_task_lifecycle::record_manual_finalization_receipt(
        run_id,
        serde_json::to_value(report).expect("finalization report serializes"),
    )
}

/// Recover publication from the durable Cook recipe and applied promotion.
pub fn recover_cook_pr(
    run_or_cook_id: &str,
    overrides: Vec<AgentTaskReviewOverride>,
    preflight: bool,
) -> Result<Value> {
    recover_cook_pr_with_backend(
        run_or_cook_id,
        overrides,
        preflight,
        &mut RealAgentTaskPrFinalizationBackend,
    )
}

pub fn recover_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    run_or_cook_id: &str,
    overrides: Vec<AgentTaskReviewOverride>,
    preflight: bool,
    backend: &mut B,
) -> Result<Value> {
    let recipe = if super::cook_recipe::recipe_exists(run_or_cook_id)? {
        super::cook_recipe::load_recipe(run_or_cook_id)?
    } else {
        match super::cook_recipe::load_recipe_for_attempt(run_or_cook_id)? {
            Some(recipe) => recipe,
            None => {
                return recover_manual_finalization_pr(
                    run_or_cook_id,
                    overrides,
                    preflight,
                    backend,
                )
            }
        }
    };
    if let Some(receipt) = completed_finalization_receipt_for_recovery(&recipe, run_or_cook_id)? {
        return Ok(receipt);
    }
    if let Some(report) = manual_finalization_intent_for_recovery(&recipe, run_or_cook_id)? {
        if !overrides.is_empty() {
            return Err(Error::validation_invalid_argument(
                "review_overrides",
                "recovered manual finalization must execute the preflight-validated dossier unchanged",
                None,
                None,
            ));
        }
        let finalization = manual_finalization_options(report)?;
        let report = if preflight {
            preflight_pr_with_backend(finalization, backend)?
        } else {
            finalize_pr_with_backend(finalization, backend)?
        };
        let value = serde_json::to_value(&report).unwrap_or(Value::Null);
        if !preflight {
            persist_manual_finalization_receipt(
                value["run_id"].as_str().unwrap_or_default(),
                &report,
            )?;
        }
        return Ok(value);
    }
    let run_id = if recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == run_or_cook_id)
    {
        run_or_cook_id.to_string()
    } else {
        canonical_cook_recovery_run_id(&recipe.cook_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_or_cook_id",
                "durable Cook has no canonical promotable candidate",
                Some(run_or_cook_id.to_string()),
                None,
            )
        })?
    };
    let promotion = persisted_promotion_for_attempt(&run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion",
            "recovery requires the attempt's persisted applied promotion",
            Some(run_id.clone()),
            None,
        )
    })?;
    let options = super::cook_recipe::reconstruct_adoption_options(&recipe)?;
    // An explicitly accepted inherited baseline failure is finalizable (#11460),
    // so it is recoverable too. Judge it with the cook's own
    // accept_inherited_failures rather than requiring a green-gate Applied.
    let recovery_outcome = promotion.gate_outcome();
    if recovery_outcome.status != AgentTaskPromotionStatus::Applied
        && !(recovery_outcome.status == AgentTaskPromotionStatus::GateFailed
            && promotion.finalization_eligible(options.gates.accept_inherited_failures))
    {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.status",
            "recovery requires an applied promotion with green gates or an explicitly accepted inherited baseline failure",
            Some(run_id),
            None,
        ));
    }
    let finalization = cook_finalization_options(&options, &run_id, &promotion, overrides)?;
    if !preflight {
        agent_task_lifecycle::record_promotion(
            &run_id,
            serde_json::to_value(&promotion).unwrap_or(Value::Null),
        )?;
    }
    let report = if preflight {
        preflight_pr_with_backend(finalization, backend)?
    } else {
        finalize_pr_with_backend(finalization, backend)?
    };
    let value = serde_json::to_value(report).unwrap_or(Value::Null);
    if !preflight {
        agent_task_lifecycle::record_cook_finalization(&run_id, value.clone())?;
    }
    Ok(value)
}

pub(crate) fn canonical_cook_recovery_run_id(cook_id: &str) -> Option<String> {
    let candidate =
        canonical_cook_candidate(cook_id).filter(|candidate| candidate["incomplete"] != true)?;
    let source_run_id = candidate["run_id"].as_str()?.to_string();
    if source_run_id.is_empty() {
        return None;
    }
    if let Ok(recipe) = super::cook_recipe::load_recipe(cook_id) {
        for attempt in recipe.attempts.iter().rev() {
            let Ok(record) = agent_task_lifecycle::exact_record(&attempt.run_id) else {
                continue;
            };
            if review_form_attempt_is_ready_for_cook_continuation(&attempt.plan, &record).ok()?
                && persisted_promotion_for_attempt(&attempt.run_id)
                    .ok()
                    .flatten()
                    .is_some_and(|promotion| {
                        promotion
                            .provenance
                            .pointer("/cook_follow_up/source_run_id")
                            .and_then(Value::as_str)
                            == Some(source_run_id.as_str())
                    })
            {
                return Some(attempt.run_id.clone());
            }
        }
    }
    Some(source_run_id)
}

/// Recover a standalone manual-finalization record. Unlike Cook attempts, a
/// manual identity can be reserved without a recipe, so it is its own durable
/// recovery root.
fn recover_manual_finalization_pr<B: AgentTaskPrFinalizationBackend>(
    run_id: &str,
    overrides: Vec<AgentTaskReviewOverride>,
    preflight: bool,
    backend: &mut B,
) -> Result<Value> {
    let record = agent_task_lifecycle::status(run_id).map_err(|_| {
        Error::validation_invalid_argument(
            "run_or_cook_id",
            "no durable Cook recipe or manual finalization record contains this run or cook id",
            Some(run_id.to_string()),
            None,
        )
    })?;
    if let Some(receipt) = manual_finalization_receipt_for_run(&record, run_id)? {
        return Ok(receipt);
    }
    if !overrides.is_empty() {
        return Err(Error::validation_invalid_argument(
            "review_overrides",
            "recovered manual finalization must execute the preflight-validated dossier unchanged",
            None,
            None,
        ));
    }
    let report = manual_finalization_intent_for_run(&record, run_id)?;
    let finalization = manual_finalization_options(report)?;
    let report = if preflight {
        preflight_pr_with_backend(finalization, backend)?
    } else {
        finalize_pr_with_backend(finalization, backend)?
    };
    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
    if !preflight {
        persist_manual_finalization_receipt(run_id, &report)?;
    }
    Ok(value)
}

fn manual_finalization_run_ids(recipe: &super::cook_recipe::AgentTaskCookRecipe) -> Vec<&str> {
    recipe
        .attempts
        .iter()
        .rev()
        .map(|attempt| attempt.run_id.as_str())
        .collect()
}

fn completed_finalization_receipt_for_recovery(
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
    run_or_cook_id: &str,
) -> Result<Option<Value>> {
    if run_or_cook_id == recipe.cook_id {
        let selected_candidate = canonical_cook_candidate(&recipe.cook_id);
        if selected_candidate.is_some() {
            let receipt = canonical_candidate_finalization(selected_candidate.as_ref(), None, None);
            return Ok(receipt.filter(cook_finalization_is_pr_receipt));
        }
    }
    let run_ids = if recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == run_or_cook_id)
    {
        vec![run_or_cook_id]
    } else {
        manual_finalization_run_ids(recipe)
    };
    for run_id in run_ids {
        let record = agent_task_lifecycle::status(run_id)?;
        let Some(value) = record.metadata.get("cook_finalization") else {
            continue;
        };
        if !matches!(
            value["status"].as_str(),
            Some("review_ready" | "draft_published")
        ) {
            continue;
        }
        // Normal Cook finalization receipts intentionally have a lightweight,
        // generic shape. Only the explicit manual receipt schema requires the
        // additional recovery integrity contract.
        if value["manual_finalization"] == true {
            return manual_finalization_receipt_for_run(&record, run_id);
        }
        return Ok(Some(value.clone()));
    }
    Ok(None)
}

fn manual_finalization_receipt_for_run(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    run_id: &str,
) -> Result<Option<Value>> {
    let Some(value) = record.metadata.get("cook_finalization") else {
        return Ok(None);
    };
    if !matches!(
        value["status"].as_str(),
        Some("review_ready" | "draft_published")
    ) || value["manual_finalization"] != true
    {
        return Ok(None);
    }
    let report: AgentTaskPrFinalizationReport =
        serde_json::from_value(value.clone()).map_err(|_| {
            Error::validation_invalid_argument(
                "cook_finalization",
                "persisted manual finalization receipt is invalid",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let intent = manual_finalization_intent_for_run(record, run_id)?;
    if !valid_manual_finalization_receipt(&report, &intent, record, run_id, true) {
        return Err(Error::validation_invalid_argument(
            "cook_finalization",
            "persisted manual finalization receipt failed integrity validation",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(Some(value.clone()))
}

fn manual_finalization_intent_for_recovery(
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
    run_or_cook_id: &str,
) -> Result<Option<AgentTaskPrFinalizationReport>> {
    let run_ids = if recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == run_or_cook_id)
    {
        vec![run_or_cook_id]
    } else {
        manual_finalization_run_ids(recipe)
    };
    for run_id in run_ids {
        let record = agent_task_lifecycle::status(run_id)?;
        let Some(_) = record.metadata.get("manual_finalization_intent") else {
            continue;
        };
        let report = manual_finalization_intent_for_run(&record, run_id)?;
        return Ok(Some(report));
    }
    Ok(None)
}

fn manual_finalization_intent_for_run(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    run_id: &str,
) -> Result<AgentTaskPrFinalizationReport> {
    let value = record
        .metadata
        .get("manual_finalization_intent")
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "manual_finalization_intent",
                "manual finalization receipt has no persisted validated intent",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let expected_digest = record
        .metadata
        .get("manual_finalization_intent_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if expected_digest != agent_task_lifecycle::manual_finalization_intent_digest(value) {
        return Err(Error::validation_invalid_argument(
            "manual_finalization_intent",
            "persisted manual finalization intent failed integrity validation",
            Some(run_id.to_string()),
            None,
        ));
    }
    let report: AgentTaskPrFinalizationReport =
        serde_json::from_value(value.clone()).map_err(|_| {
            Error::validation_invalid_argument(
                "manual_finalization_intent",
                "persisted manual finalization intent is invalid",
                Some(run_id.to_string()),
                None,
            )
        })?;
    if report.run_id != run_id {
        return Err(Error::validation_invalid_argument(
            "manual_finalization_intent.run_id",
            "persisted manual finalization intent belongs to a different durable run",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(report)
}

fn valid_manual_finalization_receipt(
    report: &AgentTaskPrFinalizationReport,
    intent: &AgentTaskPrFinalizationReport,
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    run_id: &str,
    require_persisted_digest: bool,
) -> bool {
    let Some(binding) = report.publication_proof.binding.as_ref() else {
        return false;
    };
    let Some(git_identity) = report.publication_proof.git_identity.as_ref() else {
        return false;
    };
    let Some(intent_git_identity) = intent.publication_proof.git_identity.as_ref() else {
        return false;
    };
    report.schema == crate::agent_task_finalization::AGENT_TASK_PR_FINALIZATION_SCHEMA
        && report.run_id == run_id
        && intent.status == "validated"
        && intent.manual_finalization
        && receipt_matches_manual_preflight(
            report,
            intent,
            binding,
            git_identity,
            intent_git_identity,
        )
        && (!require_persisted_digest
            || (record.metadata["manual_finalization_receipt_digest"]
                == serde_json::json!(agent_task_lifecycle::manual_finalization_intent_digest(
                    &serde_json::to_value(report).expect("finalization report serializes"),
                ))
                && record.metadata["manual_finalization_receipt_intent_digest"]
                    == record.metadata["manual_finalization_intent_digest"]))
        && validate_publication_intent(&report.publication_intent).is_ok()
        && report.publication_intent.run_id == run_id
        && report.publication_proof.run_id == run_id
        && report.finalization_outcome.run_id == run_id
        && report.publication_proof.schema
            == crate::agent_task_finalization::AGENT_TASK_PUBLICATION_PROOF_SCHEMA
        && report.publication_proof.intent_schema == report.publication_intent.schema
        && report.publication_proof.status == "review_ready"
        && report.finalization_outcome.schema
            == crate::agent_task_finalization::AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA
        && matches!(report.pr_action.as_str(), "created" | "updated")
        && report.publication_proof.adapter_action.as_deref() == Some(report.pr_action.as_str())
        && report.publication_proof.adapter_ref == report.pr_url
        && report.pr_number.is_some()
        && report.pr_url.is_some()
        && same_publication_target(
            &report.publication_proof.target,
            &report.publication_intent.target,
        )
        && report.finalization_outcome.target == report.publication_proof.target
        && report.publication_proof.target.url == report.pr_url
        && report.finalization_outcome.pr_number == report.pr_number
        && report.finalization_outcome.pr_url == report.pr_url
        && report.finalization_outcome.status == "review_ready"
        && report.finalization_outcome.publication_action == report.pr_action
        && report.finalization_outcome.publication_status == "review_ready"
        && report.finalization_outcome.published
        && !report.finalization_outcome.committed
        && !report.finalization_outcome.pushed
        && binding.candidate_sha == binding.remote_sha
        && binding.candidate_sha == binding.pr_head_sha
        && git_identity.commit_sha.as_deref() == Some(binding.candidate_sha.as_str())
        && report
            .publication_proof
            .git_tracking
            .as_ref()
            .is_none_or(|tracking| tracking.verified_remote_sha == binding.candidate_sha)
        && binding.repository == binding.head_repository
        && binding.changed_files == report.changed_files
        && report.publication_intent.changed_files == report.changed_files
        && report.finalization_outcome.changed_files == report.changed_files
}

fn receipt_matches_manual_preflight(
    receipt: &AgentTaskPrFinalizationReport,
    intent: &AgentTaskPrFinalizationReport,
    binding: &crate::agent_task_finalization::AgentTaskPublicationBinding,
    git_identity: &homeboy_core::git::GitIdentityProof,
    intent_git_identity: &homeboy_core::git::GitIdentityProof,
) -> bool {
    let mut receipt_dossier = receipt.review_dossier.clone();
    receipt_dossier
        .evidence
        .retain(|evidence| intent.review_dossier.evidence.contains(evidence));
    receipt.path == intent.path
        && receipt.base == intent.base
        && receipt.head == intent.head
        && receipt.title == intent.title
        && receipt.changed_files == intent.changed_files
        && receipt.proof == intent.proof
        && receipt_dossier == intent.review_dossier
        && receipt.review_dossier.evidence.iter().all(|evidence| {
            intent.review_dossier.evidence.contains(evidence)
                || is_publication_base_observation(evidence, receipt)
        })
        && intent
            .review_dossier
            .evidence
            .iter()
            .all(|evidence| receipt.review_dossier.evidence.contains(evidence))
        && receipt.publication_intent.proof == intent.publication_intent.proof
        && same_preflight_publication_target(
            &receipt.publication_intent.target,
            &intent.publication_intent.target,
        )
        && intent_git_identity.commit_sha.is_some()
        && git_identity.commit_sha == intent_git_identity.commit_sha
        && binding.candidate_sha
            == intent_git_identity
                .commit_sha
                .as_deref()
                .unwrap_or_default()
}

fn is_publication_base_observation(
    evidence: &AgentTaskReviewEvidence,
    receipt: &AgentTaskPrFinalizationReport,
) -> bool {
    if evidence.url.is_some() {
        return false;
    }
    let target = &receipt.publication_intent.target;
    let verified_base_sha = target.verified_base_sha.as_deref().unwrap_or_default();
    if verified_base_sha.is_empty() {
        return false;
    }
    let expected = match target.publication_base_sha.as_deref() {
        Some(publication_base_sha) if publication_base_sha == verified_base_sha => format!(
            "Base unchanged since verification: {} remains at {}.",
            receipt.base, verified_base_sha
        ),
        Some(publication_base_sha) => format!(
            "Base advanced after verification: verified {} at {}; publication observed {}. Candidate ancestry was validated against the verified snapshot.",
            receipt.base, verified_base_sha, publication_base_sha
        ),
        None => format!(
            "Base observation unavailable immediately before publication; candidate ancestry was validated against verified {} at {}.",
            receipt.base, verified_base_sha
        ),
    };
    evidence.summary == expected
}

fn same_publication_target(
    published: &crate::agent_task_finalization::AgentTaskPublicationTarget,
    intent: &crate::agent_task_finalization::AgentTaskPublicationTarget,
) -> bool {
    let mut published = published.clone();
    published.url = None;
    published == *intent
}

/// Publication can add a PR URL and a live-base observation; all preflight
/// target fields remain immutable.
fn same_preflight_publication_target(
    receipt: &crate::agent_task_finalization::AgentTaskPublicationTarget,
    intent: &crate::agent_task_finalization::AgentTaskPublicationTarget,
) -> bool {
    let mut receipt = receipt.clone();
    let mut intent = intent.clone();
    receipt.url = None;
    receipt.publication_base_sha = None;
    intent.url = None;
    intent.publication_base_sha = None;
    receipt == intent
}

fn manual_finalization_options(
    report: AgentTaskPrFinalizationReport,
) -> Result<AgentTaskPrFinalizationOptions> {
    validate_manual_preflight_report(&report, &report.run_id)?;
    let target = &report.publication_intent.target;
    let path = target.path.clone().filter(|path| path == &report.path);
    let base = target.base.clone().filter(|base| base == &report.base);
    let head = target.head.clone().filter(|head| head == &report.head);
    let verified_base_sha = target.verified_base_sha.clone();
    let expected_candidate_sha = report
        .publication_proof
        .git_identity
        .as_ref()
        .and_then(|identity| identity.commit_sha.clone());
    if path.is_none()
        || base.is_none()
        || head.is_none()
        || verified_base_sha.as_deref().unwrap_or_default().is_empty()
        || expected_candidate_sha
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "cook_finalization",
            "persisted manual finalization dossier is missing its immutable publication target",
            None,
            None,
        ));
    }
    let path = path.expect("validated path");
    Ok(AgentTaskPrFinalizationOptions {
        path: path.clone(),
        run_id: report.run_id,
        base: base.expect("validated base"),
        verified_base_sha,
        head,
        title: report.title,
        // An immutable recovered candidate must never reach commit mutation.
        commit_message: "recovered manual finalization".to_string(),
        gate_results: report.gate_results,
        normalized_gate_results: report.normalized_gate_results,
        accept_inherited_failures: report.accept_inherited_failures,
        changed_files: report.changed_files,
        evidence: report.evidence,
        ai_used_for: report.review_dossier.ai_assistance.used_for.clone(),
        review_dossier: report.review_dossier,
        review_profile: resolve_review_profile(&path)?,
        manual_finalization: true,
        expected_candidate_sha,
        protected_branches: vec![
            "main".to_string(),
            "master".to_string(),
            "trunk".to_string(),
        ],
        draft_pr: false,
    })
}

fn validate_manual_preflight_report(
    report: &AgentTaskPrFinalizationReport,
    run_id: &str,
) -> Result<()> {
    if report.run_id != run_id {
        return Err(Error::validation_invalid_argument(
            "manual_finalization_intent.run_id",
            "manual finalization dossier belongs to a different durable run",
            Some(run_id.to_string()),
            None,
        ));
    }
    if report.schema != crate::agent_task_finalization::AGENT_TASK_PR_FINALIZATION_SCHEMA
        || report.status != "validated"
        || !report.manual_finalization
        || report.title.trim().is_empty()
        || report.publication_proof.schema
            != crate::agent_task_finalization::AGENT_TASK_PUBLICATION_PROOF_SCHEMA
        || report.publication_proof.status != "validated"
        || report.publication_proof.intent_schema != report.publication_intent.schema
        || report.publication_proof.adapter_action.is_some()
        || report.publication_proof.adapter_ref.is_some()
        || report.proof != report.publication_intent.proof
        || report.proof != report.publication_proof.proof
        || report.run_id != report.publication_intent.run_id
        || report.run_id != report.publication_proof.run_id
        || report.changed_files != report.publication_intent.changed_files
        || report.changed_files != report.finalization_outcome.changed_files
        || report.publication_intent.target != report.publication_proof.target
        || report.publication_intent.target != report.finalization_outcome.target
        || report.finalization_outcome.schema
            != crate::agent_task_finalization::AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA
        || report.finalization_outcome.run_id != report.run_id
        || report.finalization_outcome.status != "validated"
        || report.finalization_outcome.publication_status != "validated"
        || report.finalization_outcome.publication_action != "none"
        || report.finalization_outcome.base != report.base
        || report.finalization_outcome.head != report.head
        || report.finalization_outcome.published
        || report.finalization_outcome.committed
        || report.finalization_outcome.pushed
    {
        return Err(Error::validation_invalid_argument(
            "cook_finalization",
            "persisted manual finalization dossier failed integrity validation",
            Some(run_id.to_string()),
            None,
        ));
    }
    validate_publication_intent(&report.publication_intent).map_err(|_| {
        Error::validation_invalid_argument(
            "cook_finalization",
            "persisted manual finalization dossier has an invalid publication intent",
            None,
            None,
        )
    })
}

fn cook_review_dossier_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    promotion: &AgentTaskPromotionReport,
    successful_run_id: &str,
) -> Result<AgentTaskReviewDossier> {
    // A form-only run owns reviewer metadata but carries forward the durable
    // gate proof while its authenticated source owns candidate scope.
    let terminal_promotion = promotion;
    let source_run_id = terminal_promotion
        .provenance
        .pointer("/cook_follow_up/source_run_id")
        .and_then(Value::as_str);
    let implementation_promotion = source_run_id
        .map(|run_id| {
            persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)?.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion.provenance.cook_follow_up.source_run_id",
                    "form-only finalization requires its source attempt's persisted promotion",
                    Some(run_id.to_string()),
                    None,
                )
            })
        })
        .transpose()?;
    let promotion = implementation_promotion
        .as_ref()
        .unwrap_or(terminal_promotion);
    // A form-only continuation carries its source's normalized gate proof.
    // The terminal record is therefore the durable reviewer-proof boundary.
    let verification_promotion = terminal_promotion;
    let changed_files = promotion.changed_files.join(", ");
    let changed_file_count = promotion.changed_files.len();
    let gate_count = verification_promotion.gate_results.len();
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
    let how_to_test = verification_promotion
        .deterministic_gates
        .iter()
        .filter_map(|gate| {
            let [shell, flag, command] = gate.command.as_slice() else {
                return None;
            };
            (shell == "sh"
                && flag == "-lc"
                && verification_promotion.has_visible_passed_gate_for_command(command)
                && crate::agent_task_review_dossier::reviewer_runnable_command(command))
            .then(|| AgentTaskReviewTestStep {
                command: command.clone(),
                expected: "passes as recorded by Cook's deterministic gate".to_string(),
            })
        })
        .collect::<Vec<_>>();
    if how_to_test.is_empty() {
        return Err(Error::validation_invalid_argument(
            "verification",
            "Cook cannot publish a test command without matching successful visible durable gate evidence bound to the promoted candidate",
            None,
            None,
        ));
    }

    // A form-only follow-up owns reviewer metadata, not the candidate it carries
    // forward. Resolve the persisted Cook lineage so that follow-up prose cannot
    // erase the implementation attempt that produced the delivered patch.
    let terminal_form = review_form_for_finalization_in_store(lifecycle_store, successful_run_id)?;
    let verified_commands = terminal_form.verify_against_promotion(verification_promotion)?;
    let lineage = cook_ai_lineage_with_stores(
        store,
        lifecycle_store,
        options,
        terminal_promotion,
        successful_run_id,
        &terminal_form,
    )?;
    let mut evidence = vec![
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
    ];
    // Form-only continuations may substitute the implementation promotion for
    // candidate metadata; the persisted terminal record remains recovery proof.
    if let Some(replacement) = lifecycle_store
        .read_record(successful_run_id)
        .ok()
        .and_then(|record| {
            record
                .metadata
                .get("promotions")
                .and_then(Value::as_array)
                .and_then(|promotions| {
                    promotions.iter().rev().find_map(|promotion| {
                        promotion
                            .pointer("/provenance/replacement_gate_proof")
                            .cloned()
                    })
                })
        })
    {
        let original_gates = replacement["original_history"]["deterministic_gate_count"]
            .as_u64()
            .unwrap_or_default();
        evidence.extend([
            AgentTaskReviewEvidence {
                summary: format!(
                    "Original infrastructure-invalid verification retained: {original_gates} failed gate record(s); inspect durable gate details."
                ),
                // The durable record is operator-only. Keep this reviewer-facing
                // provenance as text so `enrich_dossier` does not remove it.
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: format!(
                    "Replacement candidate-bound verification: {gate_count} gate(s) completed green with matching command evidence."
                ),
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: "Explicit operator authorization for external replacement proof was recorded."
                    .to_string(),
                url: None,
            },
        ]);
    }
    Ok(AgentTaskReviewDossier {
        schema: "homeboy/agent-task-review-dossier/v1".to_string(),
        summary: lineage.summary,
        what_changed: lineage.what_changed,
        how_to_test,
        compatibility: lineage.compatibility,
        // Deterministic evidence: orchestrator-owned. The task objective, scope,
        // gate count, and adoption provenance are factual records, not prose the
        // AI restates.
        evidence,
        verified_commands,
        changed_public_contracts: Vec::new(),
        public_contract_evidence: None,
        ai_assistance: AgentTaskReviewAiAssistance {
            // Deterministic: the orchestrator knows whether/what tool+model ran,
            // and attributes Homeboy as the harness that drove the change.
            used: true,
            tool: lineage.tool,
            model: lineage.model,
            used_for: lineage.used_for,
        },
        source_relationships: Vec::new(),
        overrides: Vec::new(),
    })
}

struct CookAiLineage {
    summary: String,
    what_changed: Vec<String>,
    compatibility: String,
    tool: String,
    model: String,
    used_for: String,
}

struct CookAttemptExecution {
    task_summary: String,
    form: Option<crate::agent_task_review_dossier::AiFilledReviewForm>,
    tool: String,
    model: Option<String>,
    review_form_only: bool,
}

fn selected_outcome_for_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<crate::agent_task::AgentTaskOutcome> {
    let aggregate = lifecycle_store.read_aggregate(run_id)?;
    aggregate
        .selected_outcome()
        .cloned()
        .or_else(|| {
            (aggregate.outcomes.len() == 1)
                .then(|| aggregate.outcomes.first())
                .flatten()
                .cloned()
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "Cook lineage attempt has no selected provider outcome",
                Some(run_id.to_string()),
                None,
            )
        })
}

/// Read provider identity from the durable attempt record rather than the
/// recipe or finalization flags. A pre-provider adopted source has task context
/// but no executed model; any attempt used as an execution is validated by its
/// caller before disclosure.
fn cook_attempt_execution_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<CookAttemptExecution> {
    let plan = lifecycle_store.read_controller_plan(run_id)?;
    let record = lifecycle_store.read_record(run_id)?;
    if let Some(task) = plan.tasks.iter().find(|task| {
        agent_task_lifecycle::candidate_adoption_recovery_outcome(&record, task).is_some()
    }) {
        if task.executor.backend.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "provider_tool",
                "Cook lineage attempt has no dispatched provider tool",
                Some(run_id.to_string()),
                None,
            ));
        }
        return Ok(CookAttemptExecution {
            task_summary: task
                .instructions
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("Delivered the authenticated Cook candidate.")
                .to_string(),
            form: None,
            tool: task.executor.backend.clone(),
            model: None,
            review_form_only: false,
        });
    }
    let outcome = selected_outcome_for_attempt_in_store(lifecycle_store, run_id)?;
    let task = plan
        .tasks
        .iter()
        .find(|task| task.task_id == outcome.task_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "Cook lineage outcome does not match a task in its durable execution plan",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let terminal = super::cook_pre_execution::terminal_executor_identity(&outcome, &plan, None);
    let model = terminal
        .as_ref()
        .and_then(|identity| identity.model.as_deref())
        .or_else(|| outcome.selected_model());
    let requested_model = outcome.metadata["model_identity"]["requested"].as_str();
    let resolved_model = outcome.metadata["model_identity"]["resolved"].as_str();
    let provider_reported_model = outcome.metadata["model_identity"]["provider_reported"].as_str();
    if model.is_none()
        && provider_reported_model.is_none()
        && requested_model
            .zip(resolved_model)
            .is_some_and(|(requested, resolved)| requested != resolved)
    {
        return Err(Error::validation_invalid_argument(
            "provider_model",
            "Cook lineage has unresolved requested and resolved model disagreement without a provider-reported executed model",
            Some(run_id.to_string()),
            None,
        ));
    }
    if task.executor.backend.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider_tool",
            "Cook lineage attempt has no dispatched provider tool",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(CookAttemptExecution {
        task_summary: task
            .instructions
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("Delivered the authenticated Cook candidate.")
            .to_string(),
        form: crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(
            &outcome.outputs,
        )?
        .filter(|form| form.validate().is_ok()),
        tool: terminal
            .as_ref()
            .map(|identity| identity.backend.clone())
            .unwrap_or_else(|| task.executor.backend.clone()),
        model: model.map(str::to_string),
        review_form_only: task.inputs["cook_loop"]["review_form_required"] == true,
    })
}

fn required_execution_model(execution: &CookAttemptExecution, run_id: &str) -> Result<String> {
    execution.model.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "provider_model",
            "Cook lineage attempt has no concrete executed model",
            Some(run_id.to_string()),
            None,
        )
    })
}

fn cook_ai_lineage_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    promotion: &AgentTaskPromotionReport,
    successful_run_id: &str,
    terminal_form: &crate::agent_task_review_dossier::AiFilledReviewForm,
) -> Result<CookAiLineage> {
    let recipe = store.load_recipe(&options.cook_id)?;
    let mut attempts = recipe.attempts;
    attempts.sort_by_key(|attempt| attempt.attempt);
    let Some(terminal_index) = attempts
        .iter()
        .position(|attempt| attempt.run_id == successful_run_id)
    else {
        return Err(Error::validation_invalid_argument(
            "successful_run_id",
            "finalizing Cook run is absent from its persisted recipe lineage",
            Some(successful_run_id.to_string()),
            None,
        ));
    };
    attempts.truncate(terminal_index + 1);
    // Preserve the byte-for-byte single-attempt output. Multi-attempt form-only
    // recovery instead makes each authenticated role visible to reviewers.
    if terminal_index == 0 {
        let execution = cook_attempt_execution_in_store(lifecycle_store, successful_run_id)?;
        let model = required_execution_model(&execution, successful_run_id)?;
        return Ok(CookAiLineage {
            summary: terminal_form.summary.clone(),
            what_changed: terminal_form.what_changed.clone(),
            compatibility: terminal_form.compatibility.clone(),
            tool: crate::agent_task_review_dossier::homeboy_tool_disclosure(&execution.tool),
            model,
            used_for: terminal_form.used_for.clone(),
        });
    }
    let terminal = cook_attempt_execution_in_store(lifecycle_store, successful_run_id)?;
    let terminal_model = required_execution_model(&terminal, successful_run_id)?;
    if !terminal.review_form_only {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "multi-attempt Cook finalization only composes role disclosures for a metadata-only review-form follow-up",
            Some(successful_run_id.to_string()),
            None,
        ));
    }
    let source_run_id = promotion
        .provenance
        .pointer("/cook_follow_up/source_run_id")
        .and_then(Value::as_str)
        .filter(|source_run_id| *source_run_id != successful_run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.cook_follow_up.source_run_id",
                "multi-attempt Cook finalization requires an authenticated form-only source run",
                Some(successful_run_id.to_string()),
                None,
            )
        })?;
    let implementation = attempts
        .iter()
        .find(|attempt| attempt.run_id == source_run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "form-only follow-up source run is absent from the finalized Cook lineage",
                Some(source_run_id.to_string()),
                None,
            )
        })?;
    if implementation.attempt == attempts[terminal_index].attempt {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "form-only follow-up cannot attribute its own run as the implementation attempt",
            Some(source_run_id.to_string()),
            None,
        ));
    }
    let implementation = cook_attempt_execution_in_store(lifecycle_store, &implementation.run_id)?;
    let changed = promotion
        .changed_files
        .iter()
        .map(|path| format!("Updated `{path}` in the delivered candidate."))
        .collect::<Vec<_>>();
    let (summary, what_changed, compatibility) = implementation
        .form
        .map(|form| (form.summary, form.what_changed, form.compatibility))
        .unwrap_or_else(|| {
            (
                implementation.task_summary.clone(),
                changed,
                format!(
                    "Delivered candidate verified with {} deterministic Cook gate(s); no separate compatibility assessment was recorded by the implementation attempt.",
                    promotion.gate_results.len()
                ),
            )
        });
    let (tool, model, used_for) = implementation.model.map_or_else(
        || {
            let tool = crate::agent_task_review_dossier::homeboy_tool_disclosure(&terminal.tool);
            (
                tool.clone(),
                terminal_model.clone(),
                format!(
                    "Review form: {tool} reviewed the validated adopted candidate and supplied the reviewer metadata."
                ),
            )
        },
        |implementation_model| {
            let implementation_tool =
                crate::agent_task_review_dossier::homeboy_tool_disclosure(&implementation.tool);
            let terminal_tool =
                crate::agent_task_review_dossier::homeboy_tool_disclosure(&terminal.tool);
            (
                format!("Implementation: {implementation_tool}; review form: {terminal_tool}"),
                format!(
                    "Implementation: {implementation_model}; review form: {terminal_model}"
                ),
                format!(
                    "Implementation: {implementation_tool} authored the delivered candidate changes and deterministic verification evidence. Review form: {terminal_tool} reviewed the validated candidate and supplied the reviewer metadata."
                ),
            )
        },
    );
    Ok(CookAiLineage {
        summary,
        what_changed,
        compatibility,
        tool,
        model,
        used_for,
    })
}

/// Load and validate the AI-authored review form for a finalizing run.
///
/// The cook loop's review-form gate guarantees a valid form before finalization
/// is reached; this re-reads it from the terminal outcome as the single source
/// of the reviewer-facing prose. Its absence/invalidity here is an invariant
/// violation (the gate would have looped), surfaced as a hard error rather than
/// silently falling back to machine-templated prose.
fn review_form_for_finalization_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<crate::agent_task_review_dossier::AiFilledReviewForm> {
    let outcome = selected_outcome_for_attempt_in_store(lifecycle_store, run_id)?;
    let form = crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(
        &outcome.outputs,
    )?
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

/// Every field a Cook report is built from, named at the call site.
///
/// This is a struct input rather than a positional argument list because
/// `invocation_latest_run_id` is the field that decides whether the report —
/// and every recovery command derived from it — points at THIS invocation's
/// run or at some prior session's run recorded in the cross-invocation Cook
/// index. When it was an optional trailing positional argument, two call sites
/// silently passed `None` and reported a stale run id to the orchestrator. A
/// required named field makes that class of mistake a compile error.
#[non_exhaustive]
pub(crate) struct CookReportInput<'a> {
    pub cook_id: String,
    pub status: &'a str,
    /// Whether this exit handed the work to a durable owner or stopped.
    ///
    /// Required, and deliberately not defaulted: this is the fact the
    /// orchestrator's completion depends on, and the exit building the report
    /// is the only place that knows it.
    pub disposition: CookDisposition,
    pub attempts: Vec<AgentTaskCookAttemptReport>,
    pub finalization: Option<Value>,
    pub stop_reason: Option<String>,
    pub exit_code: i32,
    /// The run this invocation is reporting on. `None` is legal only when the
    /// caller genuinely has no invocation-scoped run id — it falls back to the
    /// cross-invocation Cook index, which may name a prior session's run.
    pub invocation_latest_run_id: Option<&'a str>,
}

pub(crate) fn cook_report(input: CookReportInput<'_>) -> AgentTaskRunResult<AgentTaskCookReport> {
    let CookReportInput {
        cook_id,
        status,
        disposition,
        attempts,
        finalization,
        stop_reason,
        exit_code,
        invocation_latest_run_id,
    } = input;
    // Defense in depth, not a second source of truth: `disposition` remains
    // authoritative in release builds. This only catches an exit whose
    // declared disposition contradicts the status it reports, which is a
    // producer bug rather than a condition to recover from.
    debug_assert!(
        !CookStatus::from_status(status).is_in_flight() || disposition == CookDisposition::InFlight,
        "cook exit reported in-flight status {status:?} but declared {disposition:?}"
    );
    let history_run_ids = agent_task_lifecycle::cook_index(&cook_id)
        .map(|index| {
            index
                .attempts
                .into_iter()
                .map(|attempt| attempt.run_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| {
            super::load_recipe(&cook_id)
                .map(|recipe| {
                    recipe
                        .attempts
                        .into_iter()
                        .map(|attempt| attempt.run_id)
                        .collect()
                })
                .unwrap_or_default()
        });
    let latest_run_id = invocation_latest_run_id
        .filter(|run_id| !run_id.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            agent_task_lifecycle::cook_index(&cook_id)
                .ok()
                .map(|index| index.latest_run_id)
        })
        .or_else(|| {
            // A recipe is persisted before its lifecycle record and Cook index. If
            // materialization fails in that window, its final immutable attempt is
            // still the only durable identity we can safely report.
            super::load_recipe(&cook_id)
                .ok()
                .and_then(|recipe| recipe.attempts.last().map(|attempt| attempt.run_id.clone()))
        });
    // A controller failure produces no attempt report, but its run id is still
    // part of THIS invocation. Union it in so invocation scope is never empty
    // just because the failure happened outside an attempt.
    let mut invocation_run_ids: Vec<String> = attempts
        .iter()
        .map(|attempt| attempt.run_id.clone())
        .collect();
    if let Some(run_id) = invocation_latest_run_id {
        if !invocation_run_ids.iter().any(|known| known == run_id) {
            invocation_run_ids.push(run_id.to_string());
        }
    }
    let selected_candidate = cook_selected_candidate_provenance(&cook_id, &invocation_run_ids);
    let failure_context = (exit_code != 0)
        .then(|| cook_failure_context(&cook_id, latest_run_id.as_deref(), status))
        .flatten();
    AgentTaskRunResult {
        value: AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id,
            latest_run_id,
            history_run_ids,
            invocation_run_ids,
            status: status.to_string(),
            disposition,
            attempts,
            finalization,
            intentional_no_change: None,
            selected_candidate,
            stop_reason,
            terminal_phase: None,
            terminal_failure_classification: None,
            moving_base_recovery: None,
            failure_context,
        },
        exit_code,
    }
}

/// `select_cook_candidate` deliberately selects across the whole Cook index
/// with no invocation scope — adopting the best candidate across attempts is
/// what a Cook is for. But a report can therefore say `latest_run_id: <this
/// run>` while `selected_candidate.run_id` names a prior session's run, and
/// `cook_failure_context` prefers that cross-invocation id when it builds
/// recovery commands. The orchestrator could not previously tell the two apart.
/// `invocation_scoped` states it explicitly. The selection itself is unchanged.
fn cook_selected_candidate_provenance(
    cook_id: &str,
    invocation_run_ids: &[String],
) -> Option<Value> {
    let selection = agent_task_lifecycle::select_cook_candidate(cook_id).ok()?;
    let mut value = serde_json::to_value(&selection).ok()?;
    value["invocation_scoped"] = serde_json::json!(
        !selection.run_id.is_empty()
            && invocation_run_ids
                .iter()
                .any(|run_id| run_id == &selection.run_id)
    );
    if selection.incomplete || selection.run_id.is_empty() {
        return Some(value);
    }
    let record = agent_task_lifecycle::exact_record(&selection.run_id).ok()?;
    if let Some(promotion) = record.metadata.get("latest_promotion") {
        value["applied_promotion"] = serde_json::json!({
            "identity": promotion.pointer("/patch_artifact/sha256"),
            "destination": promotion.get("to_worktree"),
            "fingerprint": promotion.pointer("/provenance/candidate"),
        });
    }
    Some(value)
}

/// Build recovery coordinates from durable controller records only. Provider
/// output, gate output, and filesystem paths stay behind `diagnose` so a failed
/// command envelope cannot disclose private evidence.
fn cook_failure_context(
    cook_id: &str,
    latest_run_id: Option<&str>,
    status: &str,
) -> Option<super::AgentTaskCookFailureContext> {
    let recipe = super::load_recipe(cook_id).ok()?;
    let chronological_latest_run_id = latest_run_id
        .map(str::to_string)
        .or_else(|| recipe.attempts.last().map(|attempt| attempt.run_id.clone()))?;
    let selection = agent_task_lifecycle::select_cook_candidate(cook_id).ok();
    let selected_run_id = selection
        .as_ref()
        .filter(|selection| !selection.incomplete && !selection.run_id.is_empty())
        .map(|selection| selection.run_id.clone());
    // Candidate selection intentionally spans Cook history, but recovery is an
    // operation on this invocation. Its legality, phase, diagnostics, and every
    // emitted command must therefore come from this exact durable record.
    let record_run_id = chronological_latest_run_id.as_str();
    let record = agent_task_lifecycle::exact_record(record_run_id).ok();
    let provider_executions_consumed = recipe
        .attempts
        .iter()
        // Recipe entries are historical attempt identities. `status` resolves a
        // Cook ID alias to its latest attempt, which can count that later
        // provider execution again when an earlier preflight failure used the
        // Cook ID as its run ID.
        .filter_map(|attempt| agent_task_lifecycle::exact_record(&attempt.run_id).ok())
        .map(|record| {
            record.metadata["provider_executions_consumed"]
                .as_u64()
                .unwrap_or_else(|| {
                    record.metadata["provider_executions"]
                        .as_array()
                        .map(|executions| executions.len() as u64)
                        .unwrap_or_default()
                })
        })
        .sum();
    let lifecycle_state = record
        .as_ref()
        .map(|record| format!("{:?}", record.state))
        .unwrap_or_else(|| "recipe_persisted_without_lifecycle_record".to_string());
    let recovery_legal = record.is_some();
    let promotion_claim =
        agent_task_lifecycle::operation_claim(record_run_id, &format!("promote:{record_run_id}"))
            .ok()
            .flatten();
    let blocking_claim = promotion_claim.as_ref().and_then(|claim| {
        (claim.state == agent_task_lifecycle::ClaimState::Running)
            .then(|| serde_json::to_value(claim).unwrap_or(Value::Null))
    });
    let promotion_diagnostic = promotion_claim
        .as_ref()
        .filter(|claim| claim.state == agent_task_lifecycle::ClaimState::Failed)
        .and_then(|claim| claim.result.clone());
    let promotion = record
        .as_ref()
        .and_then(|record| record.metadata.get("latest_promotion"));
    let finalization_claim = promotion
        .and_then(|promotion| {
            promotion
                .pointer("/patch_artifact/sha256")
                .and_then(Value::as_str)
        })
        .and_then(|sha| {
            agent_task_lifecycle::operation_claim(
                record_run_id,
                &format!("finalize:{record_run_id}:{sha}"),
            )
            .ok()
            .flatten()
        })
        .or_else(|| {
            agent_task_lifecycle::operation_claim(
                record_run_id,
                &format!("finalize:{record_run_id}"),
            )
            .ok()
            .flatten()
        });
    let finalization_diagnostic = finalization_claim
        .as_ref()
        .filter(|claim| claim.state == agent_task_lifecycle::ClaimState::Failed)
        .and_then(|claim| claim.result.clone());
    let promotion_gate_failed = promotion
        .and_then(|promotion| promotion.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "gate_failed" | "no_op_gate_failed"));
    let progress_phase = record
        .as_ref()
        .and_then(|record| record.metadata.pointer("/cook_progress/phase"))
        .and_then(Value::as_str);
    let continuation_admission = record
        .as_ref()
        .and_then(|record| record.metadata.get("cook_continuation_admission"))
        .cloned();
    let controller_diagnostic = record
        .as_ref()
        .and_then(|record| record.metadata.get("cook_controller_failure"))
        .cloned();
    let (phase, reason_code, diagnostic) = if blocking_claim.is_some() {
        (
            "promotion".to_string(),
            "operation_in_progress".to_string(),
            None,
        )
    } else if let Some(diagnostic) = promotion_diagnostic.as_ref() {
        (
            "promotion".to_string(),
            diagnostic["code"]
                .as_str()
                .unwrap_or("promotion_rejected")
                .to_string(),
            Some(diagnostic.clone()),
        )
    } else if promotion_gate_failed
        || matches!(
            status,
            "gate_failed" | "no_op_gate_failed" | "deterministic_gate_failure"
        )
    {
        (
            "deterministic_gate".to_string(),
            "gate_failed".to_string(),
            None,
        )
    } else if let Some(diagnostic) = finalization_diagnostic.as_ref() {
        (
            "finalization".to_string(),
            diagnostic["code"]
                .as_str()
                .unwrap_or("finalization_rejected")
                .to_string(),
            Some(diagnostic.clone()),
        )
    } else if progress_phase == Some("finalization")
        || matches!(status, "finalization_failed" | "finalization_failure")
    {
        (
            "finalization".to_string(),
            "finalization_incomplete".to_string(),
            None,
        )
    } else if let Some(diagnostic) = controller_diagnostic {
        (
            "controller".to_string(),
            diagnostic
                .pointer("/deepest_cause/code")
                .or_else(|| diagnostic.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("controller_failure")
                .to_string(),
            Some(diagnostic),
        )
    } else {
        (
            "provider".to_string(),
            lifecycle_state.to_ascii_lowercase(),
            None,
        )
    };
    let recovery_actions = cook_recovery_actions(
        status,
        &chronological_latest_run_id,
        recovery_legal,
        blocking_claim.is_some(),
        provider_executions_consumed
            < recipe.retry_budget["max_attempts"]
                .as_u64()
                .unwrap_or_default(),
        exact_checkpoint_candidate_mismatch(&diagnostic),
        ambiguous_promotion_artifact_ids(record_run_id, promotion_diagnostic.as_ref(), &recipe),
        record.as_ref().and_then(lab_handoff_runtime_recovery),
    );
    let promotion_provenance = promotion.cloned();
    Some(super::AgentTaskCookFailureContext {
        cook_id: cook_id.to_string(),
        latest_run_id: chronological_latest_run_id,
        selected_run_id,
        selected_task_id: selection
            .as_ref()
            .and_then(|selection| selection.selected_task_id.clone()),
        selected_artifact_id: selection
            .as_ref()
            .and_then(|selection| selection.selected_artifact_id.clone()),
        promotion_provenance,
        durable_recipe_ref: format!("homeboy://agent-task/cooks/{cook_id}/recipe"),
        lifecycle_state,
        phase,
        reason_code,
        diagnostic,
        continuation_admission,
        blocking_claim,
        provider_budget_consumed: provider_executions_consumed > 0,
        provider_executions_consumed,
        recovery_legal,
        recovery_reason: recovery_actions.reason,
        next_actions: recovery_actions.next_actions,
        legal_actions: recovery_actions.legal_actions,
    })
}

/// Artifact IDs are durable controller metadata. Expose them only when the
/// promotion claim proves selection was the blocker, so a recovery command is
/// executable rather than a replay of the known-invalid promotion.
fn ambiguous_promotion_artifact_ids(
    run_id: &str,
    diagnostic: Option<&Value>,
    recipe: &super::AgentTaskCookRecipe,
) -> Vec<String> {
    let is_ambiguous_selection = diagnostic.is_some_and(|diagnostic| {
        diagnostic.pointer("/details/field").and_then(Value::as_str) == Some("artifact_id")
            && diagnostic
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| {
                    message.contains("multiple patch artifacts")
                        || message.contains("distinct actionable patches")
                        || message.contains("distinct canonical patch candidates")
                })
    });
    if !is_ambiguous_selection {
        return Vec::new();
    }
    let Ok(aggregate) = agent_task_lifecycle::read_attempt_aggregate(run_id) else {
        return Vec::new();
    };
    let Some(outcome) = aggregate.selected_outcome().or_else(|| {
        (aggregate.outcomes.len() == 1)
            .then(|| aggregate.outcomes.first())
            .flatten()
    }) else {
        return Vec::new();
    };
    if outcome.status != crate::agent_task::AgentTaskOutcomeStatus::CandidateRecoverable {
        return outcome
            .artifacts
            .iter()
            .filter(|artifact| {
                crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
            })
            .map(|artifact| artifact.id.clone())
            .collect();
    }
    if !recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == run_id)
    {
        return Vec::new();
    }
    let Ok(recipe_options) = super::reconstruct_options(recipe) else {
        return Vec::new();
    };
    let Ok((source, source_path)) = promotion_source(run_id) else {
        return Vec::new();
    };
    canonical_recoverable_patch_artifacts(
        outcome,
        &AgentTaskPromotionOptions {
            source,
            source_run_id: Some(run_id.to_string()),
            source_path,
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: recipe_options.to_worktree,
            task_id: selected_candidate_task_id(run_id).ok().flatten(),
            artifact_id: None,
            dry_run: false,
            gates: crate::agent_task_gate::VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
    )
    .map(|canonical| {
        canonical
            .artifacts
            .into_iter()
            .map(|artifact| artifact.id)
            .collect()
    })
    .unwrap_or_default()
}

struct CookRecoveryActions {
    legal_actions: Vec<super::AgentTaskCookRecoveryAction>,
    next_actions: Vec<super::AgentTaskCookRecoveryAction>,
    reason: String,
}

/// Standard Cook actions are built once. Lab runtime repair is legal only for
/// the exact failed admission record, so it is kept out of `next_actions`.
fn cook_recovery_actions(
    status: &str,
    run_id: &str,
    recovery_legal: bool,
    blocking_claim: bool,
    provider_retry_available: bool,
    exact_checkpoint_candidate_mismatch: bool,
    ambiguous_artifact_ids: Vec<String>,
    lab_runtime_recovery: Option<agent_task_lifecycle::AgentTaskLabRuntimeRecovery>,
) -> CookRecoveryActions {
    if !recovery_legal {
        return CookRecoveryActions {
            legal_actions: Vec::new(),
            next_actions: Vec::new(),
            reason: "No recovery command is legal because the durable recipe has no lifecycle record. Start a fresh Cook after preserving the recipe reference for investigation.".to_string(),
        };
    }

    let continuation_eligible = match status {
        "completed"
        | "review_ready"
        | "green_no_finalize"
        | "intentional_no_change"
        | "execution_budget_exhausted"
        | "retries_exhausted" => false,
        "gate_failed" | "no_op_gate_failed" => provider_retry_available,
        _ => true,
    };
    let mut actions = vec![
        super::AgentTaskCookRecoveryAction {
            action: "status".to_string(),
            command: format!("homeboy agent-task status {run_id} --full"),
        },
        super::AgentTaskCookRecoveryAction {
            action: "diagnose".to_string(),
            command: format!("homeboy agent-task diagnose {run_id}"),
        },
    ];
    if blocking_claim {
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "reconcile".to_string(),
            command: format!("homeboy agent-task reconcile {run_id} --dry-run"),
        });
    }
    if exact_checkpoint_candidate_mismatch {
        // The checkpoint authenticates one exact destination candidate. A
        // replacement run preserves that immutable evidence without claiming it
        // can safely continue against a diverged worktree.
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "fork_replacement".to_string(),
            command: format!("homeboy agent-task retry {run_id} --run"),
        });
    } else if !ambiguous_artifact_ids.is_empty() {
        actions.extend(ambiguous_artifact_ids.into_iter().map(|artifact_id| {
            super::AgentTaskCookRecoveryAction {
                action: "resume_with_artifact".to_string(),
                command: super::cook_continue_command(None, run_id, true, Some(&artifact_id)),
            }
        }));
    } else if continuation_eligible {
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "resume".to_string(),
            command: super::cook_continue_command(None, run_id, false, None),
        });
    }
    let next_actions = actions.clone();
    if let Some(recovery) = lab_runtime_recovery {
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "refresh_lab_runtime".to_string(),
            command: recovery.command(),
        });
    }
    let names = actions
        .iter()
        .map(|action| action.action.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    CookRecoveryActions {
        reason: format!("Legal recovery actions for this Cook state: {names}."),
        legal_actions: actions,
        next_actions,
    }
}

fn exact_checkpoint_candidate_mismatch(diagnostic: &Option<Value>) -> bool {
    diagnostic
        .as_ref()
        .and_then(|diagnostic| diagnostic.pointer("/details/recovery/action"))
        .and_then(Value::as_str)
        == Some("fork_replacement")
}

#[cfg(test)]
mod recovery_action_tests {
    use super::*;

    #[test]
    fn recovery_actions_follow_the_cook_state_matrix() {
        let cases = [
            (
                "gate_failed",
                true,
                false,
                vec!["status", "diagnose", "resume"],
            ),
            (
                "execution_budget_exhausted",
                false,
                false,
                vec!["status", "diagnose"],
            ),
            (
                "verification_pending",
                false,
                false,
                vec!["status", "diagnose", "resume"],
            ),
            (
                "finalization_failed",
                false,
                false,
                vec!["status", "diagnose", "resume"],
            ),
            ("completed", false, false, vec!["status", "diagnose"]),
        ];

        for (status, retry_available, exact_checkpoint_candidate_mismatch, expected) in cases {
            let recovery = cook_recovery_actions(
                status,
                "cook-state-matrix-attempt-1",
                true,
                false,
                retry_available,
                exact_checkpoint_candidate_mismatch,
                Vec::new(),
                None,
            );
            let actions = recovery
                .legal_actions
                .iter()
                .map(|action| action.action.as_str())
                .collect::<Vec<_>>();
            assert_eq!(actions, expected, "{status}");
            assert_eq!(
                recovery.reason,
                format!(
                    "Legal recovery actions for this Cook state: {}.",
                    expected.join(", ")
                ),
                "{status} prose must be derived from the commands"
            );
            assert!(recovery.legal_actions.iter().all(|action| {
                action.command.starts_with("homeboy agent-task ")
                    && action.command.ends_with("cook-state-matrix-attempt-1")
                    || action.command
                        == "homeboy agent-task status cook-state-matrix-attempt-1 --full"
            }));
            assert_eq!(
                recovery.legal_actions.iter().any(|action| action.command
                    == "homeboy agent-task cook-continue cook-state-matrix-attempt-1"),
                expected.contains(&"resume"),
                "{status} must advertise cook-continue exactly when it can advance"
            );
            assert!(recovery.legal_actions.iter().all(|action| {
                !matches!(
                    action.action.as_str(),
                    "promote_selected_candidate"
                        | "review_selected_candidate"
                        | "finalize_selected_candidate"
                )
            }));
        }
    }

    #[test]
    fn exact_checkpoint_mismatch_offers_a_replacement_not_an_illegal_resume() {
        let recovery = cook_recovery_actions(
            "verification_pending",
            "checkpoint-mismatch-attempt-1",
            true,
            false,
            false,
            true,
            Vec::new(),
            None,
        );

        let actions = |actions: &[super::super::AgentTaskCookRecoveryAction]| {
            actions
                .iter()
                .map(|action| (action.action.clone(), action.command.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            actions(&recovery.legal_actions),
            vec![
                (
                    "status".to_string(),
                    "homeboy agent-task status checkpoint-mismatch-attempt-1 --full".to_string()
                ),
                (
                    "diagnose".to_string(),
                    "homeboy agent-task diagnose checkpoint-mismatch-attempt-1".to_string()
                ),
                (
                    "fork_replacement".to_string(),
                    "homeboy agent-task retry checkpoint-mismatch-attempt-1 --run".to_string()
                ),
            ]
        );
        assert_eq!(
            actions(&recovery.next_actions),
            actions(&recovery.legal_actions)
        );
    }

    #[test]
    fn exact_checkpoint_mismatch_matches_only_the_typed_recovery_action() {
        assert!(exact_checkpoint_candidate_mismatch(&Some(
            serde_json::json!({
                "details": { "recovery": { "action": "fork_replacement" } },
            })
        )));
        assert!(!exact_checkpoint_candidate_mismatch(&Some(
            serde_json::json!({
                "details": { "recovery": { "action": "resume" } },
            })
        )));
    }

    #[test]
    fn ambiguous_artifact_selection_advertises_only_selector_continuations() {
        let recovery = cook_recovery_actions(
            "promotion_failed",
            "ambiguous-attempt-1",
            true,
            false,
            false,
            false,
            vec!["first-patch".to_string(), "second-patch".to_string()],
            None,
        );

        assert_eq!(
            recovery
                .legal_actions
                .iter()
                .map(|action| action.command.as_str())
                .collect::<Vec<_>>(),
            vec![
                "homeboy agent-task status ambiguous-attempt-1 --full",
                "homeboy agent-task diagnose ambiguous-attempt-1",
                "homeboy agent-task cook-continue ambiguous-attempt-1 --rearm --artifact-id first-patch",
                "homeboy agent-task cook-continue ambiguous-attempt-1 --rearm --artifact-id second-patch",
            ]
        );
    }

    #[test]
    fn ambiguous_selector_actions_do_not_advertise_mime_shaped_or_invalid_artifacts() {
        let recovery = cook_recovery_actions(
            "promotion_failed",
            "ambiguous-attempt-1",
            true,
            false,
            false,
            false,
            vec!["canonical-patch".to_string()],
            None,
        );
        assert_eq!(
            recovery.legal_actions.last().map(|action| action.command.as_str()),
            Some(
                "homeboy agent-task cook-continue ambiguous-attempt-1 --rearm --artifact-id canonical-patch"
            )
        );
        assert!(!recovery
            .legal_actions
            .iter()
            .any(|action| action.command.contains("mime-shaped")));
    }
}

fn lab_handoff_runtime_recovery(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<agent_task_lifecycle::AgentTaskLabRuntimeRecovery> {
    if record
        .metadata
        .pointer("/pre_execution_failure/phase")?
        .as_str()
        != Some("lab_staging_controller")
    {
        return None;
    }
    let recovery: agent_task_lifecycle::AgentTaskLabRuntimeRecovery = serde_json::from_value(
        record
            .metadata
            .pointer("/pre_execution_failure/details/lab_handoff_runtime_recovery")?
            .clone(),
    )
    .ok()?;
    recovery.is_valid().then_some(recovery)
}

pub(crate) fn source_spec_path(spec: &str) -> Option<PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(PathBuf::from(spec.strip_prefix('@').unwrap_or(spec)))
}
