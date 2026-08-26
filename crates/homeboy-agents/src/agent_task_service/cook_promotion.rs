//! Agent-task cook promotion & finalization.
//!
//! Extracted from `cook.rs`: promotion-source resolution
//! (`promotion_source`/`source_spec_path`/`source_worktree_path`), the durable
//! promote-or-load boundary (`promote_attempt`/`promote_or_load_attempt_in_store`/
//! `persisted_promotion_for_attempt`), PR finalization
//! (`finalize_or_load_cook_pr*`/`finalize_cook_pr_with_backend`), the
//! `cook_report` builder, and small spec helpers. These sit downstream of a
//! terminal provider result and publish controller-owned state; grouping them
//! keeps the promote → finalize boundary in one place.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use homeboy_core::cook_status::{CookDisposition, CookStatus};
use homeboy_core::engine::canonical_json::canonical_json_bytes;
use homeboy_engine_primitives::content_hash;
use homeboy_engine_primitives::shell::{quote_arg, quote_args};

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
    resume_promoted_patch_replacement_gates_in_observation_store, AgentTaskPromotionCandidate,
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

pub(crate) fn component_workspace_path(
    options: &AgentTaskCookServiceOptions,
) -> Result<Option<PathBuf>> {
    let Some(source) = options.source_worktree_path.as_ref() else {
        return Ok(None);
    };
    let Some(component_cwd) = options
        .initial_plan
        .metadata
        .pointer("/gate_workspace/component_cwd")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let source = source.canonicalize().map_err(|error| {
        Error::validation_invalid_argument(
            "component_cwd",
            format!("canonicalize Cook source workspace: {error}"),
            Some(source.display().to_string()),
            None,
        )
    })?;
    let component =
        homeboy_core::resolve_contained_local_path(&source, component_cwd, "component_cwd")?;
    let component = component.canonicalize().map_err(|error| {
        Error::validation_invalid_argument(
            "component_cwd",
            format!("canonicalize Cook component workspace: {error}"),
            Some(component.display().to_string()),
            None,
        )
    })?;
    if !component.starts_with(&source) {
        return Err(Error::validation_invalid_argument(
            "component_cwd",
            "canonical Cook component workspace escapes its source workspace",
            Some(component.display().to_string()),
            None,
        ));
    }
    Ok(Some(component))
}

fn replacement_component_workspace(
    original: &AgentTaskPromotionReport,
    target: &std::path::Path,
) -> Result<Option<PathBuf>> {
    let Some(cwd) = original
        .deterministic_gates
        .iter()
        .find_map(|gate| gate.cwd.as_ref())
    else {
        return Ok(None);
    };
    let relative = PathBuf::from(&cwd.effective)
        .strip_prefix(&cwd.requested)
        .map_err(|_| {
            Error::validation_invalid_argument(
                "replacement_gate.cwd",
                "persisted effective gate cwd is outside its requested worktree root",
                Some(cwd.effective.clone()),
                None,
            )
        })?
        .to_path_buf();
    Ok(Some(homeboy_core::resolve_contained_local_path(
        target,
        relative,
        "replacement_gate.cwd",
    )?))
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
            source_worktree_path: component_workspace_path(options)?
                .or_else(|| options.source_worktree_path.clone()),
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

pub(crate) fn canonical_cook_patch_artifact_id_in_store(
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
        source_worktree_path: component_workspace_path(options)?
            .or_else(|| options.source_worktree_path.clone()),
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
            let (choices, comparison) = cook_candidate_comparison(
                outcome,
                artifacts,
                &canonical.patch_contents,
                canonical.omitted_candidate_count,
                canonical.omitted_patch_bytes,
                options,
                run_id,
            );
            Err(Error::new(
                homeboy_core::ErrorCode::ValidationInvalidArgument,
                "Cook found distinct canonical patch candidates; select one before promotion",
                json!({
                    "field": "artifact_id",
                    "state": "selection_required",
                    "selection_required": true,
                    "choices": choices,
                    "comparison": comparison,
                }),
            ))
        }
    }
}

const CANDIDATE_FILE_LIMIT: usize = 12;
const CANDIDATE_EVIDENCE_LIMIT: usize = 6;
const CANDIDATE_DIAGNOSTIC_LIMIT: usize = 6;
const CANDIDATE_JSON_BYTES_LIMIT: usize = 2048;

/// Project only facts present in canonical patch bytes and durable outcome
/// evidence. This deliberately does not infer behavior from generated prose.
fn cook_candidate_comparison(
    outcome: &crate::agent_task::AgentTaskOutcome,
    artifacts: &[crate::agent_task::AgentTaskArtifact],
    patches: &BTreeMap<String, String>,
    omitted_candidate_count: usize,
    omitted_patch_bytes: u64,
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> (Vec<Value>, Value) {
    let summaries = artifacts
        .iter()
        .map(|artifact| {
            let patch = patches
                .get(&artifact.id)
                .map(String::as_str)
                .unwrap_or_default();
            let stats = patch_stats(patch);
            let (test_evidence, omitted_test_evidence_count) = bounded_test_evidence(artifact);
            let mut risk_flags = patch_risk_flags(patch, &stats.all_files);
            if test_evidence.is_empty() {
                risk_flags.push("missing_test_evidence".to_string());
            }
            CandidateSummary {
                artifact,
                all_files: stats.all_files,
                file_count: stats.file_count,
                insertions: stats.insertions,
                deletions: stats.deletions,
                test_evidence,
                risk_flags,
                omitted_test_evidence_count,
            }
        })
        .collect::<Vec<_>>();
    let common_files = summaries
        .iter()
        .map(|summary| summary.all_files.clone())
        .reduce(|left, right| left.intersection(&right).cloned().collect())
        .unwrap_or_default();
    let recommendation = deterministic_recommendation(&summaries);
    let choices = summaries
        .iter()
        .map(|summary| {
            let unique_files = summary
                .all_files
                .iter()
                .filter(|file| !common_files.contains(*file))
                .cloned()
                .collect::<Vec<_>>();
            let rationale = recommendation.as_ref().and_then(|recommended| {
                (recommended == &summary.artifact.id).then_some(
                    "Only candidate with recorded test evidence and no artifact-derived risk flags.",
                )
            });
            json!({
                "artifact_id": summary.artifact.id,
                "sha256": summary.artifact.sha256,
                "patch_artifact": {
                    "path": summary.artifact.path,
                    "url": summary.artifact.url,
                },
                "provider": summary.artifact.metadata.get("provider_backend"),
                "model": outcome.selected_model(),
                "attempt": summary.artifact.metadata.get("producer_attempt"),
                "changed_files": preview(&summary.all_files),
                "changed_file_count": summary.file_count,
                "changed_files_omitted_count": summary.file_count.saturating_sub(CANDIDATE_FILE_LIMIT),
                "line_stats": { "insertions": summary.insertions, "deletions": summary.deletions },
                "diff_summary": format!(
                    "{} file(s), {} insertion(s), {} deletion(s)",
                    summary.file_count, summary.insertions, summary.deletions
                ),
                "test_evidence": summary.test_evidence,
                "test_evidence_omitted_count": summary.omitted_test_evidence_count,
                "overlap": {
                    "shared_changed_files": preview(&common_files),
                    "shared_changed_files_omitted_count": common_files.len().saturating_sub(CANDIDATE_FILE_LIMIT),
                },
                "differences": {
                    "unique_changed_files": preview(&unique_files.into_iter().collect()),
                    "unique_changed_files_omitted_count": summary.file_count.saturating_sub(common_files.len()).saturating_sub(CANDIDATE_FILE_LIMIT),
                },
                "risk_flags": summary.risk_flags,
                "recommendation": rationale.map(|rationale| json!({ "rationale": rationale })),
                "command": cook_promotion_command(options, run_id, &outcome.task_id, &summary.artifact.id),
            })
        })
        .collect();
    let (shared_evidence, omitted_evidence_count) = bounded_shared_evidence(outcome);
    let (shared_diagnostics, omitted_diagnostic_count) = bounded_diagnostics(outcome);
    (
        choices,
        json!({
            "shared_outcome_evidence": shared_evidence,
            "shared_outcome_evidence_omitted_count": omitted_evidence_count,
            "shared_outcome_diagnostics": shared_diagnostics,
            "shared_outcome_diagnostics_omitted_count": omitted_diagnostic_count,
            "omitted_candidate_count": omitted_candidate_count,
            "omitted_patch_bytes": omitted_patch_bytes,
        }),
    )
}

struct CandidateSummary<'a> {
    artifact: &'a crate::agent_task::AgentTaskArtifact,
    all_files: BTreeSet<String>,
    file_count: usize,
    insertions: usize,
    deletions: usize,
    test_evidence: Vec<Value>,
    risk_flags: Vec<String>,
    omitted_test_evidence_count: usize,
}

struct PatchStats {
    all_files: BTreeSet<String>,
    file_count: usize,
    insertions: usize,
    deletions: usize,
}

fn patch_stats(patch: &str) -> PatchStats {
    let mut all_files = BTreeSet::new();
    let mut insertions = 0;
    let mut deletions = 0;
    for line in patch.lines() {
        if let Some(path) = line
            .strip_prefix("diff --git a/")
            .and_then(|line| line.split_once(" b/").map(|(_, path)| path))
        {
            all_files.insert(path.to_string());
        } else if line.starts_with('+') && !line.starts_with("+++") {
            insertions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions += 1;
        }
    }
    let file_count = all_files.len();
    PatchStats {
        all_files,
        file_count,
        insertions,
        deletions,
    }
}

fn bounded_test_evidence(artifact: &crate::agent_task::AgentTaskArtifact) -> (Vec<Value>, usize) {
    let evidence = artifact
        .metadata
        .get("test_evidence")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    bounded_json_values(evidence, CANDIDATE_EVIDENCE_LIMIT)
}

fn bounded_shared_evidence(outcome: &crate::agent_task::AgentTaskOutcome) -> (Vec<Value>, usize) {
    bounded_json_values(
        outcome.evidence_refs.iter().map(|evidence| {
            json!({ "kind": evidence.kind, "uri": evidence.uri, "label": evidence.label })
        }).collect(),
        CANDIDATE_EVIDENCE_LIMIT,
    )
}

fn bounded_diagnostics(outcome: &crate::agent_task::AgentTaskOutcome) -> (Vec<Value>, usize) {
    bounded_json_values(
        outcome
            .diagnostics
            .iter()
            .map(|diagnostic| json!({ "class": diagnostic.class, "message": diagnostic.message }))
            .collect(),
        CANDIDATE_DIAGNOSTIC_LIMIT,
    )
}

fn bounded_json_values(values: Vec<Value>, limit: usize) -> (Vec<Value>, usize) {
    let omitted = values.len().saturating_sub(limit);
    (
        values
            .into_iter()
            .take(limit)
            .map(bounded_json_value)
            .collect(),
        omitted,
    )
}

fn bounded_json_value(value: Value) -> Value {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    (bytes.len() <= CANDIDATE_JSON_BYTES_LIMIT)
        .then_some(value)
        .unwrap_or_else(|| {
            json!({
                "omitted": "json_value_exceeds_byte_limit",
                "size_bytes": bytes.len(),
            })
        })
}

fn preview(files: &BTreeSet<String>) -> Vec<String> {
    files.iter().take(CANDIDATE_FILE_LIMIT).cloned().collect()
}

fn patch_risk_flags(patch: &str, files: &BTreeSet<String>) -> Vec<String> {
    let mut flags = BTreeSet::new();
    if files.iter().any(|file| {
        file.starts_with(".github/workflows/")
            || file.ends_with("/Dockerfile")
            || file == "Dockerfile"
    }) {
        flags.insert("security_sensitive_automation_change");
    }
    if patch
        .lines()
        .any(|line| line.starts_with("new file mode 100755"))
    {
        flags.insert("new_executable_file");
    }
    if patch.lines().any(|line| {
        line.starts_with('+')
            && !line.starts_with("+++")
            && ["-----BEGIN", "AKIA", "password=", "secret=", "token="]
                .iter()
                .any(|pattern| line.contains(pattern))
    }) {
        flags.insert("sensitive_literal_pattern_added");
    }
    flags.into_iter().map(str::to_string).collect()
}

fn deterministic_recommendation(summaries: &[CandidateSummary<'_>]) -> Option<String> {
    let eligible = summaries
        .iter()
        .filter(|summary| !summary.test_evidence.is_empty() && summary.risk_flags.is_empty())
        .collect::<Vec<_>>();
    (eligible.len() == 1).then(|| eligible[0].artifact.id.clone())
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
    crate::agent_task_gate::append_promotion_gate_argv(&mut command, &options.gates);
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

pub(crate) fn selected_candidate_task_id_in_store(
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

// The ambient `promote_or_load_attempt()` shim that used to sit above this
// resolved a root and delegated straight here. It had no callers, so it was a
// resolution point that existed for nobody (#7505).

/// Promotion is the durable boundary between a terminal provider result and
/// controller-owned gates. Reconciliation must reuse this exact report rather
/// than apply the selected artifact again.
///
/// The persisted promotion this loads and the one it writes have to come from
/// one installation, or a reconciliation would reuse a report the injected
/// store never recorded.
pub(crate) fn promote_or_load_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let aggregate = lifecycle_store.read_aggregate(run_id)?;
    let outcome = aggregate
        .selected_outcome()
        .or_else(|| {
            (aggregate.outcomes.len() == 1)
                .then(|| aggregate.outcomes.first())
                .flatten()
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "provider_model",
                "Cook promotion has no selected provider outcome",
                Some(run_id.to_string()),
                None,
            )
        })?;
    if concrete_provider_model(outcome.selected_model()).is_none() {
        return Err(Error::validation_invalid_argument(
            "provider_model",
            "Cook promotion requires a concrete executed model",
            Some(run_id.to_string()),
            None,
        ));
    }
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
                    source_worktree_path: component_workspace_path(options)?
                        .or_else(|| options.source_worktree_path.clone()),
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
    validate_replacement_proof_finalization_eligibility(run_id, &replacement)?;
    validate_legacy_replacement_candidate_checkout(run_id, &original, &replacement)?;
    let expected_candidate = original.provenance.get("candidate");
    let observed_candidate = replacement.provenance.get("candidate");
    let same_candidate = observed_candidate == expected_candidate
        && (original
            .provenance
            .get("candidate_checkout")
            .is_none_or(Value::is_null)
            || replacement.provenance.get("candidate_checkout")
                == original.provenance.get("candidate_checkout"));
    let drifted = serde_json::json!({
        "run_id": replacement.source.run_id.as_deref() != Some(run_id),
        "target": replacement.target.worktree != original.target.worktree || replacement.target.path != original.target.path,
        "worktree": replacement.to_worktree != original.to_worktree,
        "artifact": replacement.patch_artifact.id != original.patch_artifact.id || replacement.patch_artifact.kind != original.patch_artifact.kind || replacement.patch_artifact.sha256 != original.patch_artifact.sha256,
        "changed_files": replacement.changed_files != original.changed_files,
        "verified_base": replacement.verified_base != original.verified_base,
        "candidate": !same_candidate,
    });
    if drifted
        .as_object()
        .is_some_and(|fields| fields.values().any(|value| value.as_bool() == Some(true)))
    {
        let mut error = Error::validation_invalid_argument(
            "replacement_gate_proof",
            "replacement proof drifted from the exact failed promotion candidate, base, target, artifact, or scope",
            Some(run_id.to_string()),
            None,
        );
        error.details["drift"] = drifted;
        error.details["candidate_fingerprint"] = serde_json::json!({
            "expected": bounded_candidate_fingerprint(expected_candidate),
            "observed": bounded_candidate_fingerprint(observed_candidate),
        });
        return Err(error);
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

fn bounded_candidate_fingerprint(candidate: Option<&Value>) -> Value {
    const MAX_CHANGED_FILES: usize = 32;

    let Some(candidate) = candidate else {
        return Value::Null;
    };
    let Some(fingerprint) = candidate.get("fingerprint") else {
        return serde_json::json!({ "kind": candidate.get("kind") });
    };
    let changed_files = fingerprint
        .get("changed_files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .take(MAX_CHANGED_FILES)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::json!({
        "kind": candidate.get("kind"),
        "schema": fingerprint.get("schema"),
        "target_path": fingerprint.get("target_path"),
        "head": fingerprint.get("head"),
        "base": fingerprint.get("base"),
        "tree": fingerprint.get("tree"),
        "sha256": fingerprint.get("sha256"),
        "changed_files": changed_files,
        "changed_files_truncated": fingerprint
            .get("changed_files")
            .and_then(Value::as_array)
            .is_some_and(|files| files.len() > MAX_CHANGED_FILES),
    })
}

/// Older failed promotions did not retain the checkout used by their gates.
/// Their immutable candidate fingerprint remains sufficient to authenticate a
/// replacement checkout, but only when its tree and candidate digest match.
fn validate_legacy_replacement_candidate_checkout(
    run_id: &str,
    original: &AgentTaskPromotionReport,
    replacement: &AgentTaskPromotionReport,
) -> Result<()> {
    if original
        .provenance
        .get("candidate_checkout")
        .is_some_and(|checkout| !checkout.is_null())
    {
        return Ok(());
    }
    let failed = |predicate: &'static str| {
        let mut error = Error::validation_invalid_argument(
            "replacement_gate_proof",
            format!("legacy replacement checkout is not proven: failed `{predicate}`"),
            Some(run_id.to_string()),
            None,
        );
        error.details["failed_eligibility_predicate"] = serde_json::json!(predicate);
        error
    };
    let fingerprint = original
        .provenance
        .pointer("/candidate/fingerprint")
        .ok_or_else(|| failed("legacy_candidate_fingerprint"))?;
    let head = fingerprint
        .get("head")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failed("legacy_candidate_fingerprint"))?;
    let tree = fingerprint
        .get("tree")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failed("legacy_candidate_fingerprint"))?;
    let sha256 = fingerprint
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failed("legacy_candidate_fingerprint"))?;
    let checkout = replacement
        .provenance
        .get("candidate_checkout")
        .ok_or_else(|| failed("legacy_candidate_checkout"))?;
    if checkout.get("tree").and_then(Value::as_str) != Some(tree)
        || checkout.get("candidate_sha256").and_then(Value::as_str) != Some(sha256)
    {
        return Err(failed("legacy_candidate_checkout_fingerprint"));
    }
    if replacement.provenance.get("candidate") != original.provenance.get("candidate") {
        return Err(failed("legacy_candidate_fingerprint"));
    }
    if let Some(adopted) = original
        .provenance
        .pointer("/adoption/candidate_ref")
        .and_then(Value::as_str)
    {
        if adopted != head {
            return Err(failed("legacy_candidate_adoption"));
        }
    }
    Ok(())
}

/// Admission and recovery share the same gate facts: an accepted replacement
/// must be able to hydrate a reviewer-runnable finalization dossier.
fn validate_replacement_proof_finalization_eligibility(
    run_id: &str,
    replacement: &AgentTaskPromotionReport,
) -> Result<()> {
    let failed = |predicate: &'static str, gate_id: Option<&str>| {
        let mut error = Error::validation_invalid_argument(
            "replacement_gate_proof",
            format!("replacement proof is not finalization-eligible: failed `{predicate}`"),
            Some(run_id.to_string()),
            None,
        );
        error.details["failed_eligibility_predicate"] = serde_json::json!(predicate);
        if let Some(gate_id) = gate_id {
            error.details["gate_id"] = serde_json::json!(gate_id);
        }
        error
    };

    if replacement.status != AgentTaskPromotionStatus::Applied {
        return Err(failed("applied_status", None));
    }
    if replacement.deterministic_gates.is_empty() {
        return Err(failed("non_empty_deterministic_gates", None));
    }
    if replacement.verified_base.is_none() {
        return Err(failed("verified_base", None));
    }
    if !replacement.finalization_eligible(false) {
        return Err(failed("green_gate_status", None));
    }
    let candidate_checkout = replacement.provenance.get("candidate_checkout");
    for gate in &replacement.deterministic_gates {
        if gate.command.is_empty() {
            return Err(failed("command", Some(&gate.id)));
        }
        if !replacement
            .command_evidence
            .iter()
            .any(|evidence| evidence.exit_code == 0 && evidence.command == gate.command)
        {
            return Err(failed("matching_command_evidence", Some(&gate.id)));
        }
        if candidate_checkout.is_none()
            || gate
                .candidate_checkout
                .as_ref()
                .and_then(|checkout| serde_json::to_value(checkout).ok())
                .as_ref()
                != candidate_checkout
        {
            return Err(failed("candidate_checkout", Some(&gate.id)));
        }
    }
    let visible_gates = replacement
        .deterministic_gates
        .iter()
        .filter(|gate| gate.visibility == homeboy_core::gate::HomeboyGateVisibility::Visible)
        .collect::<Vec<_>>();
    if visible_gates.is_empty() {
        return Err(failed("visibility", None));
    }
    let shell_gates = visible_gates
        .iter()
        .copied()
        .filter(|gate| matches!(gate.command.as_slice(), [shell, flag, _] if shell == "sh" && flag == "-lc"))
        .collect::<Vec<_>>();
    if shell_gates.is_empty() {
        return Err(failed("shell_command", None));
    }
    let candidate_bound_gates = shell_gates
        .iter()
        .copied()
        .filter(|gate| replacement.has_visible_passed_gate_for_command(&gate.command[2]))
        .collect::<Vec<_>>();
    if candidate_bound_gates.is_empty() {
        return Err(failed("candidate_checkout", None));
    }
    if !candidate_bound_gates
        .iter()
        .any(|gate| crate::agent_task_review_dossier::reviewer_runnable_command(&gate.command[2]))
    {
        return Err(failed("reviewer_runnable_command", None));
    }
    Ok(())
}

/// Execute corrected gates against the exact failed applied candidate and record
/// the resulting #11290 replacement proof without replaying provider work or
/// applying the patch again.
const REPLACEMENT_GATE_PROOF_CLAIM_LEASE: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);
const REPLACEMENT_GATE_EXECUTION_FENCES_KEY: &str = "replacement_gate_execution_fences";

pub fn verify_replacement_gates(
    cook_or_attempt_id: &str,
    gates: crate::agent_task_gate::VerifyGateOptions,
    external_authorization: String,
) -> Result<AgentTaskPromotionReport> {
    let run_id = super::cook_recipe::resolve_cook_continuation_run_id(cook_or_attempt_id)?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let operation_key = format!("verify-replacement:{run_id}");
    match lifecycle_store.claim_cook_operation(
        &run_id,
        &operation_key,
        REPLACEMENT_GATE_PROOF_CLAIM_LEASE,
    )? {
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            serde_json::from_value(result).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("deserialize completed replacement gate proof".to_string()),
                )
            })
        }
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            let claim = lifecycle_store.operation_claim(&run_id, &operation_key)?;
            let mut error = Error::validation_invalid_argument(
                "replacement_gate_proof",
                "operation_in_progress",
                Some(operation_key),
                Some(vec![format!("homeboy agent-task status {run_id} --full")]),
            );
            error.details["claim"] = serde_json::to_value(claim).unwrap_or(Value::Null);
            Err(error)
        }
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            let result = verify_replacement_gates_owned(
                &lifecycle_store,
                &run_id,
                gates,
                external_authorization,
            );
            match result {
                Ok(report) => {
                    lifecycle_store.complete_cook_operation(
                        &run_id,
                        &operation_key,
                        serde_json::to_value(&report)
                            .map_err(|error| Error::internal_json(error.to_string(), None))?,
                    )?;
                    Ok(report)
                }
                Err(error) => {
                    lifecycle_store.fail_cook_operation(
                        &run_id,
                        &operation_key,
                        serde_json::json!({
                            "code": error.code.as_str(),
                            "message": error.message.clone(),
                        }),
                    )?;
                    Err(error)
                }
            }
        }
    }
}

fn verify_replacement_gates_owned(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    gates: crate::agent_task_gate::VerifyGateOptions,
    external_authorization: String,
) -> Result<AgentTaskPromotionReport> {
    let original = persisted_promotion_for_attempt(&run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion",
            "replacement gates require a persisted failed promotion",
            Some(run_id.to_string()),
            None,
        )
    })?;
    // A process can die after `record_replacement_gate_proof()` commits and
    // before its operation claim is completed. Recover that durable success
    // without repeating the gate execution that produced the proof.
    if original.status == AgentTaskPromotionStatus::Applied
        && original.provenance.get("replacement_gate_proof").is_some()
    {
        return Ok(original);
    }
    if replacement_gate_execution_started(lifecycle_store, run_id)? {
        return Err(interrupted_replacement_gate_execution_error(run_id));
    }
    if original.status != AgentTaskPromotionStatus::GateFailed || !original.status.patch_promoted()
    {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.status",
            "replacement gates require an already-applied candidate whose original gates failed",
            Some(run_id.to_string()),
            None,
        ));
    }
    if !gates
        .verify
        .iter()
        .any(|command| crate::agent_task_review_dossier::reviewer_runnable_command(command))
    {
        let mut error = Error::validation_invalid_argument(
            "replacement_gate_proof",
            "replacement verification requires a reviewer-runnable visible gate before shell execution",
            Some(run_id.to_string()),
            None,
        );
        error.details["failed_eligibility_predicate"] =
            serde_json::json!("reviewer_runnable_command");
        return Err(error);
    }
    let target_path = original.target.path.as_deref().or_else(|| {
        original.provenance.get("worktree_path").and_then(Value::as_str)
    }).map(PathBuf::from).or_else(|| {
        homeboy_core::worktree::resolve_if_present(&original.to_worktree)
            .ok()
            .flatten()
            .map(|record| PathBuf::from(record.worktree_path))
    }).ok_or_else(|| Error::validation_invalid_argument(
        "promotion.target.path",
        "replacement gates require the failed promotion's durable candidate worktree path or registered worktree handle",
        Some(run_id.to_string()),
        None,
    ))?;
    let verified_base = original.verified_base.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "promotion.verified_base",
            "replacement gates require the failed promotion's verified base",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let inputs = original
        .provenance
        .pointer("/resume_contract/inputs")
        .or_else(|| original.provenance.pointer("/resume_inputs"));
    let (source, source_path) = promotion_source(&run_id)?;
    let observation_store = lifecycle_store.open_observation_initialized()?;
    // This fence is deliberately irreversible. Shell gates can have external
    // side effects, so a dead owner after this point must recover with external
    // candidate-bound proof rather than replaying an unknown partial execution.
    mark_replacement_gate_execution_started(lifecycle_store, run_id)?;
    let replacement_gate_workspace = replacement_component_workspace(&original, &target_path)?;
    let mut replacement = resume_promoted_patch_replacement_gates_in_observation_store(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some(run_id.to_string()),
            source_path,
            // Candidate identity remains rooted at the persisted promotion target.
            // The corrected gate workspace is passed separately below.
            source_worktree_path: None,
            base_ref: Some(verified_base.base.clone()),
            task_base_sha: inputs
                .and_then(|value| value.get("task_base_sha"))
                .and_then(Value::as_str)
                .map(str::to_string),
            candidate_ref: inputs
                .and_then(|value| value.get("candidate_ref"))
                .and_then(Value::as_str)
                .map(str::to_string),
            to_worktree: original.to_worktree.clone(),
            task_id: Some(original.source.task_id.clone()),
            artifact_id: Some(original.patch_artifact.id.clone()),
            dry_run: false,
            gates,
            provider_command: None,
            provider_invocation: None,
        },
        &target_path,
        &serde_json::to_value(&original)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
        replacement_gate_workspace.as_deref(),
        &observation_store,
    )?;
    // #11290's import boundary requires command evidence for each green gate.
    // The shared gate runner retains that evidence in detailed gate reports, so
    // project it into the typed promotion command-evidence view before recording.
    replacement.command_evidence.extend(
        replacement
            .deterministic_gates
            .iter()
            .filter(|gate| gate.exit_code == 0)
            .map(
                |gate| crate::agent_task_promotion::AgentTaskPromotionCommandReport {
                    command: gate.command.clone(),
                    exit_code: gate.exit_code,
                    stdout: gate.stdout.clone(),
                    stderr: gate.stderr.clone(),
                    capture: Default::default(),
                },
            ),
    );
    record_replacement_gate_proof(&run_id, replacement, Some(external_authorization))
}

fn replacement_gate_execution_started(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    Ok(lifecycle_store
        .read_record(run_id)?
        .metadata
        .pointer(&format!(
            "/{REPLACEMENT_GATE_EXECUTION_FENCES_KEY}/verify-replacement"
        ))
        .is_some())
}

pub(crate) fn mark_replacement_gate_execution_started(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<()> {
    lifecycle_store.mutate_record(run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let fences = metadata
            .entry(REPLACEMENT_GATE_EXECUTION_FENCES_KEY.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !fences.is_object() {
            *fences = serde_json::json!({});
        }
        if fences.get("verify-replacement").is_some() {
            return false;
        }
        fences["verify-replacement"] = serde_json::json!({
            "schema": "homeboy/agent-task-replacement-gate-execution-fence/v1",
            "state": "started",
        });
        true
    })?;
    Ok(())
}

fn interrupted_replacement_gate_execution_error(run_id: &str) -> Error {
    let mut error = Error::validation_invalid_argument(
        "replacement_gate_proof",
        "replacement gate execution was interrupted after its durable start fence; Homeboy will not rerun shell gates automatically",
        Some(run_id.to_string()),
        Some(vec![format!(
            "Run homeboy agent-task record-replacement-gate-proof {run_id} --promotion @replacement.json --authorize-external-proof <authorization> with candidate-bound external proof."
        )]),
    );
    error.details["recovery"] = serde_json::json!({
        "kind": "external_candidate_bound_proof_required",
        "run_id": run_id,
        "command": format!(
            "homeboy agent-task record-replacement-gate-proof {run_id} --promotion @replacement.json --authorize-external-proof <authorization>"
        ),
    });
    error
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

#[cfg(test)]
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
            let prefix = super::cook_recovery_command_prefix_for_record(&record);
            recovery.continuation =
                super::cook_recovery_command_with_prefix(&prefix, &["cook-continue", run_id]);
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
        continuation: super::cook_recovery_command(run_id, &["cook-continue", run_id]),
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

// The ambient `recover_moving_base_cook_candidate()` shim that used to sit here is
// gone; one moving-base recovery test was its only caller and now resolves its own store (#7505).

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
            source_worktree_path: component_workspace_path(options)?
                .or_else(|| options.source_worktree_path.clone()),
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

#[cfg(test)]
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

#[cfg(test)]
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
        verified_candidate_sha: None,
        protected_branches: options.protected_branches.clone(),
        draft_pr: options.draft_pr,
    })
}

/// Persist only a controller-validated manual preflight dossier for recovery.
pub fn persist_manual_finalization_intent(
    run_id: &str,
    report: &AgentTaskPrFinalizationReport,
) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord> {
    let mut report = report.clone();
    if crate::agent_task_lifecycle::persisted_status(run_id)?.state
        == crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
    {
        report.manual_candidate_binding = Some(manual_candidate_binding(run_id, &report)?);
    }
    validate_manual_preflight_report(&report, run_id, false)?;
    agent_task_lifecycle::record_manual_finalization_intent(
        run_id,
        serde_json::to_value(&report).expect("finalization report serializes"),
    )
}

/// Persist the validated dossier created immediately before a direct manual
/// publication. This retry form may still need to create its candidate commit.
pub fn persist_manual_finalization_retry_intent(
    run_id: &str,
    report: &AgentTaskPrFinalizationReport,
) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord> {
    validate_manual_preflight_report(report, run_id, true)?;
    let record = agent_task_lifecycle::record_manual_finalization_intent(
        run_id,
        serde_json::to_value(report).expect("finalization report serializes"),
    )?;
    agent_task_lifecycle::record_manual_finalization_retry(run_id)?;
    let candidate = crate::agent_task_promotion::candidate_fingerprint(&report.path)?;
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } = candidate
    else {
        return Err(Error::validation_invalid_argument(
            "path",
            "direct manual publication retry requires a materialized Git candidate",
            None,
            None,
        ));
    };
    agent_task_lifecycle::record_manual_finalization_retry_candidate(
        run_id,
        serde_json::to_value(fingerprint).expect("candidate fingerprint serializes"),
    )?;
    Ok(record)
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
        if crate::agent_task_lifecycle::persisted_status(&run_id)?.state
            == crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
        {
            let candidate = crate::agent_task_lifecycle::select_cook_candidate(requested_id)?;
            if candidate.incomplete
                || candidate.selected_task_id.is_none()
                || candidate.selected_artifact_id.is_none()
            {
                return Err(Error::validation_invalid_argument("run_id", "candidate-recoverable manual finalization requires the complete controller-selected Cook candidate", Some(run_id), None));
            }
            return require_manual_finalization_run(&candidate.run_id);
        }
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
    if record.state == crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable {
        if record.acceptance.is_some() || record.metadata.get("acceptance_requirement").is_some() {
            return Err(Error::validation_invalid_argument("acceptance", "candidate-recoverable manual finalization cannot replace a durable acceptance decision", Some(run_id.to_string()), None));
        }
        let cook_id = record.metadata["cook_id"].as_str().ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "candidate-recoverable manual finalization requires a Cook-bound candidate",
                Some(run_id.to_string()),
                None,
            )
        })?;
        let candidate = crate::agent_task_lifecycle::select_cook_candidate(cook_id)?;
        if !candidate.incomplete
            && candidate.run_id == run_id
            && candidate.selected_task_id.is_some()
            && candidate.selected_artifact_id.is_some()
        {
            return Ok(run_id.to_string());
        }
        return Err(Error::validation_invalid_argument("run_id", "candidate-recoverable manual finalization requires the complete controller-selected Cook candidate", Some(run_id.to_string()), None));
    }
    if record.lifecycle.execution.state
        != homeboy_core::run_lifecycle_record::RunExecutionState::Failed
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "manual finalization accepts an existing failed attempt, the complete controller-selected candidate-recoverable attempt, or an unused ID for a new durable manual-finalization record",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(run_id.to_string())
}

fn manual_candidate_binding(
    run_id: &str,
    report: &AgentTaskPrFinalizationReport,
) -> Result<crate::agent_task_finalization::AgentTaskManualCandidateBinding> {
    let record = crate::agent_task_lifecycle::persisted_status(run_id)?;
    if record.state != crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
        || record.acceptance.is_some()
        || record.metadata.get("acceptance_requirement").is_some()
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "manual candidate binding requires an unblocked candidate-recoverable run",
            Some(run_id.to_string()),
            None,
        ));
    }
    let cook_id = record.metadata["cook_id"].as_str().ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            "candidate-recoverable manual finalization requires a Cook-bound candidate",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let selection = crate::agent_task_lifecycle::select_cook_candidate(cook_id)?;
    if selection.incomplete
        || selection.run_id != run_id
        || selection.selected_task_id.is_none()
        || selection.selected_artifact_id.is_none()
    {
        return Err(Error::validation_invalid_argument("run_id", "candidate-recoverable manual finalization requires the complete controller-selected Cook candidate", Some(run_id.to_string()), None));
    }
    let promotion = persisted_promotion_for_attempt(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion",
            "candidate-recoverable manual finalization requires a persisted replacement gate proof",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let candidate: crate::agent_task_promotion::AgentTaskPromotionCandidate = serde_json::from_value(promotion.provenance["candidate"].clone()).map_err(|_| Error::validation_invalid_argument("latest_promotion.provenance.candidate", "candidate-recoverable manual finalization requires a durable Git candidate fingerprint", Some(run_id.to_string()), None))?;
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } = candidate
    else {
        return Err(Error::validation_invalid_argument("latest_promotion.provenance.candidate", "candidate-recoverable manual finalization requires a durable Git candidate fingerprint", Some(run_id.to_string()), None));
    };
    let model = record.lifecycle.provider_runtime.iter().rev().find_map(|runtime| runtime.metadata["model"].as_str()).filter(|model| !model.trim().is_empty()).ok_or_else(|| Error::validation_invalid_argument("run_id", "candidate-recoverable manual finalization requires concrete provider model provenance", Some(run_id.to_string()), None))?;
    let commit = report
        .publication_proof
        .git_identity
        .as_ref()
        .and_then(|identity| identity.commit_sha.as_deref())
        .unwrap_or_default();
    if promotion.source.run_id.as_deref() != Some(run_id)
        || selection.selected_task_id.as_deref() != Some(promotion.source.task_id.as_str())
        || promotion.provenance.get("replacement_gate_proof").is_none()
        || !promotion.finalization_eligible(false)
        || fingerprint.head != commit
        || fingerprint.changed_files != report.changed_files
        || report.evidence.ai_model.as_deref() != Some(model)
        || report.review_dossier.ai_assistance.model != model
        || promotion.gate_results != report.normalized_gate_results
    {
        return Err(Error::validation_invalid_argument("manual_finalization_intent", "manual finalization dossier does not match the selected candidate, model provenance, and replacement gate proof", Some(run_id.to_string()), None));
    }
    Ok(
        crate::agent_task_finalization::AgentTaskManualCandidateBinding {
            schema: crate::agent_task_finalization::AGENT_TASK_MANUAL_CANDIDATE_BINDING_SCHEMA
                .to_string(),
            selection,
            candidate: fingerprint,
            source: promotion.source,
            model: model.to_string(),
            replacement_gate_proof: promotion.provenance["replacement_gate_proof"].clone(),
            gates: promotion.gate_results,
        },
    )
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
        let retryable = agent_task_lifecycle::status(&report.run_id)?.metadata
            ["manual_finalization_retry"]
            == true;
        if retryable {
            require_manual_retry_candidate(
                &agent_task_lifecycle::status(&report.run_id)?,
                &report.path,
            )?;
        }
        let mut finalization = manual_finalization_options(report, retryable)?;
        if retryable {
            finalization.expected_candidate_sha = None;
        }
        let failure_run_id = finalization.run_id.clone();
        let result = if preflight {
            preflight_pr_with_backend(finalization, backend)
        } else {
            finalize_pr_with_backend(finalization, backend)
        };
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                if !preflight {
                    agent_task_lifecycle::record_manual_finalization_failure(
                        &failure_run_id,
                        &error,
                    )?;
                }
                return Err(error);
            }
        };
        let value = serde_json::to_value(&report).unwrap_or(Value::Null);
        if !preflight {
            if let Err(error) = persist_manual_finalization_receipt(
                value["run_id"].as_str().unwrap_or_default(),
                &report,
            ) {
                agent_task_lifecycle::record_manual_finalization_failure(&failure_run_id, &error)?;
                return Err(error);
            }
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
    // A verified empty remediation is evidence about the already-applied
    // candidate, not a replacement candidate. Recover the source promotion so
    // an exact `--recover <remediation>` cannot hide that applied promotion.
    let run_id = substantive_source_for_empty_remediation(&recipe, &run_id).unwrap_or(run_id);
    if persisted_promotion_for_attempt(&run_id)?.is_some_and(|promotion| {
        promotion.status == AgentTaskPromotionStatus::VerifiedNoChanges
            && promotion.changed_files.is_empty()
    }) {
        if let Some(receipt) = recover_verified_no_change_finalization(&recipe, &run_id)? {
            return Ok(receipt);
        }
    }
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
        && !(recovery_outcome.status == AgentTaskPromotionStatus::VerifiedNoChanges
            && !promotion.changed_files.is_empty()
            && promotion.finalization_eligible(options.gates.accept_inherited_failures))
        && !(recovery_outcome.status == AgentTaskPromotionStatus::GateFailed
            && promotion.finalization_eligible(options.gates.accept_inherited_failures))
    {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.status",
            "recovery requires an applied promotion, a verified existing-candidate delta, or an explicitly accepted inherited baseline failure",
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

/// Finalize a verified no-change Cook without routing it through patch publication.
/// The candidate is still re-read exactly: a no-change declaration authorizes no
/// publication only for the clean, bound candidate that deterministic gates checked.
fn recover_verified_no_change_finalization(
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
    run_id: &str,
) -> Result<Option<Value>> {
    let Some(promotion) = persisted_promotion_for_attempt(run_id)? else {
        return Ok(None);
    };
    if promotion.status != AgentTaskPromotionStatus::VerifiedNoChanges {
        return Ok(None);
    }
    let aggregate = agent_task_lifecycle::read_attempt_aggregate(run_id)?;
    let declaration = super::cook::intentional_no_change_from_aggregate(&aggregate).ok_or_else(|| {
        Error::validation_invalid_argument(
            "intentional_no_change",
            "verified no-change recovery requires the attempt's durable intentional-no-change declaration",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let options = super::cook_recipe::reconstruct_adoption_options(recipe)?;
    let _verified_base = promotion
        .verified_base
        .as_ref()
        .filter(|base| base.base == options.base && !base.sha.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.verified_base",
                "verified no-change recovery requires the Cook's declared immutable base snapshot",
                Some(run_id.to_string()),
                None,
            )
        })?;
    if !promotion.finalization_eligible(false) {
        return Err(Error::validation_invalid_argument(
            "latest_promotion",
            "verified no-change recovery requires green deterministic gates",
            Some(run_id.to_string()),
            None,
        ));
    }
    let selection = canonical_cook_candidate(&recipe.cook_id)
        .filter(|candidate| {
            candidate["incomplete"] != true && candidate["run_id"].as_str() == Some(run_id)
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_or_cook_id",
                "verified no-change recovery requires the exact Cook-bound candidate",
                Some(run_id.to_string()),
                None,
            )
        })?;
    if selection["selected_task_id"].as_str() != Some(promotion.source.task_id.as_str()) {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.source.task_id",
            "verified no-change recovery requires the promotion source task to match the Cook-bound candidate",
            Some(run_id.to_string()),
            None,
        ));
    }
    let expected: AgentTaskPromotionCandidate = serde_json::from_value(
        promotion
            .provenance
            .get("candidate")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|_| {
        Error::validation_invalid_argument(
            "latest_promotion.provenance.candidate",
            "verified no-change recovery requires a durable Git candidate fingerprint",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let AgentTaskPromotionCandidate::Git { .. } = &expected else {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.provenance.candidate",
            "verified no-change recovery requires a durable Git candidate fingerprint",
            Some(run_id.to_string()),
            None,
        ));
    };
    let path = promotion
        .provenance
        .pointer("/worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "latest_promotion.provenance.worktree_path",
                "verified no-change recovery requires the verified candidate worktree path",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let actual = candidate_fingerprint(path)?;
    if !verified_no_change_candidate_is_valid(&expected, &actual) {
        return Err(Error::validation_invalid_argument(
            "path",
            "verified no-change recovery requires the exact clean candidate checked by deterministic gates",
            Some(path.to_string()),
            None,
        ));
    }
    let receipt = json!({
        "schema": "homeboy/agent-task-cook-no-change-finalization/v1",
        "run_id": run_id,
        "status": "intentional_no_change_finalized",
        "disposition": "no_change_finalized",
        "publication": { "action": "none", "committed": false, "pushed": false, "published": false },
        "source_attempt": { "cook_id": recipe.cook_id, "run_id": run_id, "candidate": selection },
        "intentional_no_change": declaration,
        "gate_results": promotion.gate_outcome().gate_results,
    });
    // A completed receipt makes recovery idempotent without inventing a patch publication.
    agent_task_lifecycle::record_cook_finalization(run_id, receipt.clone())?;
    Ok(Some(receipt))
}

fn verified_no_change_candidate_is_valid(
    expected: &AgentTaskPromotionCandidate,
    actual: &AgentTaskPromotionCandidate,
) -> bool {
    let AgentTaskPromotionCandidate::Git { fingerprint } = expected else {
        return false;
    };
    expected == actual && fingerprint.changed_files.is_empty()
}

#[cfg(test)]
mod no_change_recovery_tests {
    use super::*;

    fn candidate(head: &str, changed_files: &[&str]) -> AgentTaskPromotionCandidate {
        serde_json::from_value(json!({
            "kind": "git",
            "fingerprint": {
                "schema": "homeboy/agent-task-candidate-fingerprint/v1",
                "target_path": "/fixture",
                "head": head,
                "base": "parent-head",
                "sha256": "candidate-sha",
                "tree": "candidate-tree",
                "changed_files": changed_files
            }
        }))
        .expect("candidate fixture")
    }

    #[test]
    fn verified_no_change_candidate_requires_exact_clean_binding() {
        let bound = candidate("candidate-head", &[]);
        assert!(verified_no_change_candidate_is_valid(&bound, &bound,));

        assert!(!verified_no_change_candidate_is_valid(
            &candidate("candidate-head", &["dirty.rs"]),
            &candidate("candidate-head", &["dirty.rs"])
        ));
        assert!(!verified_no_change_candidate_is_valid(
            &bound,
            &candidate("different-head", &[])
        ));
        assert!(!verified_no_change_candidate_is_valid(
            &candidate("candidate-head", &[]),
            &serde_json::from_value(
                json!({ "kind": "non_git", "disposition": "not_a_git_worktree" })
            )
            .expect("unbound candidate fixture")
        ));
    }
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
                            && !is_empty_verified_remediation(&promotion)
                    })
            {
                return Some(attempt.run_id.clone());
            }
        }
    }
    Some(source_run_id)
}

fn substantive_source_for_empty_remediation(
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
    run_id: &str,
) -> Option<String> {
    let remediation = persisted_promotion_for_attempt(run_id).ok().flatten()?;
    if !is_empty_verified_remediation(&remediation) {
        return None;
    }
    let source_run_id = remediation
        .provenance
        .pointer("/cook_follow_up/source_run_id")
        .and_then(Value::as_str)?;
    if !recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == source_run_id)
        || canonical_cook_candidate(&recipe.cook_id)
            .and_then(|candidate| candidate["run_id"].as_str().map(str::to_string))
            .as_deref()
            != Some(source_run_id)
    {
        return None;
    }
    persisted_promotion_for_attempt(source_run_id)
        .ok()
        .flatten()
        .filter(|promotion| {
            promotion.status == AgentTaskPromotionStatus::Applied
                && !promotion.changed_files.is_empty()
        })?;
    Some(source_run_id.to_string())
}

fn is_empty_verified_remediation(promotion: &AgentTaskPromotionReport) -> bool {
    promotion.status == AgentTaskPromotionStatus::VerifiedNoChanges
        && promotion.changed_files.is_empty()
        && promotion
            .provenance
            .pointer("/cook_follow_up/source_run_id")
            .and_then(Value::as_str)
            .is_some_and(|run_id| !run_id.is_empty())
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
    let retryable = record.metadata["manual_finalization_retry"] == true;
    if retryable {
        require_manual_retry_candidate(&record, &report.path)?;
    }
    let mut finalization = manual_finalization_options(report, retryable)?;
    if retryable {
        // Direct publication first persisted this dossier before it attempted a
        // mutation. It is safe to retry that original mutation after failure.
        finalization.expected_candidate_sha = None;
    }
    let result = if preflight {
        preflight_pr_with_backend(finalization, backend)
    } else {
        finalize_pr_with_backend(finalization, backend)
    };
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            if !preflight {
                agent_task_lifecycle::record_manual_finalization_failure(run_id, &error)?;
            }
            return Err(error);
        }
    };
    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
    if !preflight {
        if let Err(error) = persist_manual_finalization_receipt(run_id, &report) {
            agent_task_lifecycle::record_manual_finalization_failure(run_id, &error)?;
            return Err(error);
        }
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
        if value["status"].as_str() == Some("intentional_no_change_finalized") {
            return Ok(Some(value.clone()));
        }
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
    let candidate_recoverable = crate::agent_task_lifecycle::persisted_status(run_id)?.state
        == crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable;
    if candidate_recoverable && report.manual_candidate_binding.is_none() {
        return Err(Error::validation_invalid_argument(
            "manual_finalization_intent",
            "candidate-recoverable manual finalization intent has no controller candidate binding",
            Some(run_id.to_string()),
            None,
        ));
    }
    if let Some(binding) = report.manual_candidate_binding.as_ref() {
        if binding.schema
            != crate::agent_task_finalization::AGENT_TASK_MANUAL_CANDIDATE_BINDING_SCHEMA
            || manual_candidate_binding(run_id, &report)? != *binding
        {
            return Err(Error::validation_invalid_argument(
                "manual_finalization_intent",
                "persisted manual finalization intent no longer matches the selected Cook candidate",
                Some(run_id.to_string()),
                None,
            ));
        }
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
            record,
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
        && (record
            .metadata
            .get("manual_finalization_retry_candidate")
            .or_else(|| record.metadata.get("manual_finalization_retry_origin"))
            .is_some()
            || (!report.finalization_outcome.committed && !report.finalization_outcome.pushed))
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
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
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
        && (if let Some(candidate) = record
            .metadata
            .get("manual_finalization_retry_candidate")
            .or_else(|| record.metadata.get("manual_finalization_retry_origin"))
        {
            serde_json::from_value::<crate::agent_task_promotion::AgentTaskCandidateFingerprint>(
                candidate.clone(),
            )
            .is_ok_and(|candidate| {
                candidate.tree == binding.candidate_tree
                    && candidate.changed_files == binding.changed_files
            })
        } else {
            intent_git_identity.commit_sha.is_some()
                && git_identity.commit_sha == intent_git_identity.commit_sha
                && binding.candidate_sha
                    == intent_git_identity
                        .commit_sha
                        .as_deref()
                        .unwrap_or_default()
        })
}

fn require_manual_retry_candidate(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    path: &str,
) -> Result<()> {
    let expected = record
        .metadata
        .get("manual_finalization_retry_candidate")
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "manual_finalization_retry_candidate",
                "retryable manual publication has no durable candidate fingerprint",
                None,
                None,
            )
        })?;
    let expected: crate::agent_task_promotion::AgentTaskCandidateFingerprint =
        serde_json::from_value(expected).map_err(|_| {
            Error::validation_invalid_argument(
                "manual_finalization_retry_candidate",
                "retryable manual publication candidate fingerprint is invalid",
                None,
                None,
            )
        })?;
    let actual = crate::agent_task_promotion::candidate_fingerprint(path)?;
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git {
        fingerprint: actual,
    } = actual
    else {
        return Err(Error::validation_invalid_argument(
            "manual_finalization_retry_candidate",
            "manual publication candidate changed after the direct preflight; rerun finalization with the current candidate",
            None,
            None,
        ));
    };
    // Hooks may stage the exact candidate before rejecting it. Bind semantic
    // content and paths, not the transient staged/unstaged representation.
    if actual.tree != expected.tree || actual.changed_files != expected.changed_files {
        return Err(Error::validation_invalid_argument(
            "manual_finalization_retry_candidate",
            "manual publication candidate changed after the direct preflight; rerun finalization with the current candidate",
            None,
            None,
        ));
    }
    Ok(())
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
    allow_uncommitted_candidate: bool,
) -> Result<AgentTaskPrFinalizationOptions> {
    validate_manual_preflight_report(&report, &report.run_id, allow_uncommitted_candidate)?;
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
        verified_candidate_sha: None,
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
    allow_uncommitted_candidate: bool,
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
    if !allow_uncommitted_candidate
        && report
            .publication_proof
            .git_identity
            .as_ref()
            .and_then(|identity| identity.commit_sha.as_deref())
            .is_none_or(str::is_empty)
    {
        return Err(Error::validation_invalid_argument(
            "publication_proof.git_identity.commit_sha",
            "recoverable manual preflight requires a committed candidate; commit the candidate first and rerun preflight, or run the same manual-finalization command without --preflight to have Homeboy commit and publish it directly",
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

fn concrete_provider_model(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .filter(|model| {
            !matches!(
                model.to_ascii_lowercase().as_str(),
                "not recorded"
                    | "unknown"
                    | "ai-assisted"
                    | "ai assisted"
                    | "legacy caller did not record a model"
            )
        })
        .map(str::to_string)
}

/// Resolve the model from the terminal provider execution for the scheduler's
/// selected candidate task. Older controllers did not persist the execution
/// ledger, so their provider-reported outcome model remains a compatible
/// fallback; a ledger, when present, is the stronger source of execution fact.
fn selected_execution_model(
    provider_executions: &Value,
    outcome: &crate::agent_task::AgentTaskOutcome,
    terminal_model: Option<&str>,
    run_id: &str,
) -> Result<Option<String>> {
    let executions = provider_executions
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let terminal_attempt = executions
        .iter()
        .filter(|execution| {
            execution["task_id"] == outcome.task_id
                && matches!(
                    execution["state"].as_str(),
                    Some("succeeded" | "candidate_recoverable")
                )
        })
        .filter_map(|execution| execution["attempt"].as_u64())
        .max();
    let Some(terminal_attempt) = terminal_attempt else {
        return Ok(concrete_provider_model(terminal_model)
            .or_else(|| concrete_provider_model(outcome.selected_model())));
    };
    let mut models = executions
        .iter()
        .filter(|execution| {
            execution["task_id"] == outcome.task_id
                && execution["attempt"].as_u64() == Some(terminal_attempt)
                && matches!(
                    execution["state"].as_str(),
                    Some("succeeded" | "candidate_recoverable")
                )
        })
        .filter_map(|execution| concrete_provider_model(execution["model"].as_str()))
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    match models.as_slice() {
        [] => Ok(concrete_provider_model(terminal_model)
            .or_else(|| concrete_provider_model(outcome.selected_model()))),
        [model] => Ok(Some(model.clone())),
        _ => Err(Error::validation_invalid_argument(
            "provider_model",
            "Cook lineage selected provider execution has ambiguous concrete models",
            Some(run_id.to_string()),
            None,
        )),
    }
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
    let terminal = super::cook_pre_execution::terminal_executor_identity(
        &outcome,
        &plan,
        record.metadata.get("provider_executions"),
    );
    let model = selected_execution_model(
        record
            .metadata
            .get("provider_executions")
            .unwrap_or(&Value::Null),
        &outcome,
        terminal
            .as_ref()
            .and_then(|identity| identity.model.as_deref()),
        run_id,
    )?;
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
        model,
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

/// Shared continuation/finalization admission. Legacy attempts without durable
/// model evidence are rejected before a continuation can claim or promote them.
pub fn validate_cook_attempt_model_provenance(run_id: &str) -> Result<()> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    required_execution_model(
        &cook_attempt_execution_in_store(&lifecycle_store, run_id)?,
        run_id,
    )
    .map(|_| ())
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
            primary_failure: None,
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
/// failures contribute only a bounded, redacted causal command projection;
/// expanded output remains behind `diagnose`.
pub fn cook_failure_context(
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
    let pre_execution_diagnostic = record.as_ref().and_then(|record| {
        let failure = record.metadata.get("pre_execution_failure")?;
        let details = failure.get("details")?;
        homeboy_core::worktree_providers::compact_provider_failure_details(details).map(
            |evidence| {
                serde_json::json!({
                    "code": failure.get("error_code"),
                    "message": failure.get("message"),
                    "worktree_provider_failure": evidence,
                })
            },
        )
    });
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
    } else if let Some(diagnostic) = pre_execution_diagnostic {
        (
            record
                .as_ref()
                .and_then(|record| record.metadata.pointer("/pre_execution_failure/phase"))
                .and_then(Value::as_str)
                .unwrap_or("pre_execution")
                .to_string(),
            diagnostic["code"]
                .as_str()
                .unwrap_or("pre_execution_failure")
                .to_string(),
            Some(diagnostic),
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
    let recovery_actions = dirty_candidate_adoption_recovery_actions(
        &recipe,
        record.as_ref(),
        cook_id,
        &chronological_latest_run_id,
    )
    .unwrap_or_else(|| {
        cook_recovery_actions(
            status,
            &chronological_latest_run_id,
            recovery_legal,
            blocking_claim.is_some(),
            record
                .as_ref()
                .is_some_and(|record| super::retry_admission(&record.run_id).is_ok()),
            exact_checkpoint_candidate_mismatch(&diagnostic),
            ambiguous_promotion_artifact_ids(record_run_id, promotion_diagnostic.as_ref(), &recipe),
            record.as_ref().and_then(lab_handoff_runtime_recovery),
        )
    });
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

/// A dirty initial checkout is never provider input. Its durable failed attempt
/// can instead adopt a human-committed immutable candidate without replaying a
/// provider patch. This is available only for the recorded first-provider
/// admission failure and a recipe that already names the candidate's model.
fn dirty_candidate_adoption_recovery_actions(
    recipe: &super::AgentTaskCookRecipe,
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    cook_id: &str,
    run_id: &str,
) -> Option<CookRecoveryActions> {
    let record = record?;
    if record.metadata["provider_executions_consumed"]
        .as_u64()
        .unwrap_or_default()
        != 0
        || record.metadata["pre_execution_failure"]["details"]["dirty_candidate_adoption"]["reason"]
            != "first_provider_admission"
    {
        return None;
    }
    let workspace = record.metadata["pre_execution_failure"]["details"]["dirty_candidate_adoption"]
        ["workspace"]
        .as_str()?;
    let options = super::cook_recipe::reconstruct_adoption_options(recipe).ok()?;
    let model = options.ai_model?;
    let prefix = super::cook_recovery_command_prefix(run_id);
    let actions = vec![
        super::AgentTaskCookRecoveryAction {
            action: "commit_candidate".to_string(),
            command: format!(
                "git -C {} add -A && git -C {} commit -m {}",
                quote_arg(workspace),
                quote_arg(workspace),
                quote_arg(&options.commit_message),
            ),
        },
        super::AgentTaskCookRecoveryAction {
            action: "review_candidate".to_string(),
            command: format!("{prefix} agent-task review {}", quote_arg(run_id)),
        },
        super::AgentTaskCookRecoveryAction {
            action: "adopt_candidate".to_string(),
            command: format!(
                "{prefix} agent-task adopt {} --candidate-ref HEAD --model {}",
                quote_arg(cook_id),
                quote_arg(&model),
            ),
        },
    ];
    Some(CookRecoveryActions {
        reason: "The first provider admission refused a dirty checkout. Commit the immutable candidate, record its tracked review, then adopt HEAD through the durable Cook gates without provider patch replay.".to_string(),
        legal_actions: actions.clone(),
        next_actions: actions,
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
    retry_admitted: bool,
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
        | "retries_exhausted"
        | "pre_execution_failure" => false,
        "gate_failed" | "no_op_gate_failed" => retry_admitted,
        _ => true,
    };
    let mut actions = vec![
        super::AgentTaskCookRecoveryAction {
            action: "status".to_string(),
            command: super::cook_recovery_command(run_id, &["status", run_id, "--full"]),
        },
        super::AgentTaskCookRecoveryAction {
            action: "diagnose".to_string(),
            command: super::cook_recovery_command(run_id, &["diagnose", run_id]),
        },
    ];
    if blocking_claim {
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "reconcile".to_string(),
            command: super::cook_recovery_command(run_id, &["reconcile", run_id, "--dry-run"]),
        });
    }
    if exact_checkpoint_candidate_mismatch && retry_admitted {
        // The checkpoint authenticates one exact destination candidate. A
        // replacement run preserves that immutable evidence without claiming it
        // can safely continue against a diverged worktree.
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "fork_replacement".to_string(),
            command: super::cook_recovery_command(run_id, &["retry", run_id, "--run"]),
        });
    } else if !ambiguous_artifact_ids.is_empty() {
        actions.extend(ambiguous_artifact_ids.into_iter().map(|artifact_id| {
            super::AgentTaskCookRecoveryAction {
                action: "resume_with_artifact".to_string(),
                command: super::cook_recovery_command(
                    run_id,
                    &[
                        "cook-continue",
                        run_id,
                        "--rearm",
                        "--artifact-id",
                        &artifact_id,
                    ],
                ),
            }
        }));
    } else if status == "pre_execution_failure" && retry_admitted {
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "retry".to_string(),
            command: super::cook_recovery_command(run_id, &["retry", run_id, "--run"]),
        });
    } else if continuation_eligible {
        actions.push(super::AgentTaskCookRecoveryAction {
            action: "resume".to_string(),
            command: super::cook_recovery_command(run_id, &["cook-continue", run_id]),
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
                "pre_execution_failure",
                true,
                false,
                vec!["status", "diagnose", "retry"],
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
                    && (action.command.ends_with("cook-state-matrix-attempt-1")
                        || action
                            .command
                            .ends_with("cook-state-matrix-attempt-1 --run"))
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
            true,
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

#[cfg(test)]
mod provider_model_tests {
    use super::*;

    fn selected_outcome(model: Option<&str>) -> crate::agent_task::AgentTaskOutcome {
        crate::agent_task::AgentTaskOutcome {
            task_id: "selected".to_string(),
            metadata: model.map_or(Value::Null, |model| serde_json::json!({ "model": model })),
            ..Default::default()
        }
    }

    #[test]
    fn provider_model_uses_direct_successful_provider_execution() {
        let model = selected_execution_model(
            &serde_json::json!([{
                "task_id": "selected", "attempt": 1, "state": "succeeded",
                "model": "openai/gpt-5.6-terra"
            }]),
            &selected_outcome(None),
            None,
            "run",
        )
        .expect("direct execution model");

        assert_eq!(model.as_deref(), Some("openai/gpt-5.6-terra"));
    }

    #[test]
    fn provider_model_uses_terminal_gate_remediation_execution() {
        let model = selected_execution_model(
            &serde_json::json!([
                {"task_id": "selected", "attempt": 1, "state": "succeeded", "model": "initial-model"},
                {"task_id": "selected", "attempt": 2, "state": "succeeded", "model": "remediation-model"}
            ]),
            &selected_outcome(None),
            None,
            "run",
        )
        .expect("remediation execution model");

        assert_eq!(model.as_deref(), Some("remediation-model"));
    }

    #[test]
    fn provider_model_preserves_legacy_outcome_evidence_for_review_follow_up() {
        let outcome = selected_outcome(Some("legacy-provider-model"));
        let initial = selected_execution_model(&Value::Null, &outcome, None, "run")
            .expect("legacy initial model");
        let continuation = selected_execution_model(&Value::Null, &outcome, None, "run")
            .expect("legacy continuation model");

        assert_eq!(initial, continuation);
        assert_eq!(initial.as_deref(), Some("legacy-provider-model"));
    }

    #[test]
    fn provider_model_rejects_missing_ambiguous_and_unrelated_evidence() {
        let missing = selected_execution_model(&Value::Null, &selected_outcome(None), None, "run")
            .expect("missing evidence is unresolved");
        assert!(missing.is_none());

        let unrelated = selected_execution_model(
            &serde_json::json!([{
                "task_id": "other", "attempt": 1, "state": "succeeded", "model": "other-model"
            }]),
            &selected_outcome(None),
            None,
            "run",
        )
        .expect("unrelated evidence is ignored");
        assert!(unrelated.is_none());

        let error = selected_execution_model(
            &serde_json::json!([
                {"task_id": "selected", "attempt": 2, "state": "succeeded", "model": "model-a"},
                {"task_id": "selected", "attempt": 2, "state": "succeeded", "model": "model-b"}
            ]),
            &selected_outcome(None),
            None,
            "run",
        )
        .expect_err("ambiguous selected execution models fail closed");
        assert_eq!(error.details["field"], "provider_model");
    }

    #[test]
    fn provider_model_continuation_resolves_same_model_as_initial_finalization() {
        let executions = serde_json::json!([{
            "task_id": "selected",
            "attempt": 1,
            "state": "succeeded",
            "model": "openai/gpt-5.6-terra"
        }]);
        let outcome = selected_outcome(None);

        let initial = selected_execution_model(&executions, &outcome, None, "run-1")
            .expect("initial finalization model");
        let continuation = selected_execution_model(&executions, &outcome, None, "run-1")
            .expect("continuation finalization model");

        assert_eq!(initial, continuation);
        assert_eq!(initial.as_deref(), Some("openai/gpt-5.6-terra"));
    }

    #[test]
    fn provider_model_durable_execution_evidence_takes_precedence_over_stale_outcome() {
        let executions = serde_json::json!([{
            "task_id": "selected",
            "attempt": 1,
            "state": "succeeded",
            "model": "openai/gpt-5.6-terra"
        }]);
        // A stale or rotation-normalized outcome model must not override the
        // actual executed provider model recorded in the durable ledger.
        let outcome = selected_outcome(Some("stale-normalized-model"));

        let model = selected_execution_model(&executions, &outcome, None, "run")
            .expect("durable execution model wins");

        assert_eq!(model.as_deref(), Some("openai/gpt-5.6-terra"));
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
