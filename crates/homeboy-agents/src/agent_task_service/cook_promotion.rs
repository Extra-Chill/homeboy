//! Agent-task cook promotion & finalization.
//!
//! Extracted from `cook.rs`: promotion-source resolution
//! (`promotion_source_in_store`/`source_worktree_path`), the durable
//! promote-or-load boundary (`promote_attempt`/`promote_or_load_attempt_in_store`/
//! `persisted_promotion_for_attempt`), PR finalization
//! (`finalize_or_load_cook_pr*` and the shared `publish_cook_finalization`), the
//! `cook_report` builder, and small spec helpers. These sit downstream of a
//! terminal provider result and publish controller-owned state; grouping them
//! keeps the promote → finalize boundary in one place.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use homeboy_core::cook_status::{CookDisposition, CookStatus};
use homeboy_core::engine::canonical_json::canonical_json_bytes;
use homeboy_engine_primitives::content_hash;
use homeboy_engine_primitives::shell::{quote_arg, quote_args};

use crate::agent_task_finalization::{
    finalize_pr_with_backend, preflight_pr_with_backend, AgentTaskPrEvidence,
    AgentTaskPrFinalizationBackend, AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails,
    AgentTaskPrSourceRelationship, AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{
    candidate_fingerprint, promote_with_checkpoint_in_observation_store,
    resume_promoted_patch_in_observation_store,
    resume_promoted_patch_replacement_gates_in_observation_store, AgentTaskPromotionCandidate,
    AgentTaskPromotionReport, AgentTaskPromotionRequest, AgentTaskPromotionStatus,
};
use crate::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewEvidence, AgentTaskReviewOverride,
};
use crate::agent_task_scheduler::AgentTaskAggregate;
use homeboy_core::{Error, Result};

use super::cook::{
    canonical_candidate_finalization, canonical_cook_candidate, canonical_cook_candidate_in_store,
    cook_finalization_is_pr_receipt, AgentTaskCookAttemptReport, AgentTaskCookReport, CookRequest,
};
use super::cook_delivery_artifacts::*;
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

pub(crate) fn component_workspace_path(options: &CookRequest) -> Result<Option<PathBuf>> {
    let Some(source) = options.workspace.source_worktree_path.as_ref() else {
        return Ok(None);
    };
    let Some(component_cwd) = options
        .identity
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
        homeboy_core::resolve_contained_local_path(&source, component_cwd, "component_cwd")
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "promotion_provider.patch",
                    error.to_string(),
                    Some(component_cwd.to_string()),
                    None,
                )
            })?;
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

pub(crate) fn promotion_source_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<(String, Option<PathBuf>)> {
    match lifecycle_store.aggregate_source_exact(run_id) {
        Ok((raw, path)) => Ok((raw, Some(path))),
        Err(aggregate_error) => {
            let record = lifecycle_store.read_record(run_id)?;
            recover_executor_outcome_source(lifecycle_store, &record, &aggregate_error)
                .map(|(raw, path)| (raw, Some(path)))
        }
    }
}

/// Replayable dry-run command for a recoverable Cook, or a structured reason
/// that promotion cannot be reconstructed. Does not write aggregate state.
pub fn promotion_replay_guidance(requested_id: &str) -> Value {
    let lifecycle_store = match agent_task_lifecycle::AgentTaskLifecycleStore::from_environment() {
        Ok(store) => store,
        Err(error) => return promotion_replay_unavailable(requested_id, requested_id, &error),
    };
    let resolved = match agent_task_lifecycle::resolve_cook_reader_run_id_in_store(
        &lifecycle_store,
        requested_id,
    ) {
        Ok(run_id) => run_id,
        Err(error) => return promotion_replay_unavailable(requested_id, requested_id, &error),
    };
    let record = lifecycle_store.read_record_bounded(&resolved).ok();
    let candidate = record
        .as_ref()
        .map(preserved_candidate_evidence)
        .unwrap_or(Value::Null);
    let gates = record
        .as_ref()
        .map(preserved_gate_evidence)
        .unwrap_or(Value::Null);
    if let Some(record) = record.as_ref() {
        if lifecycle_store
            .aggregate_source_exact(&record.run_id)
            .is_ok()
            || executor_outcome_available(&lifecycle_store, record)
        {
            let command = replay_promotion_command(requested_id, &resolved, record);
            return json!({
                "available": command.is_some(),
                "requested_id": requested_id,
                "run_id": resolved,
                "command": command,
                "candidate": candidate,
                "gates": gates,
                "reason": if command.is_some() {
                    "replayable promotion dry-run"
                } else {
                    "promotion source is replayable but the cook recipe has no destination or base"
                },
            });
        }
    }
    let aggregate_error = lifecycle_store
        .aggregate_source_exact(&resolved)
        .err()
        .unwrap_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "agent-task run has no aggregate artifact yet",
                Some(resolved.clone()),
                None,
            )
        });
    let recovery_error = record.as_ref().map_or_else(
        || {
            Error::validation_invalid_argument(
                "run_id",
                "the lifecycle record has no persisted executor outcome evidence",
                Some(resolved.clone()),
                None,
            )
        },
        |record| {
            missing_promotion_source_error(
                &record.run_id,
                &aggregate_error,
                "the lifecycle record has no persisted executor outcome evidence",
            )
        },
    );
    let error = record.as_ref().map_or_else(
        || recovery_error.clone(),
        |record| {
            promotion_reconstruction_error(
                &lifecycle_store,
                requested_id,
                record,
                &aggregate_error,
                &recovery_error,
            )
        },
    );
    let mut guidance = promotion_replay_unavailable(requested_id, &resolved, &error);
    if candidate != Value::Null {
        guidance["candidate"] = candidate;
    }
    if gates != Value::Null {
        guidance["gates"] = gates;
    }
    guidance
}

fn promotion_replay_unavailable(requested_id: &str, run_id: &str, error: &Error) -> Value {
    json!({
        "available": false,
        "requested_id": requested_id,
        "run_id": error.details.get("run_id").and_then(Value::as_str).unwrap_or(run_id),
        "reason": error.message,
        "aggregate_key": error.details.get("aggregate_key").cloned().unwrap_or(Value::Null),
        "transports": error.details.get("transports").cloned().unwrap_or(Value::Null),
        "candidate": error.details.get("candidate").cloned().unwrap_or(Value::Null),
        "gates": error.details.get("gates").cloned().unwrap_or(Value::Null),
        "remediation": error.details.get("remediation").cloned().unwrap_or(Value::Null),
    })
}

fn replay_promotion_command(
    requested_id: &str,
    resolved: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<String> {
    let cook_id = record
        .metadata
        .get("cook_id")
        .and_then(Value::as_str)
        .unwrap_or(requested_id);
    let recipe = super::load_recipe(cook_id)
        .ok()
        .or_else(|| super::load_recipe_for_attempt(resolved).ok().flatten())?;
    let to_worktree = recipe.finalization.get("to_worktree")?.as_str()?;
    let base = recipe.finalization.get("base")?.as_str()?;
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "promote".to_string(),
        resolved.to_string(),
        "--to-worktree".to_string(),
        to_worktree.to_string(),
        "--base".to_string(),
        base.to_string(),
        "--gates-from-cook-recipe".to_string(),
        "--dry-run".to_string(),
    ];
    // The dry-run replays the exact persisted promotion's selectors.
    let promotion = recorded_promotion_report(&record.metadata).ok().flatten();
    let promotion = promotion.as_ref();
    if let Some(task_id) = promotion
        .and_then(|promotion| promotion.pointer("/source/task_id"))
        .and_then(Value::as_str)
    {
        command.extend(["--task-id".to_string(), task_id.to_string()]);
    }
    Some(quote_args(&command))
}

fn executor_outcome_available(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    let referenced_outcome_available =
        record
            .latest_executor_evidence
            .as_ref()
            .is_some_and(|evidence| {
                evidence
                    .outcome_ref
                    .uri
                    .strip_prefix("file://")
                    .is_some_and(|path| Path::new(path).is_file())
            });
    if referenced_outcome_available {
        return true;
    }
    record.tasks.iter().any(|task| {
        crate::agent_task_executor_evidence::executor_result_evidence_path(
            &lifecycle_store.artifact_root(),
            &record.run_id,
            &task.task_id,
        )
        .is_file()
    })
}

fn promotion_reconstruction_error(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    requested_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    aggregate_error: &Error,
    recovery_error: &Error,
) -> Error {
    let transports = aggregate_transport_report(lifecycle_store, &record.run_id);
    let missing = transports
        .iter()
        .filter(|transport| transport.get("available") != Some(&Value::Bool(true)))
        .filter_map(|transport| transport.get("transport").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let message = if missing.is_empty() {
        format!(
            "promotion source is unavailable: aggregate key `agent_task_aggregate` could not be reconstructed ({})",
            recovery_error.message
        )
    } else {
        format!(
            "promotion source is unavailable: missing aggregate key `agent_task_aggregate` on transport {}",
            missing.join(", ")
        )
    };
    Error::new(
        homeboy_core::ErrorCode::ValidationInvalidArgument,
        message,
        json!({
            "requested_id": requested_id,
            "run_id": record.run_id,
            "aggregate_key": "agent_task_aggregate",
            "transports": transports,
            "candidate": preserved_candidate_evidence(record),
            "gates": preserved_gate_evidence(record),
            "cause": recovery_error.message,
            "cause_details": recovery_error.details,
            "aggregate_error": aggregate_error.to_string(),
            "remediation": "retain or restore aggregate.json or the observation mirror key agent_task_aggregate, or the referenced executor-result evidence, before promotion",
        }),
    )
}

fn aggregate_transport_report(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Vec<Value> {
    let path = lifecycle_store.aggregate_path(run_id);
    let mirror_error = lifecycle_store
        .read_aggregate_readonly(run_id)
        .err()
        .map(|error| error.message);
    vec![
        json!({
            "transport": "aggregate.json",
            "key": path.display().to_string(),
            "available": path.is_file(),
        }),
        json!({
            "transport": "observation_mirror",
            "key": "agent_task_aggregate",
            "available": mirror_error.is_none(),
            "detail": mirror_error,
        }),
    ]
}

fn preserved_candidate_evidence(record: &agent_task_lifecycle::AgentTaskRunRecord) -> Value {
    let promotion = recorded_promotion_report(&record.metadata).ok().flatten();
    let promotion = promotion.as_ref();
    json!({
        "status": promotion.and_then(|promotion| promotion.get("status")),
        "task_id": promotion.and_then(|promotion| {
            promotion
                .pointer("/source/task_id")
                .or_else(|| promotion.get("task_id"))
        }),
        "candidate": promotion.and_then(|promotion| promotion.get("candidate")),
        "fingerprint": promotion.and_then(|promotion| promotion.pointer("/provenance/candidate")),
    })
}

fn preserved_gate_evidence(record: &agent_task_lifecycle::AgentTaskRunRecord) -> Value {
    recorded_promotion_report(&record.metadata)
        .ok()
        .flatten()
        .and_then(|mut promotion| promotion.get_mut("deterministic_gates").map(Value::take))
        .unwrap_or(Value::Null)
}

fn recover_executor_outcome_source(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    aggregate_error: &Error,
) -> Result<(String, PathBuf)> {
    let (task_id, path) = match record
        .latest_executor_evidence
        .as_ref()
        .and_then(|evidence| {
            evidence
                .outcome_ref
                .uri
                .strip_prefix("file://")
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .map(|path| (evidence.task_id.as_str(), path))
        }) {
        Some((task_id, path)) => (task_id, path),
        None => {
            let candidates = record
                .tasks
                .iter()
                .map(|task| {
                    (
                        task.task_id.as_str(),
                        crate::agent_task_executor_evidence::executor_result_evidence_path(
                            &lifecycle_store.artifact_root(),
                            &record.run_id,
                            &task.task_id,
                        ),
                    )
                })
                .filter(|(_, path)| path.is_file())
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [(task_id, path)] => (*task_id, path.clone()),
                [] => {
                    let reason = if record
                        .latest_executor_evidence
                        .as_ref()
                        .is_some_and(|evidence| !evidence.outcome_ref.uri.starts_with("file://"))
                    {
                        "the executor outcome evidence is not a local file reference"
                    } else {
                        "the lifecycle record has no persisted executor outcome evidence"
                    };
                    return Err(missing_promotion_source_error(
                        &record.run_id,
                        aggregate_error,
                        reason,
                    ));
                }
                _ => {
                    return Err(missing_promotion_source_error(
                        &record.run_id,
                        aggregate_error,
                        "multiple persisted executor outcomes exist; select a task-specific promotion source",
                    ));
                }
            }
        }
    };
    let canonical_path = path.canonicalize().map_err(|error| {
        missing_promotion_source_error(
            &record.run_id,
            aggregate_error,
            &format!("the executor outcome evidence cannot be read: {error}"),
        )
    })?;
    let canonical_artifact_root =
        lifecycle_store
            .artifact_root()
            .canonicalize()
            .map_err(|error| {
                missing_promotion_source_error(
                    &record.run_id,
                    aggregate_error,
                    &format!("the artifact root cannot be resolved: {error}"),
                )
            })?;
    if !canonical_path.starts_with(&canonical_artifact_root) {
        return Err(missing_promotion_source_error(
            &record.run_id,
            aggregate_error,
            "the executor outcome evidence is outside the configured artifact root",
        ));
    }

    let source = fs::read_to_string(&canonical_path).map_err(|error| {
        missing_promotion_source_error(
            &record.run_id,
            aggregate_error,
            &format!("the executor outcome evidence cannot be read: {error}"),
        )
    })?;
    let outcome: crate::agent_task::AgentTaskOutcome =
        serde_json::from_str(&source).map_err(|error| {
            missing_promotion_source_error(
                &record.run_id,
                aggregate_error,
                &format!("the executor outcome evidence is invalid JSON: {error}"),
            )
        })?;
    if outcome.schema != crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA
        || outcome.task_id != task_id
        || !record.tasks.iter().any(|task| task.task_id == task_id)
    {
        return Err(missing_promotion_source_error(
            &record.run_id,
            aggregate_error,
            "the executor outcome evidence does not match the lifecycle record",
        ));
    }

    agent_task_lifecycle::recover_single_task_aggregate_from_executor_outcome_in_store(
        lifecycle_store,
        record,
        outcome,
    )?;
    lifecycle_store.aggregate_source_exact(&record.run_id)
}

fn missing_promotion_source_error(
    run_id: &str,
    aggregate_error: &Error,
    recovery_error: &str,
) -> Error {
    Error::new(
        homeboy_core::ErrorCode::ValidationInvalidArgument,
        "promotion source is unavailable",
        json!({
            "run_id": run_id,
            "aggregate_error": aggregate_error.to_string(),
            "executor_evidence_error": recovery_error,
            "remediation": "retain or restore aggregate.json or the referenced executor-result evidence before promotion",
        }),
    )
}

pub(super) fn cook_candidate_base_sha(options: &CookRequest) -> Option<String> {
    options
        .identity
        .initial_plan
        .metadata
        .get("cook_workspace_base_snapshot")
        .filter(|snapshot| {
            snapshot.get("mode").and_then(Value::as_str) == Some("isolated_attempt_snapshot")
        })
        .and_then(|snapshot| snapshot.get("resolved_base"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| options.workspace.task_base_sha.clone())
}

pub(crate) fn promote_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let (source, source_path) = promotion_source_in_store(lifecycle_store, run_id)?;
    let selected_task_id = selected_candidate_task_id_in_store(lifecycle_store, run_id)?;
    let artifact_id = None;
    let repository_integrity_evidence =
        admitted_repository_integrity_evidence(lifecycle_store, run_id)?;
    let observation_store = lifecycle_store.open_observation_initialized()?;
    crate::agent_task_promotion::with_gate_supervision(
        cook_gate_supervision(lifecycle_store, options, run_id),
        || {
            promote_with_checkpoint_in_observation_store(
                AgentTaskPromotionRequest {
                    source,
                    source_run_id: Some(run_id.to_string()),
                    source_path,
                    source_worktree_path: component_workspace_path(options)?
                        .or_else(|| options.workspace.source_worktree_path.clone()),
                    base_ref: Some(options.finalization.base.clone()),
                    task_base_sha: cook_candidate_base_sha(options),
                    candidate_ref: None,
                    to_worktree: options.workspace.to_worktree.clone(),
                    task_id: selected_task_id,
                    artifact_id,
                    dry_run: false,
                    gates: options.gates.clone(),
                    provider_invocation: options.provider_transport.provider_invocation.clone(),
                    repository_integrity_evidence,
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
        },
    )
}

/// Fresh and resumed gates share budgets and durable progress reporting.
fn cook_gate_supervision(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
) -> crate::agent_task_gate::GateSupervision {
    let run_id = run_id.to_string();
    let gate_store = lifecycle_store.clone();
    let cook_id = options.identity.cook_id.clone();
    crate::agent_task_gate::GateSupervision {
        timeout: options.gates.gate_timeout(),
        no_progress_timeout: options.gates.gate_no_progress_timeout(),
        heartbeat_interval: options.gates.gate_heartbeat_interval(),
        on_spawn: Arc::new({
            let run_id = run_id.clone();
            let gate_store = gate_store.clone();
            move |_, command| {
                agent_task_lifecycle::record_promotion_progress_in_store(
                    &gate_store,
                    &run_id,
                    "gate",
                    Some(command),
                    Some("gate process started"),
                    None,
                )
            }
        }),
        on_heartbeat: Arc::new({
            let run_id = run_id.clone();
            let gate_store = gate_store.clone();
            move |status| {
                agent_task_lifecycle::record_promotion_progress_in_store(
                    &gate_store,
                    &run_id,
                    "gate",
                    None,
                    Some(&format!(
                        "gate elapsed={}ms last-progress={}ms",
                        status.elapsed_ms,
                        status.last_progress_ms_ago.unwrap_or(status.elapsed_ms),
                    )),
                    Some(&status.output_tail),
                )
            }
        }),
        is_cancelled: Arc::new({
            let gate_store = gate_store.clone();
            move || {
                gate_store
                    .read_cook_index(&cook_id)
                    .is_ok_and(|index| index.cancellation_fence.is_some())
            }
        }),
    }
}

fn admitted_repository_integrity_evidence(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<homeboy_core::repository_integrity::RepositoryIntegrityEvidence>> {
    let record = lifecycle_store.read_record(run_id)?;
    let evidence = record
        .metadata
        .pointer("/source_checkout/repository_integrity_evidence")
        .or_else(|| {
            record
                .metadata
                .pointer("/runner_handoff/source_snapshot/repository_integrity_evidence")
        })
        .or_else(|| {
            record
                .metadata
                .pointer("/source_snapshot/repository_integrity_evidence")
        });
    evidence
        .map(|evidence| {
            serde_json::from_value(evidence.clone()).map_err(|error| {
                Error::validation_invalid_argument(
                    "source_snapshot.repository_integrity_evidence",
                    format!("persisted source snapshot evidence is invalid: {error}"),
                    Some(run_id.to_string()),
                    None,
                )
            })
        })
        .transpose()
}

/// The provider outcome a continuation promotes, with its aggregate.
fn continuation_outcome<'a>(
    run_id: &str,
    aggregate: Option<&'a AgentTaskAggregate>,
) -> Result<(
    &'a AgentTaskAggregate,
    &'a crate::agent_task::AgentTaskOutcome,
)> {
    let aggregate = aggregate.ok_or_else(|| {
        Error::validation_invalid_argument(
            "agent_task_aggregate",
            "authoritative observation has no mirrored agent-task aggregate",
            Some(run_id.to_string()),
            None,
        )
    })?;
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
    Ok((aggregate, outcome))
}

/// The outcome's immutable candidate, or `None` for a verified no-change
/// outcome. Any other outcome without a candidate cannot be promoted.
fn continuation_candidate(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Result<Option<&crate::agent_task::AgentTaskCandidate>> {
    if outcome.candidate.is_none()
        && (outcome.is_verified_unchanged_attempt()
            || outcome.status == crate::agent_task::AgentTaskOutcomeStatus::NoOp)
    {
        return Ok(None);
    }
    outcome.candidate.as_ref().map(Some).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source.candidate",
            "non-recoverable: outcome has no immutable candidate; run a new Cook",
            None,
            None,
        )
    })
}

/// Why continuation cannot admit this terminal attempt's candidate: the
/// `candidate_admission` check `recover --preflight` runs, read from the same
/// durable evidence, so the planner never names a continuation it refuses.
pub(crate) fn continuation_candidate_refusal(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<String> {
    if !super::cook_continuation_requires_model_provenance(record)
        || recorded_finalization_receipt(&record.metadata).is_some_and(|receipt| !receipt.is_null())
    {
        return None;
    }
    let promotion = persisted_promotion_from_record(&record.run_id, record)
        .ok()
        .flatten();
    if promotion.as_ref().is_some_and(|promotion| {
        !matches!(
            promotion.status,
            AgentTaskPromotionStatus::VerificationPending
                | AgentTaskPromotionStatus::GateEnvironmentUnavailable
        )
    }) {
        return None;
    }
    let (_, aggregate) = lifecycle_store
        .read_record_with_aggregate_bounded(&record.run_id)
        .ok()?;
    let result =
        continuation_outcome(&record.run_id, aggregate.as_ref()).and_then(|(_, outcome)| {
            if promotion.is_some() {
                Ok(None)
            } else {
                continuation_candidate(outcome)
            }
        });
    result.err().map(|error| error.message)
}

pub fn preflight_cook_promotion_for_observation_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
    artifact_id: Option<&str>,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    aggregate: Option<&AgentTaskAggregate>,
) -> Result<Value> {
    if let Some(promotion) = persisted_promotion_from_record(run_id, record)? {
        let behavior = if promotion.status == AgentTaskPromotionStatus::GateEnvironmentUnavailable {
            "resume_deferred_gates"
        } else if promotion.status == AgentTaskPromotionStatus::VerificationPending {
            "resume_verification_pending"
        } else {
            "load_persisted"
        };
        if !matches!(
            promotion.status,
            AgentTaskPromotionStatus::VerificationPending
                | AgentTaskPromotionStatus::GateEnvironmentUnavailable
        ) {
            return Ok(json!({
                "behavior": behavior,
                "status": promotion.status,
                "candidate": promotion.candidate,
                "candidate_fingerprint": promotion.provenance.get("candidate").cloned().unwrap_or(Value::Null),
                "execution_only_checks": persisted_promotion_execution_only_checks(
                    promotion.status,
                    options.finalization.no_finalize,
                ),
            }));
        }
    }
    let (aggregate, outcome) = continuation_outcome(run_id, aggregate)?;
    if let Some(promotion) = persisted_promotion_from_record(run_id, record)? {
        let behavior = if promotion.status == AgentTaskPromotionStatus::GateEnvironmentUnavailable {
            "resume_deferred_gates"
        } else if promotion.status == AgentTaskPromotionStatus::VerificationPending {
            "resume_verification_pending"
        } else {
            "load_persisted"
        };
        return Ok(json!({
            "behavior": behavior,
            "status": promotion.status,
            "candidate": promotion.candidate,
            "candidate_fingerprint": promotion.provenance.get("candidate").cloned().unwrap_or(Value::Null),
            "execution_only_checks": persisted_promotion_execution_only_checks(
                promotion.status,
                options.finalization.no_finalize,
            ),
        }));
    }
    let source = serde_json::to_string_pretty(&aggregate).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task aggregate {run_id}")),
        )
    })?;
    let promotion_options = AgentTaskPromotionRequest {
        source,
        source_run_id: Some(run_id.to_string()),
        source_path: Some(lifecycle_store.aggregate_path(run_id)),
        source_worktree_path: component_workspace_path(options)?
            .or_else(|| options.workspace.source_worktree_path.clone()),
        base_ref: Some(options.finalization.base.clone()),
        task_base_sha: cook_candidate_base_sha(options),
        candidate_ref: None,
        to_worktree: options.workspace.to_worktree.clone(),
        task_id: Some(outcome.task_id.clone()),
        artifact_id: artifact_id.map(str::to_string),
        dry_run: false,
        gates: options.gates.clone(),
        provider_invocation: options.provider_transport.provider_invocation.clone(),
        repository_integrity_evidence: None,
    };
    let Some(candidate) = continuation_candidate(outcome)? else {
        return Ok(
            json!({"behavior": "no_changes", "artifact_id": Value::Null, "candidate_fingerprint": Value::Null, "execution_only_checks": []}),
        );
    };
    if let Some(path) = promotion_options.source_worktree_path.as_deref() {
        crate::agent_task_promotion::validate_candidate_commit(path, candidate)?;
    }
    Ok(json!({
        "behavior": "promote_new",
        "artifact_id": Value::Null,
        "candidate_fingerprint": candidate,
        "execution_only_checks": ["promotion_target_resolution", "candidate_materialization", "verification_gates"],
    }))
}

fn persisted_promotion_execution_only_checks(
    status: AgentTaskPromotionStatus,
    no_finalize: bool,
) -> Vec<&'static str> {
    let mut checks = Vec::new();
    if status == AgentTaskPromotionStatus::VerificationPending {
        checks.extend([
            "promotion_target_resolution",
            "patch_presence",
            "verification_gates",
        ]);
    }
    if status == AgentTaskPromotionStatus::GateFailed {
        checks.push("gate_failure_differential");
    }
    checks.push("cook_loop_evaluation");
    if !no_finalize && status.patch_promoted() {
        checks.push("finalization");
    }
    checks
}

#[cfg(test)]
mod preflight_report_tests {
    use super::*;

    #[test]
    fn every_persisted_promotion_status_reports_remaining_execution() {
        for status in [
            AgentTaskPromotionStatus::DryRun,
            AgentTaskPromotionStatus::VerificationPending,
            AgentTaskPromotionStatus::Applied,
            AgentTaskPromotionStatus::GateFailed,
            AgentTaskPromotionStatus::VerifiedNoChanges,
            AgentTaskPromotionStatus::NoChangesGateFailed,
            AgentTaskPromotionStatus::NoChanges,
        ] {
            let checks = persisted_promotion_execution_only_checks(status, false);
            assert!(checks.contains(&"cook_loop_evaluation"), "{status:?}");
            if status.patch_promoted() {
                assert!(checks.contains(&"finalization"), "{status:?}");
            }
        }

        let verification = persisted_promotion_execution_only_checks(
            AgentTaskPromotionStatus::VerificationPending,
            true,
        );
        assert!(verification.contains(&"promotion_target_resolution"));
        assert!(verification.contains(&"patch_presence"));
        assert!(verification.contains(&"verification_gates"));
        assert!(!verification.contains(&"finalization"));
    }
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
    options: &CookRequest,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let timing_store = lifecycle_store
        .open_observation_initialized()
        .map_err(|error| {
            homeboy_core::log_status!("observe", "phase timing persistence failed: {}", error);
        })
        .ok();
    let _recovery = timing_store.as_ref().map(|store| {
        store
            .phase_timer(run_id)
            .into_guard("promote.recover.candidate")
    });
    let _identities = crate::agent_task_gate::placement::runner_identity_scope();
    if let Some(promotion) = persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)? {
        // Infrastructure deferral has no verdict to replay. Reverify the
        // fingerprint-bound candidate without invoking the provider again.
        if !matches!(
            promotion.status,
            AgentTaskPromotionStatus::VerificationPending
                | AgentTaskPromotionStatus::GateEnvironmentUnavailable
        ) {
            if promotion.provenance.get("replacement_gate_proof").is_some() {
                validate_replacement_promotion_in_store(lifecycle_store, &promotion)?;
            }
            return Ok(promotion);
        }
    }
    if let Some(promotion) = persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)? {
        if matches!(
            promotion.status,
            AgentTaskPromotionStatus::VerificationPending
                | AgentTaskPromotionStatus::GateEnvironmentUnavailable
        ) {
            let target_path = promotion.target.path.as_deref().or_else(|| {
                promotion.provenance.get("worktree_path").and_then(Value::as_str)
            }).map(PathBuf::from).or_else(|| {
                homeboy_core::worktree_provider::resolve_worktree_ownership_if_present(&promotion.to_worktree)
                    .ok()
                    .flatten()
                    .map(|workspace| PathBuf::from(workspace.path))
            }).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion.target.path",
                    "verification-pending promotion has no durable candidate worktree path or registered worktree handle",
                    Some(run_id.to_string()),
                    None,
                )
            })?;
            let (source, source_path) =
                persisted_promotion_source_in_store(lifecycle_store, &promotion, run_id)?;
            let observation_store = lifecycle_store.open_observation_initialized()?;
            let resumed = crate::agent_task_promotion::with_gate_supervision(
                cook_gate_supervision(lifecycle_store, options, run_id),
                || {
                    resume_promoted_patch_in_observation_store(
                        AgentTaskPromotionRequest {
                            source,
                            source_run_id: Some(run_id.to_string()),
                            source_path,
                            source_worktree_path: component_workspace_path(options)?
                                .or_else(|| options.workspace.source_worktree_path.clone()),
                            base_ref: Some(options.finalization.base.clone()),
                            task_base_sha: cook_candidate_base_sha(options),
                            candidate_ref: None,
                            to_worktree: options.workspace.to_worktree.clone(),
                            // A resumed verification must retain the scheduler-selected
                            // candidate rather than falling back to aggregate outcome order.
                            task_id: selected_candidate_task_id_in_store(lifecycle_store, run_id)?,
                            // The checkpoint already authenticated this exact artifact;
                            // retain its identity rather than selecting an equivalent alias.
                            artifact_id: None,
                            dry_run: false,
                            gates: options.gates.clone(),
                            provider_invocation: options
                                .provider_transport
                                .provider_invocation
                                .clone(),
                            repository_integrity_evidence: promotion
                                .repository_integrity_evidence
                                .clone(),
                        },
                        &target_path,
                        &serde_json::to_value(&promotion)
                            .map_err(|error| Error::internal_json(error.to_string(), None))?,
                        &observation_store,
                    )
                },
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

pub(crate) fn persisted_promotion_for_attempt(
    run_id: &str,
) -> Result<Option<AgentTaskPromotionReport>> {
    let record = agent_task_lifecycle::reconcile_status(run_id)?;
    persisted_promotion_from_record(run_id, &record)
}

pub(crate) fn persisted_promotion_from_record(
    requested_run_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<AgentTaskPromotionReport>> {
    let Some(value) = recorded_promotion_report(&record.metadata)? else {
        return Ok(None);
    };
    let mut promotion: AgentTaskPromotionReport =
        serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "cook_delivery.artifacts.promotion",
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
            "cook_delivery.artifacts.promotion.source.run_id",
            "persisted cook promotion does not belong to this attempt",
            Some(requested_run_id.to_string()),
            None,
        );
        error.details["requested_run_id"] = serde_json::json!(requested_run_id);
        error.details["resolved_run_id"] = serde_json::json!(record.run_id);
        error.details["promotion_run_id"] = serde_json::json!(promotion.source.run_id);
        return Err(error);
    }
    promotion.normalize_gate_outcome();
    Ok(Some(promotion))
}

pub(crate) fn persisted_promotion_for_attempt_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<AgentTaskPromotionReport>> {
    let mut record = lifecycle_store.read_record(run_id)?;
    if let Some(value) = record
        .metadata
        .pointer_mut("/cook_delivery/artifacts/promotion")
    {
        *value = homeboy_core::observation::store::metadata_payloads::resolve_metadata_payload(
            value,
            &lifecycle_store
                .open_observation_initialized()?
                .artifact_root()?,
        )?;
    }
    persisted_promotion_from_record(run_id, &record)
}

/// Whether `promotion` is an already-applied candidate whose verification
/// failed or never completed and that no replacement proof has made
/// finalization-eligible. This is the only state replacement gates accept.
pub(crate) fn applied_candidate_needs_replacement_gates(
    promotion: &AgentTaskPromotionReport,
) -> bool {
    promotion.status.applied_without_passing_verdict()
        && promotion.provenance.get("replacement_gate_proof").is_none()
        && !promotion.finalization_eligible()
}

/// Read-only admission for `recover --preflight` with replacement-gate
/// inputs: the attempt must hold an applied candidate whose gates failed.
pub(crate) fn ensure_replacement_gate_candidate(run_id: &str) -> Result<()> {
    let admitted = persisted_promotion_for_attempt(run_id)?
        .is_some_and(|promotion| applied_candidate_needs_replacement_gates(&promotion));
    if admitted {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "replace-gates",
        "replacement gates require an already-applied candidate whose verification failed, deferred or did not complete",
        Some(run_id.to_string()),
        Some(vec![format!("homeboy agent-task status {run_id}")]),
    ))
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
            "cook_delivery.artifacts.promotion",
            "replacement gate proof requires a persisted failed promotion",
            Some(run_id.to_string()),
            None,
        )
    })?;
    if original.provenance.get("replacement_gate_proof").is_some() {
        if original.finalization_eligible()
            && replacement.source == original.source
            && replacement.target == original.target
            && replacement.to_worktree == original.to_worktree
            && replacement.candidate == original.candidate
            && replacement.changed_files == original.changed_files
            && replacement.verified_base == original.verified_base
            && replacement.deterministic_gates == original.deterministic_gates
            && replacement.command_evidence == original.command_evidence
        {
            return Ok(original);
        }
        return Err(Error::validation_invalid_argument(
            "replacement_gate_proof",
            "replacement proof is already recorded; replay its exact evidence",
            Some(run_id.to_string()),
            None,
        ));
    }
    if !original.status.applied_without_passing_verdict() {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.status",
            "replacement gate proof is only valid for an already-applied candidate whose verification failed, deferred or did not complete",
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
    // Replacement gates authenticate the already-admitted candidate; they must
    // never replace or discard the policy evidence bound at original promotion.
    replacement.repository_integrity_evidence = original.repository_integrity_evidence.clone();
    replacement.normalize_gate_outcome();
    validate_replacement_proof_finalization_eligibility(run_id, &replacement)?;
    validate_legacy_replacement_candidate_checkout(run_id, &original, &replacement)?;
    let expected_candidate = original.provenance.get("candidate_commit").cloned();
    let observed_candidate = replacement.provenance.get("candidate_commit").cloned();
    let same_candidate = observed_candidate == expected_candidate;
    let drifted = serde_json::json!({
        "run_id": replacement.source.run_id.as_deref() != Some(run_id),
        "target": replacement.target.worktree != original.target.worktree || replacement.target.path != original.target.path,
        "worktree": replacement.to_worktree != original.to_worktree,
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
            "expected": expected_candidate,
            "observed": observed_candidate,
        });
        return Err(error);
    }
    let record = agent_task_lifecycle::reconcile_status(run_id)?;
    let latest = recorded_promotion_report(&record.metadata)?;
    let history = recorded_promotion_history(&record.metadata)?;
    let (original_history_index, original_history) = history
        .as_ref()
        .zip(latest.as_ref())
        .and_then(|(promotions, latest)| {
            let index = promotions
                .iter()
                .rposition(|promotion| promotion == latest)?;
            Some((index, &promotions[index]))
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_delivery.artifacts.promotion",
                "replacement gate proof requires the original immutable promotion in history",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let original_digest =
        content_hash::sha256_hex(&canonical_json_bytes(original_history).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize original promotion history".to_string()),
            )
        })?);
    let reason = match original.status {
        AgentTaskPromotionStatus::VerificationPending => "interrupted_original_verification",
        _ => "infrastructure_invalid_original_gates",
    };
    replacement.provenance["replacement_gate_proof"] = serde_json::json!({
        "schema": "homeboy/agent-task-replacement-gate-proof/v1",
        "original_history": {
            "run_id": record.run_id,
            "metadata_key": "cook_delivery.artifacts.promotions",
            "index": original_history_index,
            "status": original.status,
            "deterministic_gate_count": original.deterministic_gates.len(),
            "sha256": original_digest,
        },
        "reason": reason,
        "operator_authorization": external_authorization,
        "externally_produced": true,

        // `Inherit` serializes as the default in gate reports; recording the
        // resolved policy here keeps that valid standard policy explicit.
        "environment_policy": replacement.deterministic_gates.iter().map(|gate| serde_json::json!({
            "gate_id": gate.id,
            "runner_id": gate.environment.lab_runner,
            "lab_receipt": gate.lab_receipt,
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

fn canonical_candidate_fingerprint(candidate: Option<&Value>) -> Value {
    let Some(candidate) = candidate else {
        return Value::Null;
    };
    serde_json::from_value::<AgentTaskPromotionCandidate>(candidate.clone())
        .and_then(serde_json::to_value)
        // Preserve malformed legacy data verbatim so an identity mismatch is
        // still inspectable rather than silently treated as equivalent.
        .unwrap_or_else(|_| candidate.clone())
}

/// Both continuation and adoption consume the same admitted proof and candidate.
/// A green projection alone cannot authorize recovery of a failed provider run.
pub(super) fn validate_replacement_promotion_in_store(
    store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    promotion: &AgentTaskPromotionReport,
) -> Result<()> {
    let run_id = promotion.source.run_id.as_deref().unwrap_or_default();
    let record = store.read_record(run_id)?;
    let proof = &promotion.provenance["replacement_gate_proof"];
    let reference = &proof["original_history"];
    let history = recorded_promotion_history(&record.metadata)?;
    let original = reference["index"]
        .as_u64()
        .and_then(|index| history.as_ref()?.get(index as usize));
    let valid_history = original.is_some_and(|original| {
        reference["run_id"] == run_id
            && reference["metadata_key"] == "cook_delivery.artifacts.promotions"
            && canonical_json_bytes(original)
                .is_ok_and(|bytes| reference["sha256"] == content_hash::sha256_hex(&bytes))
            && serde_json::from_value::<AgentTaskPromotionReport>(original.clone()).is_ok_and(
                |original| {
                    original.status.applied_without_passing_verdict()
                        && original.source.run_id == promotion.source.run_id
                        && original.source.task_id == promotion.source.task_id
                        && original.target.worktree == promotion.target.worktree
                        && original.target.path == promotion.target.path
                        && original.to_worktree == promotion.to_worktree
                        && original.changed_files == promotion.changed_files
                        && original.verified_base == promotion.verified_base
                        && original.provenance.get("candidate_commit").is_some()
                        && original.provenance.get("candidate_commit")
                            == promotion.provenance.get("candidate_commit")
                },
            )
    });
    if !crate::agent_task_finalization::authorized_replacement_gate_proof(promotion)
        || !valid_history
        || selected_candidate_task_id_in_store(store, run_id)?.as_deref()
            != Some(promotion.source.task_id.as_str())
    {
        return Err(Error::validation_invalid_argument(
            "replacement_gate_proof",
            "recovery requires authorized proof bound to the selected candidate and immutable original promotion",
            Some(run_id.to_string()),
            None,
        ));
    }
    validate_replacement_proof_finalization_eligibility(run_id, promotion)?;
    // A completed publication can change HEAD and consume the dirty candidate.
    // Its durable receipt, rather than the pre-publication tree, owns replay.
    if !super::CookDelivery::from_run_metadata(&record.metadata)
        .is_some_and(|delivery| delivery.has_publication_receipt())
    {
        let path = promotion.target.path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_delivery.artifacts.promotion.target.path",
                "corrected-gate recovery requires its recorded candidate destination",
                Some(run_id.to_string()),
                None,
            )
        })?;
        let current =
            serde_json::to_value(crate::agent_task_promotion::candidate_fingerprint(path)?)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
        if canonical_candidate_fingerprint(Some(&current))
            != canonical_candidate_fingerprint(promotion.provenance.get("candidate"))
        {
            return Err(Error::validation_invalid_argument(
                "candidate",
                "corrected-gate candidate changed after verification",
                Some(run_id.to_string()),
                None,
            ));
        }
    }
    Ok(())
}

/// Older failed promotions did not retain the checkout used by their gates.
/// Their immutable candidate fingerprint remains sufficient to authenticate a
/// replacement checkout, but only when its content tree matches.
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
    let checkout = replacement
        .provenance
        .get("candidate_checkout")
        .ok_or_else(|| failed("legacy_candidate_checkout"))?;
    if checkout.get("tree").and_then(Value::as_str) != Some(tree) {
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

/// Name the gate(s) that kept a replacement proof from being green, with
/// bounded output tails, so operators can act without re-reading the run.
fn replacement_green_gate_status_error(
    run_id: &str,
    replacement: &AgentTaskPromotionReport,
) -> Error {
    const TAIL_LINES: usize = 40;
    let failing = replacement
        .finalization_blocking_gates()
        .collect::<Vec<_>>();
    let tail = |captured: &str, evidence: Option<&str>| {
        let source = if captured.is_empty() {
            evidence.unwrap_or_default()
        } else {
            captured
        };
        bounded_gate_detail(&crate::agent_task_gate::text_tail(source, TAIL_LINES))
    };
    let failing_gates = failing
        .iter()
        .map(|gate| {
            let evidence = gate.failure_evidence.as_ref();
            let executed = gate.skip_reason.is_none()
                && !matches!(
                    gate.status,
                    crate::agent_task_gate::AgentTaskGateStatus::Skipped
                        | crate::agent_task_gate::AgentTaskGateStatus::Deferred
                        | crate::agent_task_gate::AgentTaskGateStatus::Unavailable
                );
            serde_json::json!({
                "gate_id": gate.id,
                "status": gate.status,
                "exit_code": gate.exit_code,
                "termination": gate.termination,
                "executed": executed,
                "skip_reason": gate.skip_reason,
                "stdout_tail": tail(&gate.stdout, evidence.map(|e| e.stdout_tail.as_str())),
                "stderr_tail": tail(&gate.stderr, evidence.map(|e| e.stderr_tail.as_str())),
            })
        })
        .collect::<Vec<_>>();
    let suffix = match failing.first() {
        Some(gate) if failing_gates[0]["executed"] == false => format!(
            " (gate `{}` {}: {})",
            gate.id,
            failing_gates[0]["status"].as_str().unwrap_or("unavailable"),
            gate.skip_reason
                .as_ref()
                .map(|skip| skip.reason.as_str())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| failing_gates[0]["stderr_tail"].as_str().unwrap_or_default()),
        ),
        Some(gate) => format!(" (gate `{}` exited {})", gate.id, gate.exit_code),
        None => String::new(),
    };
    let mut error = Error::validation_invalid_argument(
        "replacement_gate_proof",
        format!(
            "replacement proof is not finalization-eligible: failed `green_gate_status`{suffix}"
        ),
        Some(run_id.to_string()),
        None,
    );
    error.details["failed_eligibility_predicate"] = serde_json::json!("green_gate_status");
    if let Some(gate) = failing.first() {
        error.details["gate_id"] = serde_json::json!(gate.id);
    }
    error.details["failing_gates"] = serde_json::json!(failing_gates);
    error
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

    if !replacement.status.patch_promoted() {
        return Err(failed("promoted_status", None));
    }
    if replacement.deterministic_gates.is_empty() {
        return Err(failed("non_empty_deterministic_gates", None));
    }
    if replacement.verified_base.is_none() {
        return Err(failed("verified_base", None));
    }
    if !replacement.finalization_eligible() {
        return Err(replacement_green_gate_status_error(run_id, replacement));
    }
    let candidate_checkout = replacement.provenance.get("candidate_checkout");
    for gate in &replacement.deterministic_gates {
        if gate.command.is_empty() {
            return Err(failed("command", Some(&gate.id)));
        }
        if !replacement.command_evidence.iter().any(|evidence| {
            evidence.command == gate.command
                && match gate.status {
                    crate::agent_task_gate::AgentTaskGateStatus::Succeeded => {
                        evidence.exit_code == 0
                    }
                    crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure => {
                        evidence.exit_code == gate.exit_code
                    }
                    _ => false,
                }
        }) {
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
    let candidate_bound_gates = visible_gates
        .iter()
        .copied()
        .filter(|gate| match gate.status {
            crate::agent_task_gate::AgentTaskGateStatus::Succeeded => gate.exit_code == 0,
            crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure => true,
            _ => false,
        })
        .collect::<Vec<_>>();
    if candidate_bound_gates.is_empty() {
        return Err(failed("candidate_checkout", None));
    }
    if !candidate_bound_gates.iter().any(|gate| {
        gate.invocation()
            .map(|invocation| invocation.reviewer_command())
            .is_ok_and(|command| {
                crate::agent_task_review_dossier::reviewer_runnable_command(&command)
            })
    }) {
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

/// Why a replacement gate execution runs. The mode decides how a dead start
/// fence is handled and whether a non-passing gate run is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplacementGateMode {
    /// Operator replacement proof. A start fence whose owner died requires
    /// explicit interrupted-rerun authorization before shell gates run again.
    Replacement {
        interrupted_rerun_authorization: Option<String>,
    },
    /// `recover --regate`: re-run deterministic gates on the candidate the
    /// Cook already produced. It spends no provider budget and the operator
    /// asked for exactly this rerun, so a dead start fence is superseded with
    /// a recorded reason, and every gate run becomes the latest promotion so
    /// status and diagnose never keep projecting a stale receipt (#18315).
    Regate,
}

pub fn verify_replacement_gates(
    cook_or_attempt_id: &str,
    gates: crate::agent_task_gate::VerifyGateOptions,
    external_authorization: String,
    interrupted_rerun_authorization: Option<String>,
) -> Result<AgentTaskPromotionReport> {
    verify_replacement_gates_in_mode(
        cook_or_attempt_id,
        gates,
        external_authorization,
        ReplacementGateMode::Replacement {
            interrupted_rerun_authorization,
        },
    )
}

/// Re-run deterministic gates on the candidate for `recover --regate`.
///
/// A passing run records replacement gate proof, exactly like
/// [`verify_replacement_gates`]. A failing or unavailable run is returned
/// (not raised) after it is persisted as the attempt's latest promotion, so
/// status, diagnose and the Cook report show the new gate run while the
/// promotion history keeps the earlier one. A start fence left by an
/// interrupted earlier execution whose owner is gone is superseded instead of
/// demanding interrupted-rerun authorization (#18042, #18075).
pub(crate) fn regate_candidate_gates(
    cook_or_attempt_id: &str,
    gates: crate::agent_task_gate::VerifyGateOptions,
    authorization: String,
) -> Result<AgentTaskPromotionReport> {
    verify_replacement_gates_in_mode(
        cook_or_attempt_id,
        gates,
        authorization,
        ReplacementGateMode::Regate,
    )
}

fn verify_replacement_gates_in_mode(
    cook_or_attempt_id: &str,
    gates: crate::agent_task_gate::VerifyGateOptions,
    external_authorization: String,
    mode: ReplacementGateMode,
) -> Result<AgentTaskPromotionReport> {
    let run_id = super::cook_recipe::resolve_cook_continuation_run_id(cook_or_attempt_id)?;
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let operation_key = format!("verify-replacement:{run_id}");
    match lifecycle_store.claim_cook_operation(
        &run_id,
        &operation_key,
        REPLACEMENT_GATE_PROOF_CLAIM_LEASE,
    )? {
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            let report: AgentTaskPromotionReport =
                serde_json::from_value(result).map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("deserialize completed replacement gate proof".to_string()),
                    )
                })?;
            if !report.finalization_eligible() {
                return Err(Error::validation_invalid_argument(
                    "replacement_gate_proof",
                    "completed replacement proof is not finalization-eligible; inspect the persisted evidence with agent-task status",
                    Some(run_id),
                    None,
                ));
            }
            Ok(report)
        }
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            let claim = lifecycle_store.operation_claim(&run_id, &operation_key)?;
            let mut error = Error::validation_invalid_argument(
                "replacement_gate_proof",
                "operation_in_progress",
                Some(operation_key),
                Some(vec![format!("homeboy agent-task status {run_id}")]),
            );
            error.details["claim"] = serde_json::to_value(claim).unwrap_or(Value::Null);
            Err(error)
        }
        agent_task_lifecycle::ClaimOutcome::Acquired(_lease) => {
            let result = verify_replacement_gates_owned(
                &lifecycle_store,
                &run_id,
                gates,
                external_authorization,
                mode,
            );
            // Gates that returned, green or failing, are finished, not interrupted.
            lifecycle_store.mutate_record(&run_id, |record| {
                finish_replacement_gate_execution(&mut record.metadata)
            })?;
            match result {
                // A regate that ran but did not pass is persisted evidence, not
                // a completed proof: fail the claim so the next regate runs
                // fresh gates instead of replaying this result.
                Ok(report) if !report.finalization_eligible() => {
                    _lease.fail(serde_json::json!({
                        "code": "regate_gates_not_passing",
                        "message": format!("regate gates did not pass (status: {:?})", report.status),
                    }))?;
                    Ok(report)
                }
                Ok(report) => {
                    _lease.complete(
                        serde_json::to_value(&report)
                            .map_err(|error| Error::internal_json(error.to_string(), None))?,
                    )?;
                    Ok(report)
                }
                Err(error) => {
                    _lease.fail(serde_json::json!({
                        "code": error.code.as_str(),
                        "message": error.message.clone(),
                    }))?;
                    Err(error)
                }
            }
        }
    }
}

/// Supervision for replacement gates, built from the declared gate options
/// exactly as Cook promotion builds it (#15856).
///
/// A foreground client running them (`agent-task recover`) cancels them when
/// it is signalled or outlives its admission bound (#18042). The scope is
/// captured here because gate supervision may be polled off this thread.
fn replacement_gate_supervision(
    gates: &crate::agent_task_gate::VerifyGateOptions,
) -> crate::agent_task_gate::GateSupervision {
    let mut supervision = gates.declared_supervision();
    if let Some(scope) = homeboy_core::foreground_wait::current() {
        let declared = supervision.is_cancelled.clone();
        supervision.is_cancelled =
            std::sync::Arc::new(move || declared() || scope.stop_requested().is_some());
    }
    supervision
}

/// A foreground client that was signalled or outlived its admission bound
/// returns that stop instead of recording whatever the cancelled gates
/// reported: they did not judge the candidate (#18042).
fn foreground_gate_stop() -> Option<Error> {
    let scope = homeboy_core::foreground_wait::current()?;
    let stop = scope.stop_requested()?;
    Some(stop.error(
        "replacement gate execution",
        scope.last_subject().as_ref(),
        "any queued or running runner work was withdrawn, the operation lease was released and no start fence was kept",
    ))
}

fn verify_replacement_gates_owned(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    gates: crate::agent_task_gate::VerifyGateOptions,
    external_authorization: String,
    mode: ReplacementGateMode,
) -> Result<AgentTaskPromotionReport> {
    let mut gates = gates;
    // Recovery admits new commands, not a new execution placement. Reuse the
    // same durable placement default as fresh and continued Cook promotion.
    if let Some(recipe) = super::cook_recipe::load_recipe_for_attempt(run_id)? {
        super::cook_recipe::default_gate_lab_runner_from_attempt_dispatch(
            &mut gates,
            &recipe.promotion_transport["attempt_dispatch"],
        );
    }
    let gate_timeout = gates.gate_timeout();
    let admitted_component_id = gates.gate_environment.admitted_component_id.clone();
    let original = persisted_promotion_for_attempt(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion",
            "replacement gates require a persisted failed promotion",
            Some(run_id.to_string()),
            None,
        )
    })?;
    // A process can die after `record_replacement_gate_proof()` commits and
    // before its operation claim is completed. Recover that durable success
    // without repeating the gate execution that produced the proof.
    if original.provenance.get("replacement_gate_proof").is_some() {
        if original.finalization_eligible() {
            return Ok(original);
        }
        return Err(Error::validation_invalid_argument(
            "replacement_gate_proof",
            "completed replacement proof is not finalization-eligible; inspect the persisted evidence with agent-task status",
            Some(run_id.to_string()),
            None,
        ));
    }
    let regate = mode == ReplacementGateMode::Regate;
    let interrupted_rerun_authorization = match mode {
        ReplacementGateMode::Replacement {
            interrupted_rerun_authorization,
        } => {
            interrupted_rerun_authorization.filter(|authorization| !authorization.trim().is_empty())
        }
        ReplacementGateMode::Regate => None,
    };
    // A gate job the daemon admitted after its controller died is a started
    // gate even though no local fence was written (#18709).
    // A finished one is offered to the gates below: a gate whose request its
    // receipt is bound to adopts that verdict instead of running again (#18727).
    let adoptable_gate_jobs = reconcile_admitted_replacement_gate_jobs(lifecycle_store, run_id)?;
    let recovering_interrupted_execution =
        match replacement_gate_execution_owner_live(&lifecycle_store.read_record(run_id)?.metadata)
        {
            Some(true) => return Err(live_replacement_gate_execution_error(run_id)),
            Some(false) => true,
            None => false,
        };
    // A regate re-runs the deterministic gates of a candidate that already
    // exists and spends no provider budget, so the dead fence protects
    // nothing the operator's `--regate` did not just ask for (#18042, #18075).
    let superseded_fence = if recovering_interrupted_execution && regate {
        Some(supersede_interrupted_replacement_gate_fence(
            lifecycle_store,
            run_id,
        )?)
    } else {
        None
    };
    if recovering_interrupted_execution && !regate && interrupted_rerun_authorization.is_none() {
        return Err(interrupted_replacement_gate_execution_error(run_id));
    }
    if !original.status.applied_without_passing_verdict() {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.status",
            "replacement gates require an already-applied candidate whose verification failed, deferred or did not complete",
            Some(run_id.to_string()),
            None,
        ));
    }
    let has_reviewer_runnable_gate = gates
        .test_execution_plan
        .as_ref()
        .and_then(|plan| plan.declared_command().ok())
        .map(homeboy_engine_primitives::shell::quote_args)
        .into_iter()
        .chain(gates.verify.iter().cloned())
        .any(|command| crate::agent_task_review_dossier::reviewer_runnable_command(&command));
    if !has_reviewer_runnable_gate {
        let mut error = Error::validation_invalid_argument(
            "replacement_gate_proof",
            "replacement verification requires a reviewer-runnable visible gate",
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
        homeboy_core::worktree_provider::resolve_worktree_ownership_if_present(&original.to_worktree)
            .ok()
            .flatten()
            .map(|workspace| PathBuf::from(workspace.path))
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
    // The persisted candidate, not the mutable Cook selection, owns this
    // replay. A later retry may have an empty aggregate (or a different task).
    let (source, source_path) =
        persisted_promotion_source_in_store(lifecycle_store, &original, run_id)?;
    let observation_store = lifecycle_store.open_observation_initialized()?;
    let replacement_gate_workspace = replacement_component_workspace(&original, &target_path)?;
    let original_value = serde_json::to_value(&original)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    // Replacement gates run under the same declared supervision as Cook
    // promotion. Without this scope the gate runner falls back to its default
    // no-progress budget and silently drops the operator's declaration (#15856).
    // The verified-base differential replays below must run inside the same
    // scope: they reach the gate runner without explicit supervision and rely
    // on the active scope for the declared budgets on a Lab runner (#15856).
    let supervision = replacement_gate_supervision(&gates);
    let repro = gates.repro.clone();
    let job_submission: std::rc::Rc<dyn Fn(&str, &str, &Value) -> Result<()>> = {
        let lifecycle_store = lifecycle_store.clone();
        let run_id = run_id.to_string();
        std::rc::Rc::new(move |runner_id: &str, job_id: &str, submission: &Value| {
            mark_replacement_gate_job_submitted(
                &lifecycle_store,
                &run_id,
                runner_id,
                job_id,
                submission,
            )
        })
    };
    let mut gate_job_adoptions = Vec::new();
    let replacement = crate::agent_task_promotion::with_gate_supervision(supervision, || {
        // Each runner job is recorded as soon as its daemon accepts it, so
        // recovery reads the daemon's start record for it even if this
        // controller dies before it sees the job leave the queue (#18709).
        let (resumed, adoptions) = crate::agent_task_gate::placement::with_adoptable_gate_jobs(
            adoptable_gate_jobs,
            || {
                crate::agent_task_gate::execution_start::with_job_submission(job_submission, || {
                    resume_promoted_patch_replacement_gates_in_observation_store(
                        AgentTaskPromotionRequest {
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
                            artifact_id: None,
                            dry_run: false,
                            gates,
                            provider_invocation: None,
                            repository_integrity_evidence: original
                                .repository_integrity_evidence
                                .clone(),
                        },
                        &target_path,
                        &original_value,
                        replacement_gate_workspace.as_deref(),
                        &observation_store,
                        {
                            let lifecycle_store = lifecycle_store.clone();
                            let run_id = run_id.to_string();
                            move || {
                                mark_replacement_gate_execution_started(&lifecycle_store, &run_id)
                            }
                        },
                    )
                })
            },
        );
        gate_job_adoptions = adoptions;
        let mut replacement = resumed?;
        super::cook_baseline::compare_gate_failures_to_verified_base(
            &observation_store,
            &mut replacement,
            &repro,
            &target_path,
            replacement_gate_workspace
                .as_deref()
                .unwrap_or(&target_path),
            &verified_base.sha,
            gate_timeout,
            admitted_component_id.as_deref(),
            |_, _| Ok(()),
        )?;
        Ok(replacement)
    });
    record_replacement_gate_job_adoptions(lifecycle_store, run_id, &gate_job_adoptions)?;
    if let Some(stop) = foreground_gate_stop() {
        return Err(stop);
    }
    let mut replacement = replacement?;
    let adopted_gate_jobs = gate_job_adoptions
        .iter()
        .filter(|outcome| outcome["adopted"] == true)
        .cloned()
        .collect::<Vec<_>>();
    if !gate_job_adoptions.is_empty() {
        replacement.provenance["replacement_gate_job_adoption"] = serde_json::json!({
            "schema": "homeboy/agent-task-replacement-gate-job-adoption/v1",
            "outcomes": gate_job_adoptions,
        });
    }
    // #11290's import boundary requires command evidence for each accepted gate.
    // The shared gate runner retains that evidence in detailed gate reports, so
    // project it into the typed promotion command-evidence view before recording.
    replacement.command_evidence.extend(
        replacement
            .deterministic_gates
            .iter()
            .filter(|gate| {
                // Exit 0 alone is not proof: unexecuted gates also carry it.
                (gate.status == crate::agent_task_gate::AgentTaskGateStatus::Succeeded
                    && gate.exit_code == 0)
                    || gate.status
                        == crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
            })
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
    if let Some(fence) = superseded_fence {
        replacement.provenance["replacement_gate_execution_recovery"] = serde_json::json!({
            "schema": "homeboy/agent-task-replacement-gate-execution-recovery/v1",
            "superseded_by": "regate",
            "reason": REGATE_SUPERSEDES_FENCE_REASON,
            "superseded_fence": fence,
            "automatic_rerun": false,
            "adopted_gate_jobs": adopted_gate_jobs,
        });
    } else if recovering_interrupted_execution {
        let authorization = interrupted_rerun_authorization
            .expect("interrupted execution recovery requires explicit authorization");
        replacement.provenance["replacement_gate_execution_recovery"] = serde_json::json!({
            "schema": "homeboy/agent-task-replacement-gate-execution-recovery/v1",
            "operator_authorization": authorization,
            "automatic_rerun": false,
        });
    }
    if regate {
        replacement.repository_integrity_evidence = original.repository_integrity_evidence.clone();
        replacement.normalize_gate_outcome();
        if !replacement.finalization_eligible()
            && replacement.status.applied_without_passing_verdict()
        {
            return record_regate_gate_run(
                lifecycle_store,
                run_id,
                &original,
                replacement,
                &external_authorization,
            );
        }
    }
    record_replacement_gate_proof(run_id, replacement, Some(external_authorization))
}

const REGATE_SUPERSEDES_FENCE_REASON: &str = "recover --regate re-runs the deterministic gates of the already-produced candidate without provider work; the interrupted execution's owner is gone, so its start fence is superseded";

/// Lift a start fence whose owner process is gone so `recover --regate` can
/// run fresh gates, recording which fence was superseded and why.
fn supersede_interrupted_replacement_gate_fence(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Value> {
    let mut superseded = Value::Null;
    lifecycle_store.mutate_record(run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let Some(fence) = metadata
            .get_mut(REPLACEMENT_GATE_EXECUTION_FENCES_KEY)
            .and_then(Value::as_object_mut)
            .and_then(|fences| fences.remove("verify-replacement"))
        else {
            return false;
        };
        superseded = fence.clone();
        let supersessions = metadata
            .entry("replacement_gate_execution_fence_supersessions".to_string())
            .or_insert_with(|| serde_json::json!([]));
        if !supersessions.is_array() {
            *supersessions = serde_json::json!([]);
        }
        let supersessions = supersessions.as_array_mut().expect("array");
        supersessions.push(serde_json::json!({
            "schema": "homeboy/agent-task-replacement-gate-fence-supersession/v1",
            "superseded_by": "regate",
            "reason": REGATE_SUPERSEDES_FENCE_REASON,
            "fence": fence,
            "superseded_at": chrono::Utc::now().to_rfc3339(),
        }));
        let overflow = supersessions.len().saturating_sub(16);
        supersessions.drain(..overflow);
        true
    })?;
    Ok(superseded)
}

/// Persist a regate whose fresh gates ran but did not pass as the attempt's
/// latest promotion. The earlier promotion stays in the promotion history.
/// The report carries no replacement proof, so it stays a recoverable,
/// unverified candidate that a later regate or replacement may verify.
fn record_regate_gate_run(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    original: &AgentTaskPromotionReport,
    mut report: AgentTaskPromotionReport,
    authorization: &str,
) -> Result<AgentTaskPromotionReport> {
    let reviewer_command = |gate: &crate::agent_task_gate::AgentTaskGateReport| {
        if gate.visibility == crate::agent_task_gate::AgentTaskGateVisibility::Private {
            return "<private>".to_string();
        }
        gate.invocation()
            .map(|invocation| invocation.reviewer_command())
            .unwrap_or_else(|_| quote_args(&gate.command))
    };
    let gate_summary = |promotion: &AgentTaskPromotionReport| {
        promotion
            .deterministic_gates
            .iter()
            .map(|gate| {
                serde_json::json!({
                    "gate_id": gate.id,
                    "command": reviewer_command(gate),
                    "status": gate.status,
                    "lab_execution": gate
                        .lab_receipt
                        .as_ref()
                        .and_then(|receipt| receipt.get("execution_context"))
                        .cloned(),
                })
            })
            .collect::<Vec<_>>()
    };
    report.provenance["regate_gate_run"] = serde_json::json!({
        "schema": "homeboy/agent-task-regate-gate-run/v1",
        "operator_authorization": authorization,
        "verdict": report.status,
        "commands": report
            .deterministic_gates
            .iter()
            .map(reviewer_command)
            .collect::<Vec<_>>(),
        "gates": gate_summary(&report),
        "superseded_gate_run": {
            "status": original.status,
            "gates": gate_summary(original),
        },
        "recorded_at": chrono::Utc::now().to_rfc3339(),
    });
    lifecycle_store.record_promotion(
        run_id,
        serde_json::to_value(&report)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )?;
    Ok(report)
}

pub(crate) fn persisted_promotion_source_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    promotion: &AgentTaskPromotionReport,
    fallback_run_id: &str,
) -> Result<(String, Option<PathBuf>)> {
    // Promotions persisted before the owning attempt was recorded have no
    // source run; they can only belong to the run being resumed.
    let owner = promotion
        .source
        .run_id
        .as_deref()
        .unwrap_or(fallback_run_id);
    let (source, path) = promotion_source_in_store(lifecycle_store, owner)?;
    Ok((bind_persisted_promotion_artifact(source, promotion)?, path))
}

fn bind_persisted_promotion_artifact(
    source: String,
    promotion: &AgentTaskPromotionReport,
) -> Result<String> {
    let mut source_value: Value = serde_json::from_str(&source).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("deserialize replacement gate promotion source".to_string()),
        )
    })?;
    let outcome = if let Some(outcomes) = source_value
        .get_mut("outcomes")
        .and_then(Value::as_array_mut)
    {
        outcomes
            .iter_mut()
            .find(|outcome| {
                outcome.get("task_id").and_then(Value::as_str)
                    == Some(promotion.source.task_id.as_str())
            })
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion.source.task_id",
                    "replacement gate source aggregate has no outcome for the persisted promotion",
                    Some(promotion.source.task_id.clone()),
                    None,
                )
            })?
    } else {
        &mut source_value
    };
    let candidate = promotion
        .provenance
        .get("candidate_commit")
        .filter(|candidate| !candidate.is_null())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.candidate_commit",
                "non-recoverable: checkpoint has no immutable candidate; run a new Cook",
                None,
                None,
            )
            .with_retryable(false)
        })?;
    outcome["candidate"] = candidate.clone();
    serde_json::to_string(&source_value)
        .map_err(|error| Error::internal_json(error.to_string(), None))
}

/// A replacement gate execution still fenced: `Some(live)` says whether its
/// owner process runs. A finished or never-started execution is `None`.
pub(crate) fn replacement_gate_execution_owner_live(metadata: &Value) -> Option<bool> {
    let fence = metadata
        .get(REPLACEMENT_GATE_EXECUTION_FENCES_KEY)?
        .get("verify-replacement")?;
    Some(
        fence["owner_pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(homeboy_core::process::pid_is_running),
    )
}

/// Written immediately before the first replacement gate command executes.
pub(crate) fn mark_replacement_gate_execution_started(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<()> {
    lifecycle_store.mutate_record(run_id, |record| {
        let fences = record
            .ensure_metadata_object()
            .entry(REPLACEMENT_GATE_EXECUTION_FENCES_KEY.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !fences.is_object() {
            *fences = serde_json::json!({});
        }
        fences["verify-replacement"] = serde_json::json!({
            "schema": "homeboy/agent-task-replacement-gate-execution-fence/v1",
            "state": "started",
            "owner_pid": std::process::id(),
        });
        true
    })?;
    Ok(())
}

/// Lift this process's fence and submitted-job record once its gate
/// execution has returned.
fn finish_replacement_gate_execution(metadata: &mut Value) -> bool {
    let mut changed = false;
    for key in [
        REPLACEMENT_GATE_EXECUTION_FENCES_KEY,
        REPLACEMENT_GATE_EXECUTION_JOBS_KEY,
    ] {
        let owner = format!("/{key}/verify-replacement/owner_pid");
        if metadata.pointer(&owner) == Some(&Value::from(std::process::id())) {
            changed |= metadata[key]
                .as_object_mut()
                .is_some_and(|entries| entries.remove("verify-replacement").is_some());
        }
    }
    changed
}

const REPLACEMENT_GATE_EXECUTION_JOBS_KEY: &str = "replacement_gate_execution_jobs";

/// Record a runner job the replacement gates handed to a daemon, as soon as
/// the daemon accepted it and before it may leave the daemon's queue. The
/// daemon stamps the job's start itself; this job id is how recovery reads
/// that start after a controller that died before seeing it (#18709).
pub(crate) fn mark_replacement_gate_job_submitted(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    job_id: &str,
    submission: &Value,
) -> Result<()> {
    lifecycle_store.mutate_record(run_id, |record| {
        let entries = record
            .ensure_metadata_object()
            .entry(REPLACEMENT_GATE_EXECUTION_JOBS_KEY.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entries.is_object() {
            *entries = serde_json::json!({});
        }
        let owner_pid = std::process::id();
        let entry = &mut entries["verify-replacement"];
        if entry["owner_pid"] != Value::from(owner_pid) || !entry["jobs"].is_array() {
            *entry = serde_json::json!({
                "schema": "homeboy/agent-task-replacement-gate-execution-jobs/v1",
                "owner_pid": owner_pid,
                "jobs": [],
            });
        }
        entry["jobs"]
            .as_array_mut()
            .expect("array")
            .push(serde_json::json!({
                "runner_id": runner_id,
                "job_id": job_id,
                "submission": submission,
                "submitted_at": chrono::Utc::now().to_rfc3339(),
            }));
        true
    })?;
    Ok(())
}

/// A replacement gate job the daemon admitted out of its queue, submitted by
/// a controller that is gone. The daemon's start record stands in for the
/// start fence that controller never wrote (#18709).
#[derive(Debug, Clone)]
struct AdmittedGateJob {
    runner_id: String,
    job_id: String,
    owner_pid: Value,
    state: crate::agent_task_gate::placement::LabGateJobState,
    submission: Value,
}

impl AdmittedGateJob {
    fn running(&self) -> bool {
        !self.state.status.is_terminal()
    }

    /// A finished job that ran its gate to a verdict: recovery offers its
    /// receipt for adoption before running the gate again (#18727).
    fn adoptable(&self) -> bool {
        use homeboy_core::api_jobs::JobStatus;
        matches!(self.state.status, JobStatus::Succeeded | JobStatus::Failed)
    }

    fn submitted(&self) -> crate::agent_task_gate::placement::LabGateSubmittedJob {
        crate::agent_task_gate::placement::LabGateSubmittedJob {
            runner_id: self.runner_id.clone(),
            job_id: self.job_id.clone(),
            submission: self.submission.clone(),
        }
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "runner_id": self.runner_id,
            "job_id": self.job_id,
            "owner_pid": self.owner_pid,
            "status": self.state.status,
            "running": self.running(),
            "adoptable": self.adoptable(),
            "logs_command": format!(
                "homeboy runner job logs {} {}{}",
                self.runner_id,
                self.job_id,
                if self.running() { " --follow" } else { " --json" }
            ),
        })
    }
}

/// Submitted jobs whose controller is gone, with the daemon's record of
/// each: `None` when there is no such record. Jobs the daemon never admitted
/// (still queued, or reaped from the queue) are not returned: no gate ran.
fn dead_owner_admitted_gate_jobs(metadata: &Value) -> Option<Vec<AdmittedGateJob>> {
    let entry = metadata
        .get(REPLACEMENT_GATE_EXECUTION_JOBS_KEY)?
        .get("verify-replacement")?;
    let owner_pid = entry["owner_pid"].clone();
    let owner_live = owner_pid
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .is_some_and(homeboy_core::process::pid_is_running);
    if owner_live {
        return None;
    }
    let jobs = entry["jobs"].as_array()?;
    Some(
        jobs.iter()
            .filter_map(|job| {
                let runner_id = job["runner_id"].as_str()?;
                let job_id = job["job_id"].as_str()?;
                // An unreadable daemon cannot prove the gate started; the
                // regate then goes through that runner's own admission.
                let state = crate::agent_task_gate::placement::gate_job(runner_id, job_id)
                    .ok()
                    .flatten()?;
                state.started.then(|| AdmittedGateJob {
                    runner_id: runner_id.to_string(),
                    job_id: job_id.to_string(),
                    owner_pid: owner_pid.clone(),
                    state,
                    submission: job.get("submission").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

/// The replacement gate job the daemon admitted for `run_id` after its
/// controller died, preferring one still running (#18709). `recover` reports
/// it instead of offering a fresh regate as if no gate had run.
pub(crate) fn admitted_replacement_gate_job(run_id: &str) -> Option<Value> {
    let record = agent_task_lifecycle::exact_record(run_id).ok()?;
    let jobs = dead_owner_admitted_gate_jobs(&record.metadata)?;
    jobs.iter()
        .find(|job| job.running())
        .or_else(|| jobs.iter().find(|job| job.adoptable()))
        .or_else(|| jobs.first())
        .map(AdmittedGateJob::to_value)
}

/// Reconcile the local start fence with the daemon's start record of jobs a
/// dead controller submitted: an admitted job becomes the fence it never
/// wrote, and one still running vetoes a second execution. The submitted-job
/// record is dropped once none of its jobs still runs; its finished jobs are
/// returned so the gates can adopt their receipts (#18727).
fn reconcile_admitted_replacement_gate_jobs(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Vec<crate::agent_task_gate::placement::LabGateSubmittedJob>> {
    let record = lifecycle_store.read_record(run_id)?;
    let Some(jobs) = dead_owner_admitted_gate_jobs(&record.metadata) else {
        return Ok(Vec::new());
    };
    let running = jobs.iter().find(|job| job.running()).cloned();
    lifecycle_store.mutate_record(run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let fenced = metadata
            .get(REPLACEMENT_GATE_EXECUTION_FENCES_KEY)
            .and_then(|fences| fences.get("verify-replacement"))
            .is_some();
        let admitted = running.as_ref().or_else(|| jobs.first());
        if let Some(job) = admitted.filter(|_| !fenced) {
            let fences = metadata
                .entry(REPLACEMENT_GATE_EXECUTION_FENCES_KEY.to_string())
                .or_insert_with(|| serde_json::json!({}));
            if !fences.is_object() {
                *fences = serde_json::json!({});
            }
            fences["verify-replacement"] = serde_json::json!({
                "schema": "homeboy/agent-task-replacement-gate-execution-fence/v1",
                "state": "started",
                "owner_pid": job.owner_pid,
                "reconciled_from": "daemon_job_start",
                "runner_job": job.to_value(),
            });
        }
        if running.is_none() {
            if let Some(entries) = metadata
                .get_mut(REPLACEMENT_GATE_EXECUTION_JOBS_KEY)
                .and_then(Value::as_object_mut)
            {
                entries.remove("verify-replacement");
            }
        }
        true
    })?;
    match running {
        Some(job) => {
            let mut error = live_replacement_gate_execution_error(run_id);
            let logs = job.to_value()["logs_command"].clone();
            error.details["recovery"]["runner_job"] = job.to_value();
            if let Some(logs) = logs.as_str() {
                error = error.with_hint(format!("Wait for the admitted gate job: {logs}"));
            }
            Err(error)
        }
        None => Ok(jobs
            .iter()
            .filter(|job| job.adoptable())
            .map(AdmittedGateJob::submitted)
            .collect()),
    }
}

/// Record what recovery did with each finished gate job it offered for
/// adoption: adopted, or why the gate ran again instead (#18727).
fn record_replacement_gate_job_adoptions(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    outcomes: &[Value],
) -> Result<()> {
    if outcomes.is_empty() {
        return Ok(());
    }
    lifecycle_store.mutate_record(run_id, |record| {
        let entries = record
            .ensure_metadata_object()
            .entry("replacement_gate_job_adoptions".to_string())
            .or_insert_with(|| serde_json::json!([]));
        if !entries.is_array() {
            *entries = serde_json::json!([]);
        }
        let entries = entries.as_array_mut().expect("array");
        let recorded_at = chrono::Utc::now().to_rfc3339();
        entries.extend(outcomes.iter().map(|outcome| {
            let mut outcome = outcome.clone();
            outcome["recorded_at"] = Value::from(recorded_at.clone());
            outcome
        }));
        let overflow = entries.len().saturating_sub(16);
        entries.drain(..overflow);
        true
    })?;
    Ok(())
}

fn interrupted_replacement_gate_execution_error(run_id: &str) -> Error {
    let mut error = Error::validation_invalid_argument(
        "replacement_gate_proof",
        "replacement gate execution was interrupted after its durable start fence; Homeboy will not rerun shell gates automatically",
        Some(run_id.to_string()),
        Some(vec![
            format!(
                "Re-run the candidate's deterministic gates: homeboy agent-task recover {run_id} --regate"
            ),
            format!(
                "Run homeboy agent-task recover {run_id} --promotion @replacement.json --authorize-external-proof <authorization> with candidate-bound external proof."
            ),
        ]),
    );
    error.details["recovery"] = serde_json::json!({
        "kind": "external_candidate_bound_proof_required",
        "run_id": run_id,
        "regate_command": format!("homeboy agent-task recover {run_id} --regate"),
        "command": format!(
            "homeboy agent-task recover {run_id} --authorize-external-proof <proof-authorization> --authorize-interrupted-rerun <rerun-authorization> --verify '<candidate-bound command>'"
        ),
        "external_proof_command": format!(
            "homeboy agent-task recover {run_id} --promotion @replacement.json --authorize-external-proof <authorization>"
        ),
    });
    error
}

fn live_replacement_gate_execution_error(run_id: &str) -> Error {
    let mut error = Error::validation_invalid_argument(
        "replacement_gate_proof",
        "replacement gate execution still has a live owner; Homeboy will not rerun shell gates concurrently",
        Some(run_id.to_string()),
        Some(vec![format!("homeboy agent-task status {run_id}")]),
    );
    error.details["recovery"] = serde_json::json!({
        "kind": "replacement_gate_execution_live",
        "run_id": run_id,
        "status_command": format!("homeboy agent-task status {run_id}"),
    });
    error
}

/// Older applied reports lost the post-apply baseline when the final gate
/// report replaced its checkpoint. Recover it only from one controller-owned
/// checkpoint for this exact promotion; any conflicting or incomplete evidence
/// leaves the destination unavailable for dirty-worktree reuse.
pub(crate) fn attempt_needs_execution_with_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> bool {
    lifecycle_store
        .read_record(run_id)
        .map(|record| run_record_needs_execution(&record))
        .unwrap_or(true)
}

fn run_record_needs_execution(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    !matches!(
        record.state(),
        homeboy_lifecycle_contract::RunExecutionState::Succeeded
            | homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable
            | homeboy_lifecycle_contract::RunExecutionState::PartialFailure
            | homeboy_lifecycle_contract::RunExecutionState::Failed
            | homeboy_lifecycle_contract::RunExecutionState::Cancelled
    )
}

fn normalized_changed_files(files: &[String]) -> Vec<String> {
    let mut files = files.to_vec();
    files.sort();
    files.dedup();
    files
}

pub(crate) fn finalize_or_load_cook_pr_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
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
    options: &CookRequest,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    let store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    finalize_or_load_cook_pr_with_backend_with_stores(
        &store,
        &lifecycle_store,
        options,
        successful_run_id,
        promotion,
        backend,
    )
}

/// The reviewer-visible gap that requires draft publication.
/// Automatic finalization and `recover --preflight` read this one decision,
/// so preflight never refuses a candidate `recover` publishes as a draft.
fn draft_publication_gap(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
    supplied_form: bool,
    title_policy: crate::agent_task_review_dossier::PrTitlePolicy,
) -> Result<Option<String>> {
    if promotion.status == crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed {
        let failed = promotion
            .deterministic_gates
            .iter()
            .filter(|gate| gate.status == crate::agent_task_gate::AgentTaskGateStatus::Failed)
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>();
        return Ok(Some(format!(
            "Incomplete candidate; deterministic verification failed: {}",
            failed.join(", ")
        )));
    }
    // Eligible gates that include no pass are proven inherited failures: the
    // candidate is publishable, but only as a draft without verified commands.
    let unverified = crate::agent_task_review_dossier::verified_commands_from_promotion(promotion)
        .is_empty()
        && promotion.deterministic_gates.iter().any(|gate| {
            gate.status == crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
        });
    if crate::agent_task_review_dossier::verified_commands_from_promotion(promotion).is_empty()
        && !unverified
    {
        return Err(Error::validation_invalid_argument(
            "verification",
            "Cook cannot publish a test command without matching successful visible durable gate evidence bound to the promoted candidate",
            None,
            None,
        ));
    }
    let review_failure = if unverified || supplied_form {
        None
    } else {
        let aggregate = lifecycle_store.read_aggregate(run_id)?;
        match super::cook::review_form_from_aggregate(&aggregate) {
            Ok(Some(form)) => form
                .validate(title_policy)
                .err()
                .map(|error| error.to_string()),
            Ok(None) => Some(
                aggregate
                    .outcomes
                    .iter()
                    .flat_map(|outcome| &outcome.diagnostics)
                    .map(|diagnostic| diagnostic.class.clone())
                    .next()
                    .unwrap_or_else(|| "missing review_form output".to_string()),
            ),
            Err(error) => Some(error.to_string()),
        }
    };
    Ok(if unverified {
        Some("No deterministic gate passed on this candidate; every gate failure is inherited from the base".to_string())
    } else {
        review_failure.map(|reason| {
            format!(
                "Review form unavailable: {}; completion not assessed; deterministic gates passed",
                reason.replace(['\n', '\r'], " ")
            )
        })
    })
}

pub(crate) fn finalize_or_load_cook_pr_with_backend_with_stores<
    B: AgentTaskPrFinalizationBackend,
>(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    let mut promotion = promotion.clone();
    promotion.normalize_gate_outcome();
    let failed_draft = options.finalization.publish_incomplete_draft
        && promotion.status == crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed
        && !promotion.changed_files.is_empty();
    if !(promotion.finalization_eligible() || failed_draft) {
        return Err(Error::validation_invalid_argument(
            "promotion",
            format!(
                "agent-task cook finalization requires green gates or proven inherited baseline failures; inspect the blocking gates with `homeboy agent-task status {successful_run_id}`, then run `homeboy agent-task recover {successful_run_id}`"
            ),
            Some(successful_run_id.to_string()),
            Some(vec![
                format!("homeboy agent-task status {successful_run_id}"),
                format!("homeboy agent-task recover {successful_run_id}"),
            ]),
        ));
    }
    let finalization = cook_finalization_options_with_stores(
        store,
        lifecycle_store,
        options,
        successful_run_id,
        &promotion,
        Vec::new(),
        None,
    )?;
    publish_cook_finalization(
        lifecycle_store,
        successful_run_id,
        &promotion,
        finalization,
        backend,
        false,
    )
}

/// The one finalize-and-record tail shared by automatic Cook finalization and
/// `--recover`. Publication persists the promotion it publishes, runs the
/// engine, and records the receipt on the attempt's Cook delivery; a preflight
/// mutates nothing.
fn publish_cook_finalization<B: AgentTaskPrFinalizationBackend>(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
    finalization: AgentTaskPrFinalizationOptions,
    backend: &mut B,
    preflight: bool,
) -> Result<Value> {
    if preflight {
        return preflight_pr_with_backend(finalization, backend, lifecycle_store)
            .map(|report| serde_json::to_value(report).unwrap_or(Value::Null));
    }
    lifecycle_store.record_promotion(
        run_id,
        serde_json::to_value(promotion).unwrap_or(Value::Null),
    )?;
    let report = finalize_pr_with_backend(finalization, backend, lifecycle_store)?;
    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
    lifecycle_store.record_cook_finalization(run_id, value.clone())?;
    Ok(value)
}

#[cfg(test)]
pub(crate) fn cook_finalization_options(
    options: &CookRequest,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    overrides: Vec<AgentTaskReviewOverride>,
) -> Result<AgentTaskPrFinalizationOptions> {
    cook_finalization_options_with_stores(
        &super::cook_recipe::CookRecipeStore::from_current_data_root()?,
        &agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?,
        options,
        successful_run_id,
        promotion,
        overrides,
        None,
    )
}

pub(crate) fn cook_finalization_options_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    overrides: Vec<AgentTaskReviewOverride>,
    review_form: Option<&AgentTaskSuppliedReviewForm>,
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
        .workspace
        .source_refs
        .iter()
        .cloned()
        .chain(std::iter::once(format!(
            "homeboy://agent-task/run/{successful_run_id}"
        )))
        .collect();
    let artifact_refs = Vec::new();
    let verified_base = promotion
        .verified_base
        .as_ref()
        .filter(|base| base.base == options.finalization.base && !base.sha.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.verified_base",
                "cook finalization requires the typed declared base snapshot captured before promotion gates; rerun promotion against the configured base before finalizing",
                None,
                None,
            )
        })?;
    let mut review_dossier = cook_review_dossier_with_stores_and_review_form(
        store,
        lifecycle_store,
        options,
        promotion,
        successful_run_id,
        review_form,
    )?;
    review_dossier.overrides = overrides;
    review_dossier.source_relationships =
        cook_task_issue_relationships(&options.workspace.source_refs, Path::new(&path));
    let targeted_checks_run = review_dossier
        .verified_commands
        .iter()
        .map(|verified| verified.command.clone())
        .collect();
    let inherited_gates = promotion
        .deterministic_gates
        .iter()
        .filter(|gate| {
            gate.status == crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
        })
        .map(|gate| format!("{}: baseline-red (`{}`)", gate.id, gate.command.join(" ")))
        .collect::<Vec<_>>();
    let inherited_failure_count = inherited_gates.len();
    let attempt_summary = if promotion.status
        == crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed
    {
        format!(
            "{} deterministic cook gate attempt(s) completed; verification failed",
            promotion.deterministic_gates.len()
        )
    } else if inherited_failure_count == 0 {
        format!(
            "{} deterministic cook gate attempt(s) completed green",
            promotion.deterministic_gates.len()
        )
    } else {
        format!(
            "{} deterministic cook gate attempt(s) completed; {inherited_failure_count} inherited baseline-red failure(s) were reproduced on the immutable base and accepted: {}",
            promotion.deterministic_gates.len(),
            inherited_gates.join("; ")
        )
    };
    // A shared repository can register more than one component against the
    // same checkout; Cook resolves and durably records the exact component it
    // admitted the run against, so finalization must reuse that identity
    // rather than re-deriving it ambiguously from the bare worktree path
    // (#14725).
    let recorded_component_id =
        super::cook::cook_repository_identity_component_id(&options.identity.initial_plan);
    let selected_component_id = options
        .gates
        .gate_environment
        .admitted_component_id
        .as_deref()
        .or(recorded_component_id.as_deref());
    let review_profile = resolve_review_profile(selected_component_id, &path)?;
    let title_policy =
        crate::agent_task_review_dossier::PrTitlePolicy::from_plan(&options.identity.initial_plan);
    let gap = draft_publication_gap(
        lifecycle_store,
        successful_run_id,
        promotion,
        review_form.is_some(),
        title_policy,
    )?;
    // Passing regression gates is not proof that the task was completed.
    // Only a repro that failed on base and passed on this candidate can
    // authorize a new ready PR. Existing PR state is preserved by finalization.
    let draft_pr = options.finalization.draft_pr
        || gap.is_some()
        || review_dossier.repro
            != Some(crate::agent_task_review_dossier::AgentTaskReviewRepro::Verified);
    review_dossier.validate_for_publication(&review_profile, draft_pr)?;
    let form_title = if !options.finalization.title.is_empty() {
        None
    } else if gap.is_some() {
        let aggregate = lifecycle_store.read_aggregate(successful_run_id)?;
        let form = review_form.map(|form| form.form.clone()).or_else(|| {
            super::cook::review_form_from_aggregate(&aggregate)
                .ok()
                .flatten()
        });
        Some(super::cook::draft_title(
            None,
            form.as_ref().and_then(|form| form.specific_title()),
            tracker_publication_title(&options.identity.initial_plan).as_deref(),
            options.identity.initial_plan.metadata["cook_goal"].as_str(),
            title_policy,
        ))
    } else {
        Some(terminal_form_title(
            lifecycle_store,
            successful_run_id,
            review_form,
            title_policy,
        )?)
    };
    let pr_title = form_title
        .clone()
        .unwrap_or_else(|| options.finalization.title.clone());
    if !title_policy.accepts(&pr_title) {
        return Err(Error::validation_invalid_argument(
            "title",
            title_policy.violation(),
            Some(pr_title),
            None,
        ));
    }
    let commit_message = cook_commit_message(
        &options.finalization.commit_message,
        &pr_title,
        selected_component_id,
    );
    Ok(AgentTaskPrFinalizationOptions {
        path: path.clone(),
        component_id: selected_component_id.map(str::to_string),
        run_id: successful_run_id.to_string(),
        base: options.finalization.base.clone(),
        verified_base_sha: Some(verified_base.sha.clone()),
        head: options.finalization.head.clone(),
        title: pr_title,
        cook_form_title: form_title,
        commit_message,
        gate_results: Vec::new(),
        normalized_gate_results: promotion.gate_results.clone(),

        changed_files: promotion.changed_files.clone(),
        evidence: AgentTaskPrEvidence {
            source_refs,
            artifact_refs,
            attempt_summary,
            ai_tool: String::new(),
            // Runtime rotation can select a fallback after the immutable Cook
            // request was persisted. The dossier carries that terminal route.
            ai_model: Some(review_dossier.ai_assistance.model.clone()),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification {
                targeted_checks_run,
                dependency_hydration: Vec::new(),
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
        review_dossier,
        review_profile,
        manual_finalization: false,
        candidate: promotion.candidate.clone(),
        expected_candidate_tree: None,
        verified_candidate_tree: None,
        inherited_gate_evidence: None,
        protected_branches: options.finalization.protected_branches.clone(),
        draft_pr,
        gate_failed_draft: options.finalization.publish_incomplete_draft
            && promotion.status
                == crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed,
        repository_integrity_evidence: promotion.repository_integrity_evidence.clone(),
    })
}

/// Source relationships for the Cook PR: the task's source refs mapped against
/// the GitHub repository the PR is published to.
pub(super) fn cook_task_issue_relationships(
    source_refs: &[String],
    worktree: &Path,
) -> Vec<crate::agent_task_review_dossier::AgentTaskReviewIssueRelationship> {
    let destination =
        homeboy_core::git::publication_repository(None, &worktree.display().to_string()).ok();
    let mut relationships = Vec::new();
    for source_ref in source_refs {
        if let Some(relationship) = crate::agent_task_review_dossier::task_issue_relationship(
            source_ref,
            destination.as_ref(),
        ) {
            if !relationships.contains(&relationship) {
                relationships.push(relationship);
            }
        }
    }
    relationships
}

/// The subject a Cook candidate is committed under.
///
/// An explicit `--commit-message` always wins. Otherwise the commit carries the
/// PR title, which is the validated agent-authored title unless the operator
/// supplied one: a Cook PR has one commit, and GitHub squash-merges a
/// single-commit PR under that commit's subject, so a generic message here
/// becomes the permanent history entry (#16052). The generic subject remains
/// only for a Cook with no title at all.
pub(crate) fn cook_commit_message(
    explicit: &str,
    pr_title: &str,
    component_id: Option<&str>,
) -> String {
    if !explicit.trim().is_empty() {
        return explicit.to_string();
    }
    let title = pr_title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    format!("fix: cook {}", component_id.unwrap_or("agent task"))
}

fn terminal_form_title(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    supplied: Option<&AgentTaskSuppliedReviewForm>,
    title_policy: crate::agent_task_review_dossier::PrTitlePolicy,
) -> Result<String> {
    let form = match supplied {
        Some(supplied) => supplied.form.clone(),
        None => review_form_for_finalization_in_store(lifecycle_store, run_id, title_policy)?,
    };
    form.validate(title_policy)?;
    Ok(form.pr_title)
}

/// Recover publication from the durable Cook recipe and applied promotion.
pub fn recover_cook_pr(
    run_or_cook_id: &str,
    overrides: Vec<AgentTaskReviewOverride>,
    preflight: bool,
) -> Result<Value> {
    recover_cook_pr_with_review_form(run_or_cook_id, None, None, overrides, preflight)
}

/// Auditable authorship for a review form supplied outside provider execution.
/// The Cook controller owns the surrounding deterministic dossier, while this
/// records who authored the non-deterministic reviewer prose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskSuppliedReviewForm {
    pub form: crate::agent_task_review_dossier::AiFilledReviewForm,
    pub tool: String,
    pub model: String,
    pub operator: String,
}

impl AgentTaskSuppliedReviewForm {
    fn validate(
        &self,
        title_policy: crate::agent_task_review_dossier::PrTitlePolicy,
    ) -> Result<()> {
        self.form.validate(title_policy)?;
        self.form.ensure_publishable()?;
        for (field, value) in [
            ("review_form.tool", &self.tool),
            ("review_form.model", &self.model),
            ("review_form.operator", &self.operator),
        ] {
            if value.trim().is_empty() {
                return Err(Error::validation_invalid_argument(
                    field,
                    "supplied review form provenance must not be empty",
                    None,
                    None,
                ));
            }
        }
        if concrete_provider_model(Some(&self.model)).is_none() {
            return Err(Error::validation_invalid_argument(
                "review_form.model",
                "supplied review form provenance requires a concrete authoring model",
                None,
                None,
            ));
        }
        Ok(())
    }
}

/// Recover publication using an operator-supplied form only when the immutable
/// candidate's historical provider outcome did not retain one.
///
/// `component` disambiguates a shared-repository worktree only when the
/// durable Cook recipe recorded no usable component identity at admission
/// (e.g. a pre-existing durable record predating #14725); a recovered Cook
/// with a recorded identity reuses it and ignores this fallback.
pub fn recover_cook_pr_with_review_form(
    run_or_cook_id: &str,
    review_form: Option<AgentTaskSuppliedReviewForm>,
    component: Option<&str>,
    overrides: Vec<AgentTaskReviewOverride>,
    preflight: bool,
) -> Result<Value> {
    recover_cook_pr_with_backend_and_review_form(
        run_or_cook_id,
        review_form,
        component,
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
    recover_cook_pr_with_backend_and_review_form(
        run_or_cook_id,
        None,
        None,
        overrides,
        preflight,
        backend,
    )
}

pub(crate) fn recover_cook_pr_with_backend_and_review_form<B: AgentTaskPrFinalizationBackend>(
    run_or_cook_id: &str,
    review_form: Option<AgentTaskSuppliedReviewForm>,
    _component: Option<&str>,
    overrides: Vec<AgentTaskReviewOverride>,
    preflight: bool,
    backend: &mut B,
) -> Result<Value> {
    let (recipe, recovered_retry) = if super::cook_recipe::recipe_exists(run_or_cook_id)? {
        (super::cook_recipe::load_recipe(run_or_cook_id)?, false)
    } else if let Some(recipe) = super::cook_recipe::load_recipe_for_attempt(run_or_cook_id)? {
        (recipe, false)
    } else if let Some(recipe) = cook_recipe_for_retry_lineage(run_or_cook_id)? {
        (recipe, true)
    } else {
        return Err(Error::validation_invalid_argument(
            "run_or_cook_id",
            "recovery requires a durable Cook run",
            Some(run_or_cook_id.to_string()),
            None,
        ));
    };
    if let Some(form) = &review_form {
        form.validate(title_policy_for_recipe(&recipe, run_or_cook_id))?;
    }
    let completed = if recovered_retry {
        completed_finalization_receipt_for_run(run_or_cook_id)?
    } else {
        completed_finalization_receipt_for_recovery(&recipe, run_or_cook_id)?
    };
    if let Some(receipt) = completed {
        return Ok(receipt);
    }
    let run_id = if recovered_retry
        || recipe
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == run_or_cook_id)
    {
        run_or_cook_id.to_string()
    } else {
        review_form
            .as_ref()
            .and_then(|_| canonical_cook_candidate(&recipe.cook_id))
            .and_then(|candidate| candidate["run_id"].as_str().map(str::to_string))
            .or_else(|| canonical_cook_recovery_run_id(&recipe.cook_id))
            .ok_or_else(|| {
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
    let timing_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
        .and_then(|store| store.open_observation_initialized())
        .map_err(|error| {
            homeboy_core::log_status!("observe", "phase timing persistence failed: {}", error);
        })
        .ok();
    let _execution = timing_store.as_ref().map(|store| {
        store
            .phase_timer(&run_id)
            .into_guard("promote.recover.finalization")
    });
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
            "cook_delivery.artifacts.promotion",
            "recovery requires the attempt's persisted applied promotion",
            Some(run_id.clone()),
            None,
        )
    })?;
    let mut options = super::cook_recipe::reconstruct_options_without_dispatcher(&recipe)?;
    if recovered_retry {
        let verified_base = promotion.verified_base.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.verified_base",
                "detached Cook retry recovery requires the typed base snapshot verified by promotion",
                Some(run_id.clone()),
                None,
            )
        })?;
        options.finalization.base = verified_base.base.clone();
        options.finalization.head = promotion.target.branch.clone();
    }
    // Normalize historical status from the same authoritative eligibility rule
    // used by fresh Cook; recovery never executes gates to refresh this view.
    let recovery_outcome = promotion.gate_outcome();
    if !promotion.finalization_eligible()
        || !(recovery_outcome.status == AgentTaskPromotionStatus::Applied
            || (recovery_outcome.status == AgentTaskPromotionStatus::VerifiedNoChanges
                && !promotion.changed_files.is_empty()))
    {
        let next_command = format!("homeboy agent-task status {run_id}");
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.status",
            format!("persisted gates are not finalization-eligible; inspect the blocking gate evidence with `{next_command}`"),
            Some(run_id.clone()),
            Some(vec![next_command]),
        ));
    }
    // A supplied form replaces the agent's form whatever its state; the
    // agent's form is never consulted alongside it (#16441).
    let supplied_review_form = resolve_supplied_recovery_review_form(&run_id, review_form)?;
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let recipe_store = super::cook_recipe::CookRecipeStore::from_current_data_root()?;
    let finalization = cook_finalization_options_with_stores(
        &recipe_store,
        &lifecycle_store,
        &options,
        &run_id,
        &promotion,
        overrides,
        supplied_review_form.as_ref(),
    )?;
    if let (false, Some(submission)) = (preflight, &supplied_review_form) {
        // Record the exact validated submission before publication. A retry
        // reuses this immutable receipt rather than losing provenance after
        // an interrupted finalization.
        persist_supplied_recovery_review_form(&run_id, submission, &promotion, &options)?;
    }
    publish_cook_finalization(
        &lifecycle_store,
        &run_id,
        &promotion,
        finalization,
        backend,
        preflight,
    )
}

fn resolve_supplied_recovery_review_form(
    run_id: &str,
    supplied: Option<AgentTaskSuppliedReviewForm>,
) -> Result<Option<AgentTaskSuppliedReviewForm>> {
    let record = agent_task_lifecycle::reconcile_status(run_id)?;
    let persisted = record
        .metadata
        .get("recovery_review_form")
        .map(|value| {
            serde_json::from_value::<AgentTaskSuppliedReviewForm>(value["submission"].clone())
                .map_err(|_| {
                    Error::validation_invalid_argument(
                        "recovery_review_form",
                        "persisted supplied review form is malformed",
                        Some(run_id.to_string()),
                        None,
                    )
                })
        })
        .transpose()?;
    match (supplied, persisted) {
        (Some(submitted), Some(existing)) if submitted != existing => Err(Error::validation_invalid_argument(
            "review_form",
            "recovery already has a different supplied review form receipt; retry with the recorded submission",
            Some(run_id.to_string()),
            None,
        )),
        (Some(submitted), _) => Ok(Some(submitted)),
        (None, Some(existing)) => Ok(Some(existing)),
        (None, None) => Ok(None),
    }
}

fn persist_supplied_recovery_review_form(
    run_id: &str,
    submission: &AgentTaskSuppliedReviewForm,
    promotion: &AgentTaskPromotionReport,
    options: &CookRequest,
) -> Result<()> {
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    lifecycle_store.compare_and_set_metadata_value(
        run_id,
        "recovery_review_form",
        serde_json::json!({
            "schema": "homeboy/agent-task-recovery-review-form/v1",
            "submission": submission,
            "provenance": {
                "source": "operator_supplied_recovery",
                "run_id": run_id,
                "tree_sha": promotion.candidate.as_ref().map(|candidate| &candidate.tree_sha),
                "source_refs": options.workspace.source_refs,
                "deterministic_gates": promotion.deterministic_gates,
            },
        }),
    )
}

/// Resolve an ordinary durable retry back to the Cook attempt whose policy it
/// inherited. The retry remains the publication and gate-proof authority; the
/// ancestor recipe supplies only the immutable Cook finalization policy.
fn cook_recipe_for_retry_lineage(
    run_id: &str,
) -> Result<Option<super::cook_recipe::AgentTaskCookRecipe>> {
    const MAX_RETRY_LINEAGE_DEPTH: usize = 64;

    let mut current = run_id.to_string();
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_RETRY_LINEAGE_DEPTH {
        if !visited.insert(current.clone()) {
            return Err(Error::validation_invalid_argument(
                "retry_of",
                "durable retry lineage contains a cycle",
                Some(run_id.to_string()),
                None,
            ));
        }
        let record = match agent_task_lifecycle::exact_record(&current) {
            Ok(record) => record,
            Err(_) if current == run_id => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(parent) = record.metadata.get("retry_of").and_then(Value::as_str) else {
            return Ok(None);
        };
        if let Some(recipe) = super::cook_recipe::load_recipe_for_attempt(parent)? {
            return Ok(Some(recipe));
        }
        current = parent.to_string();
    }

    Err(Error::validation_invalid_argument(
        "retry_of",
        "durable retry lineage exceeds the supported depth",
        Some(run_id.to_string()),
        None,
    ))
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
    let options = super::cook_recipe::reconstruct_options_without_dispatcher(recipe)?;
    let _verified_base = promotion
        .verified_base
        .as_ref()
        .filter(|base| base.base == options.finalization.base && !base.sha.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.verified_base",
                "verified no-change recovery requires the Cook's declared immutable base snapshot",
                Some(run_id.to_string()),
                None,
            )
        })?;
    if !promotion.finalization_eligible() {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion",
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
            "cook_delivery.artifacts.promotion.source.task_id",
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
            "cook_delivery.artifacts.promotion.provenance.candidate",
            "verified no-change recovery requires a durable Git candidate fingerprint",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let AgentTaskPromotionCandidate::Git { .. } = &expected else {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.provenance.candidate",
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
                "cook_delivery.artifacts.promotion.provenance.worktree_path",
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
    agent_task_lifecycle::record_cook_finalization_in_store(
        &agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?,
        run_id,
        receipt.clone(),
    )?;
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
                "schema": "homeboy/agent-task-candidate-fingerprint/v2",
                "target_path": "/fixture",
                "head": head,
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
    canonical_cook_recovery_run_id_with_stores(None, cook_id)
}

pub(crate) fn canonical_cook_recovery_run_id_with_stores(
    stores: Option<(
        &super::cook_recipe::CookRecipeStore,
        &agent_task_lifecycle::AgentTaskLifecycleStore,
    )>,
    cook_id: &str,
) -> Option<String> {
    let candidate = stores
        .and_then(|(_, lifecycle)| canonical_cook_candidate_in_store(lifecycle, cook_id))
        .or_else(|| {
            stores
                .is_none()
                .then(|| canonical_cook_candidate(cook_id))
                .flatten()
        })
        .filter(|candidate| candidate["incomplete"] != true)?;
    let source_run_id = candidate["run_id"].as_str()?.to_string();
    if source_run_id.is_empty() {
        return None;
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
        recipe
            .attempts
            .iter()
            .rev()
            .map(|attempt| attempt.run_id.as_str())
            .collect()
    };
    for run_id in run_ids {
        if let Some(receipt) = completed_finalization_receipt_for_run(run_id)? {
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

fn completed_finalization_receipt_for_run(run_id: &str) -> Result<Option<Value>> {
    let record = agent_task_lifecycle::reconcile_status(run_id)?;
    let Some(value) = recorded_finalization_receipt(&record.metadata) else {
        return Ok(None);
    };
    if value["status"].as_str() == Some("intentional_no_change_finalized") {
        return Ok(Some(value.clone()));
    }
    if !cook_finalization_is_pr_receipt(value) {
        return Ok(None);
    }
    Ok(Some(value.clone()))
}

fn cook_review_dossier_with_stores_and_review_form(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    promotion: &AgentTaskPromotionReport,
    successful_run_id: &str,
    review_form: Option<&AgentTaskSuppliedReviewForm>,
) -> Result<AgentTaskReviewDossier> {
    // A form-only run owns reviewer metadata but carries forward the durable
    // gate proof while its authenticated source owns candidate scope.
    let terminal_promotion = promotion;
    let source_run_id = terminal_promotion
        .provenance
        .pointer("/cook_follow_up/source_run_id")
        .and_then(Value::as_str);
    // The source attempt's persisted promotion must exist; it owns the
    // candidate scope a form-only continuation carries forward.
    source_run_id
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
    // A form-only continuation carries its source's normalized gate proof.
    // The terminal record is therefore the durable reviewer-proof boundary.
    let verification_promotion = terminal_promotion;
    let gate_count = verification_promotion.gate_results.len();
    let title_policy =
        crate::agent_task_review_dossier::PrTitlePolicy::from_plan(&options.identity.initial_plan);
    // Every reviewer-visible verification line derives from a passed, visible
    // gate bound to the promoted candidate; without one there is nothing to show.
    let gap = draft_publication_gap(
        lifecycle_store,
        successful_run_id,
        promotion,
        review_form.is_some(),
        title_policy,
    )?;

    // A form-only follow-up owns reviewer metadata, not the candidate it carries
    // forward. Resolve the persisted Cook lineage so that follow-up prose cannot
    // erase the implementation attempt that produced the delivered patch.
    let terminal_form = match review_form {
        Some(form) => form.form.clone(),
        None if gap.is_some() => {
            let aggregate = lifecycle_store.read_aggregate(successful_run_id)?;
            super::cook::review_form_from_aggregate(&aggregate)
                .ok()
                .flatten()
                .unwrap_or(serde_json::from_value(json!({"summary": ""}))?)
        }
        None => {
            review_form_for_finalization_in_store(lifecycle_store, successful_run_id, title_policy)?
        }
    };
    let verified_commands = if gap.is_some() {
        crate::agent_task_review_dossier::verified_commands_from_promotion(verification_promotion)
    } else {
        terminal_form.verify_against_promotion(verification_promotion)?
    };
    let out_of_scope_changes = remediation_out_of_scope_changes(
        lifecycle_store,
        source_run_id.unwrap_or(successful_run_id),
        terminal_promotion,
    );
    let lineage = cook_ai_lineage_with_stores(
        store,
        lifecycle_store,
        options,
        successful_run_id,
        &terminal_form,
    )?;
    let repro = super::cook_repro::candidate_repro_verdict(
        &options.gates.repro,
        // The base repro result is recorded on the Cook before dispatch (#17653).
        super::cook_repro::recorded_repro_base_evidence(lifecycle_store, &options.identity.cook_id)
            .as_ref(),
        verification_promotion,
    );
    let mut evidence: Vec<AgentTaskReviewEvidence> = terminal_form
        .evidence
        .iter()
        .cloned()
        .map(AgentTaskReviewEvidence::for_reviewers)
        .collect();
    if let Some(form) = review_form {
        evidence.push(AgentTaskReviewEvidence {
            summary: format!(
                "Review form provenance: supplied by {} using {} ({}) rather than a recorded provider outcome.",
                form.operator, form.tool, form.model
            ),
            url: None,
        });
    }
    // Form-only continuations may substitute the implementation promotion for
    // candidate metadata; the persisted terminal record remains recovery proof.
    if let Some(replacement) = lifecycle_store
        .read_record(successful_run_id)
        .ok()
        .and_then(|record| {
            recorded_promotion_history(&record.metadata)
                .ok()
                .flatten()
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
                    "Recovery retained {original_gates} infrastructure-invalid gate record(s)."
                ),
                // The durable record is operator-only. Keep this reviewer-facing
                // provenance as text so `enrich_dossier` does not remove it.
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: format!("Replacement proof covers {gate_count} gate(s)."),
                url: None,
            },
            AgentTaskReviewEvidence {
                summary: "External proof authorized by operator.".to_string(),
                url: None,
            },
        ]);
    }
    let mut summary = if gap.is_some() {
        terminal_form.summary.clone()
    } else {
        lineage.summary.clone()
    };
    let mut what_changed = if gap.is_some() {
        terminal_form.what_changed.clone()
    } else {
        lineage.what_changed.clone()
    };
    if let Some(gap) = gap {
        if summary.trim().is_empty() {
            summary = gap;
        } else {
            summary.push_str(&format!(" {gap}"));
        }
        if what_changed.iter().all(|line| line.trim().is_empty()) {
            let framing = publication_framing(&options.identity.initial_plan);
            what_changed = vec![framing.clone()];
            if terminal_form.summary.trim().is_empty() {
                summary = format!("{framing}. {summary}");
            }
        }
    }
    // Use durable timestamps so re-rendering preserves the Cook wall time.
    // Missing records or invalid timestamps must never block finalization.
    let cook_seconds = (|| {
        let mut attempts = store.load_recipe(&options.identity.cook_id).ok()?.attempts;
        attempts.sort_by_key(|attempt| attempt.attempt);
        let first = lifecycle_store
            .read_record(&attempts.first()?.run_id)
            .ok()?;
        let successful = lifecycle_store.read_record(successful_run_id).ok()?;
        let start = chrono::DateTime::parse_from_rfc3339(&first.submitted_at).ok()?;
        let end = chrono::DateTime::parse_from_rfc3339(
            successful
                .updated_at
                .as_deref()
                .unwrap_or(&successful.submitted_at),
        )
        .ok()?;
        let span = end.signed_duration_since(start);
        if span < chrono::Duration::zero() {
            return None;
        }
        u64::try_from(span.num_seconds()).ok()
    })();
    Ok(AgentTaskReviewDossier {
        schema: "homeboy/agent-task-review-dossier/v1".to_string(),
        summary: if review_form.is_some() {
            terminal_form.summary.clone()
        } else {
            summary
        },
        what_changed: if review_form.is_some() {
            terminal_form.what_changed.clone()
        } else {
            what_changed
        },
        how_to_test: Vec::new(),
        // Deterministic evidence: orchestrator-owned. Each candidate-bound
        // gate renders once from `verified_commands`; `evidence` carries only
        // provenance that changes how a reviewer should read the result.
        evidence,
        verified_commands,
        failed_gate_output: promotion
            .deterministic_gates
            .iter()
            .filter(|gate| gate.status == crate::agent_task_gate::AgentTaskGateStatus::Failed)
            .map(|gate| {
                let output = crate::agent_task_review_dossier::plain_text(&format!(
                    "{}\n{}",
                    gate.stdout, gate.stderr
                ));
                let lines = output.lines().collect::<Vec<_>>();
                (
                    gate.id.clone(),
                    lines[lines.len().saturating_sub(40)..].join("\n"),
                )
            })
            .collect(),
        changed_public_contracts: Vec::new(),
        public_contract_evidence: None,
        verified_base: None,
        ai_assistance: AgentTaskReviewAiAssistance {
            // Deterministic: the orchestrator knows whether/what tool+model ran,
            // and attributes Homeboy as the harness that drove the change.
            used: true,
            tool: lineage.tool,
            model: lineage.model,
            cook_seconds,
            contribution: terminal_form.used_for.clone(),
        },
        source_relationships: Vec::new(),
        overrides: Vec::new(),
        out_of_scope_changes,
        repro: Some(repro),
        delivery: terminal_form.delivery.clone(),
    })
}

/// Files the candidate-producing remediation attempt changed outside its
/// dispatched `remediation_scope`: the original candidate's files plus the
/// conflict or gate-failure paths it was asked to fix (#17651). A first
/// attempt carries no scope and has nothing out of scope.
pub(super) fn remediation_out_of_scope_changes(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    candidate_run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Vec<String> {
    let Some(scope) = lifecycle_store
        .read_controller_plan(candidate_run_id)
        .ok()
        .and_then(|plan| plan.tasks.into_iter().next())
        .and_then(|task| {
            task.inputs
                .pointer("/cook_loop/remediation_scope")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<std::collections::BTreeSet<_>>()
                })
        })
    else {
        return Vec::new();
    };
    normalized_changed_files(&promotion.changed_files)
        .into_iter()
        .filter(|file| !scope.contains(file))
        .collect()
}

struct CookAiLineage {
    summary: String,
    what_changed: Vec<String>,
    tool: String,
    model: String,
}

struct CookAttemptExecution {
    tool: String,
    model: String,
}

struct CookContribution {
    execution: CookAttemptExecution,
}

struct CookContributionSource {
    run_id: String,
    task_id: String,
    patch_sha256: String,
}

fn concrete_provider_model(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .filter(|model| {
            !matches!(
                model.to_ascii_lowercase().as_str(),
                "not recorded" | "unknown" | "ai-assisted" | "ai assisted"
            )
        })
        .map(str::to_string)
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

fn cook_attempt_execution_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<CookAttemptExecution> {
    let plan = lifecycle_store.read_controller_plan(run_id)?;
    let record = lifecycle_store.read_record(run_id)?;
    // `candidate_adoption_recovery_outcome` only recognizes its own narrow set
    // of authenticated recovery markers (an expired preacceptance handoff with
    // no runner job ever recorded, or a specific pre-execution failure
    // phase). A historical source attempt can be a genuine never-executed
    // orphan through a path that helper does not cover — e.g. a lab handoff
    // that was dispatched (recording a runner job id) and only later expired
    // — while still having produced no outcome evidence at all. A single
    // field like `provider_executions_consumed == 0` is not a safe stand-in
    // by itself: plenty of legitimately-executed fixtures never bump that
    // counter. Require the same durable absence-of-evidence combination
    // `candidate_adoption_recovery_outcome`'s own expired-handoff branch
    // checks (minus its narrower runner-job-id requirement) before treating
    // this attempt as never having produced a real outcome, so an attempt
    // that genuinely executed keeps reading its authoritative outcome
    // (homeboy#15005).
    let never_executed = record.aggregate_path.is_none()
        && record.totals.is_none()
        && record.artifact_refs.is_empty()
        && record.provider_handles.is_empty()
        && record.latest_executor_evidence.is_none()
        && record.metadata["provider_executions_consumed"] == 0;
    let described_task = plan
        .tasks
        .iter()
        .find(|task| {
            agent_task_lifecycle::candidate_adoption_recovery_outcome(&record, task).is_some()
        })
        .or_else(|| never_executed.then(|| plan.tasks.first()).flatten());
    if let Some(task) = described_task {
        if task.executor.backend.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "provider_tool",
                "Cook lineage attempt has no dispatched provider tool",
                Some(run_id.to_string()),
                None,
            ));
        }
        // Never executed: disclose the dispatched route from the plan alone.
        let provenance = crate::execution_provenance::ExecutionProvenance {
            tool: task.executor.backend.clone(),
            route: task.executor.model().unwrap_or_default().to_string(),
            served_models: Vec::new(),
        };
        return Ok(CookAttemptExecution {
            model: crate::execution_provenance::render_execution_provenance(&provenance),
            tool: provenance.tool,
        });
    }
    let outcome = selected_outcome_for_attempt_in_store(lifecycle_store, run_id)?;
    if !plan
        .tasks
        .iter()
        .any(|task| task.task_id == outcome.task_id)
    {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "Cook lineage outcome does not match a task in its durable execution plan",
            Some(run_id.to_string()),
            None,
        ));
    }
    let provenance = crate::execution_provenance::execution_provenance(&record);
    Ok(CookAttemptExecution {
        model: crate::execution_provenance::render_execution_provenance(&provenance),
        tool: provenance.tool,
    })
}

fn cook_contribution_source(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<CookContributionSource>> {
    let plan = lifecycle_store.read_controller_plan(run_id)?;
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
    let provenance = &task.inputs["cook_loop"]["artifact_provenance"];
    // A follow-up of an attempt that produced no candidate records the source
    // run with a null tree: there is nothing to bind, so the chain ends here
    // and this attempt's own candidate is the contribution.
    if provenance.is_null() || provenance["source_candidate_tree_sha"].is_null() {
        return Ok(None);
    }
    let source = CookContributionSource {
        run_id: provenance["source_run_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        task_id: provenance["source_task_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        patch_sha256: provenance["source_candidate_tree_sha"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    };
    if source.run_id.is_empty() || source.task_id.is_empty() || source.patch_sha256.is_empty() {
        return Err(Error::validation_invalid_argument(
            "cook_loop.artifact_provenance",
            "Cook contribution source binding is incomplete",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(Some(source))
}

fn exact_promotion_for_contribution(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let record = lifecycle_store.read_record(run_id)?;
    let value = recorded_promotion_report(&record.metadata)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion",
            "Cook contribution source has no persisted promotion",
            Some(run_id.to_string()),
            None,
        )
    })?;
    let promotion: AgentTaskPromotionReport = serde_json::from_value(value).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion",
            format!("persisted Cook contribution promotion is invalid: {error}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    if promotion.source.run_id.as_deref() != Some(run_id) {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.source.run_id",
            "persisted Cook contribution promotion belongs to another attempt",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(promotion)
}

fn substantive_contribution_chain(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    attempts: &[super::AgentTaskCookRecipeAttempt],
    terminal_run_id: &str,
) -> Result<Vec<CookContribution>> {
    let positions = attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| (attempt.run_id.as_str(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut contributions = Vec::new();
    let mut current_run_id = terminal_run_id.to_string();
    // A Homeboy-verified no-change attempt still materializes the baseline its
    // follow-up runs in, so it is a provenance link, but it authored nothing
    // and is never disclosed as a contribution (#15923).
    let mut current_contributes = true;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current_run_id.clone()) {
            return Err(Error::validation_invalid_argument(
                "cook_loop.artifact_provenance",
                "Cook contribution lineage contains a cycle",
                Some(current_run_id),
                None,
            ));
        }
        let current_position =
            positions
                .get(current_run_id.as_str())
                .copied()
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "cook_recipe.attempts",
                        "Cook contribution is absent from the persisted recipe",
                        Some(current_run_id.clone()),
                        None,
                    )
                })?;
        if current_contributes {
            let execution = cook_attempt_execution_in_store(lifecycle_store, &current_run_id)?;
            contributions.push(CookContribution { execution });
        }
        let source = cook_contribution_source(lifecycle_store, &current_run_id)?;
        let Some(source) = source else {
            break;
        };
        let source_position = positions
            .get(source.run_id.as_str())
            .copied()
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "cook_loop.artifact_provenance.source_run_id",
                    "Cook contribution source is absent from the persisted recipe",
                    Some(source.run_id.clone()),
                    None,
                )
            })?;
        if source_position >= current_position {
            return Err(Error::validation_invalid_argument(
                "cook_loop.artifact_provenance.source_run_id",
                "Cook contribution source must precede its consumer",
                Some(source.run_id),
                None,
            ));
        }
        let source_promotion = exact_promotion_for_contribution(lifecycle_store, &source.run_id)?;
        let source_contributes = source_promotion.status.patch_promoted();
        if !(source_contributes
            || verified_no_change_link(lifecycle_store, &source.run_id, &source_promotion)?)
            || source_promotion.source.task_id.as_str() != source.task_id.as_str()
            || source_promotion
                .candidate
                .as_ref()
                .map(|candidate| candidate.tree_sha.as_str())
                != Some(source.patch_sha256.as_str())
        {
            return Err(Error::validation_invalid_argument(
                "cook_loop.artifact_provenance",
                "Cook contribution source binding does not match its persisted promotion",
                Some(source.run_id),
                None,
            ));
        }
        current_run_id = source.run_id;
        current_contributes = source_contributes;
    }
    contributions.reverse();
    Ok(contributions)
}

/// A source attempt Homeboy proved changed nothing: its persisted promotion
/// promoted no patch and its selected outcome carries Homeboy's unchanged
/// checkout proof. Its follow-up still binds to it because its promotion owns
/// the retry baseline, so the chain passes through it to its own source.
fn verified_no_change_link(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<bool> {
    if promotion.status != AgentTaskPromotionStatus::NoChanges
        || !promotion.changed_files.is_empty()
    {
        return Ok(false);
    }
    let outcome = selected_outcome_for_attempt_in_store(lifecycle_store, run_id)?;
    Ok(outcome.task_id == promotion.source.task_id && outcome.is_verified_unchanged_attempt())
}

fn substantive_lineage(
    contributions: Vec<CookContribution>,
    terminal_form: &crate::agent_task_review_dossier::AiFilledReviewForm,
) -> Result<CookAiLineage> {
    let Some(terminal) = contributions.last() else {
        return Err(Error::internal_unexpected(
            "substantive Cook lineage has no authenticated contribution",
        ));
    };
    if contributions.len() == 1 {
        let model = terminal.execution.model.clone();
        return Ok(CookAiLineage {
            summary: terminal_form.summary.clone(),
            what_changed: terminal_form.what_changed.clone(),
            tool: crate::agent_task_review_dossier::homeboy_tool_disclosure(
                &terminal.execution.tool,
            ),
            model,
        });
    }
    let mut tools = Vec::new();
    let mut models = Vec::new();
    for (index, contribution) in contributions.iter().enumerate() {
        let role = format!("Implementation {}", index + 1);
        let tool =
            crate::agent_task_review_dossier::homeboy_tool_disclosure(&contribution.execution.tool);
        let model = contribution.execution.model.clone();
        tools.push(format!("{role}: {tool}"));
        models.push(format!("{role}: {model}"));
    }
    Ok(CookAiLineage {
        summary: terminal_form.summary.clone(),
        what_changed: terminal_form.what_changed.clone(),
        tool: tools.join("; "),
        model: models.join("; "),
    })
}

fn cook_ai_lineage_with_stores(
    store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    options: &CookRequest,
    successful_run_id: &str,
    terminal_form: &crate::agent_task_review_dossier::AiFilledReviewForm,
) -> Result<CookAiLineage> {
    let recipe = store.load_recipe(&options.identity.cook_id)?;
    let mut attempts = recipe.attempts;
    attempts.sort_by_key(|attempt| attempt.attempt);
    let terminal_index = match attempts
        .iter()
        .position(|attempt| attempt.run_id == successful_run_id)
    {
        Some(index) => index,
        None if retry_descends_from_recipe(lifecycle_store, &attempts, successful_run_id)? => {
            let attempt = attempts
                .iter()
                .map(|attempt| attempt.attempt)
                .max()
                .unwrap_or_default()
                .saturating_add(1);
            attempts.push(super::cook_recipe::AgentTaskCookRecipeAttempt {
                attempt,
                run_id: successful_run_id.to_string(),
                plan: agent_task_lifecycle::load_plan_in_store(lifecycle_store, successful_run_id)?,
                // In-memory only: orders the lineage chain, never persisted.
                lineage: None,
            });
            attempts.len() - 1
        }
        None => {
            return Err(Error::validation_invalid_argument(
                "successful_run_id",
                "finalizing Cook run is absent from its persisted recipe lineage",
                Some(successful_run_id.to_string()),
                None,
            ));
        }
    };
    attempts.truncate(terminal_index + 1);
    substantive_lineage(
        substantive_contribution_chain(lifecycle_store, &attempts, successful_run_id)?,
        terminal_form,
    )
}

fn retry_descends_from_recipe(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    attempts: &[super::cook_recipe::AgentTaskCookRecipeAttempt],
    run_id: &str,
) -> Result<bool> {
    const MAX_RETRY_LINEAGE_DEPTH: usize = 64;

    let recipe_runs = attempts
        .iter()
        .map(|attempt| attempt.run_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut current = run_id.to_string();
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_RETRY_LINEAGE_DEPTH {
        if !visited.insert(current.clone()) {
            return Err(Error::validation_invalid_argument(
                "retry_of",
                "durable retry lineage contains a cycle",
                Some(run_id.to_string()),
                None,
            ));
        }
        // Both retries and remediations are lineage edges (#15567).
        let parent = match attempts.iter().find(|attempt| attempt.run_id == current) {
            Some(attempt) => {
                super::cook_lineage::recipe_attempt_lineage(attempt).map(|edge| edge.source_run_id)
            }
            None => {
                super::cook_lineage::lifecycle_retry_parent_in_store(lifecycle_store, &current)?
            }
        };
        let Some(parent) = parent else {
            return Ok(false);
        };
        if recipe_runs.contains(parent.as_str()) {
            return Ok(true);
        }
        current = parent;
    }
    Err(Error::validation_invalid_argument(
        "retry_of",
        "durable retry lineage exceeds the supported depth",
        Some(run_id.to_string()),
        None,
    ))
}

/// Load and validate the AI-authored review form for a finalizing run.
///
/// Resolve the human-facing subject from the selected review form or declared issue title.
pub fn candidate_publication_title(
    store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<String> {
    // The attempt's own plan declares its review-form title rule.
    let title_policy = agent_task_lifecycle::load_plan_in_store(store, run_id)
        .map(|plan| crate::agent_task_review_dossier::PrTitlePolicy::from_plan(&plan))
        .unwrap_or(crate::agent_task_review_dossier::PrTitlePolicy::ConventionalCommit);
    if let Ok(form) = review_form_for_finalization_in_store(store, run_id, title_policy) {
        return Ok(form.pr_title);
    }
    let recipe = super::load_recipe_for_attempt(run_id)?;
    let plan = agent_task_lifecycle::load_plan_in_store(store, run_id).ok();
    let recorded_form = store.read_aggregate(run_id).ok().and_then(|aggregate| {
        super::cook::review_form_from_aggregate(&aggregate)
            .ok()
            .flatten()
    });
    let operator = recipe
        .as_ref()
        .and_then(|recipe| recipe.finalization["title"].as_str())
        .filter(|title| !title.trim().is_empty());
    let agent = recorded_form
        .as_ref()
        .and_then(|form| form.specific_title());
    // Read the tracker only when the Cook recorded no title of its own.
    let tracker = (operator.is_none() && agent.is_none())
        .then(|| plan.as_ref().and_then(tracker_publication_title))
        .flatten();
    let goal = plan
        .as_ref()
        .and_then(|plan| plan.metadata["cook_goal"].as_str());
    // No placeholder: without a real title the candidate keeps its own
    // subject (#18129).
    if operator.is_none()
        && agent.is_none()
        && tracker.is_none()
        && goal.and_then(super::cook::goal_headline).is_none()
    {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "Cook has no publication title",
            None,
            None,
        ));
    }
    Ok(super::cook::draft_title(
        operator,
        agent,
        tracker.as_deref(),
        goal,
        title_policy,
    ))
}

/// Resolve tracker framing only when a task URL is available. Tracker outages
/// must not prevent publishing an otherwise authenticated candidate.
fn tracker_publication_title(plan: &crate::agent_task_scheduler::AgentTaskPlan) -> Option<String> {
    let url = plan
        .tasks
        .iter()
        .find_map(|task| task.workspace.task_url.as_deref())?;
    let output = std::process::Command::new("gh")
        .args(["issue", "view", url, "--json", "title"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    value["title"].as_str().map(str::to_owned)
}

fn publication_framing(plan: &crate::agent_task_scheduler::AgentTaskPlan) -> String {
    tracker_publication_title(plan)
        .or_else(|| {
            plan.metadata["cook_goal"]
                .as_str()
                .and_then(super::cook::goal_headline)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Deliver candidate changes".to_string())
}

/// The PR-title rule a durable Cook declared on the attempt's review form
/// (falling back to its initial attempt).
fn title_policy_for_recipe(
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
    run_id: &str,
) -> crate::agent_task_review_dossier::PrTitlePolicy {
    recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .or_else(|| recipe.attempts.first())
        .map(|attempt| crate::agent_task_review_dossier::PrTitlePolicy::from_plan(&attempt.plan))
        .unwrap_or(crate::agent_task_review_dossier::PrTitlePolicy::ConventionalCommit)
}

/// Ready publication requires a valid, publishable recorded form.
fn review_form_for_finalization_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    title_policy: crate::agent_task_review_dossier::PrTitlePolicy,
) -> Result<crate::agent_task_review_dossier::AiFilledReviewForm> {
    let outcome = selected_outcome_for_attempt_in_store(lifecycle_store, run_id)?;
    crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(&outcome.outputs)
        .and_then(|form| {
            let form = form.ok_or_else(|| {
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
            form.validate(title_policy)?;
            form.ensure_publishable()?;
            Ok(form)
        })
        .map_err(|mut error| {
            error.message = format!(
                "{} Supply a complete replacement with `homeboy agent-task recover {run_id} --review-form @FORM.json --review-form-tool <TOOL> --review-form-model <MODEL> --review-form-author <IDENTITY>`.",
                error.message
            );
            error
        })
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
    pub status: CookStatus,
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

/// The recipe and lifecycle stores that own one Cook.
#[derive(Clone, Copy)]
pub(crate) struct CookStores<'a> {
    pub recipe: &'a super::cook_recipe::CookRecipeStore,
    pub lifecycle: &'a agent_task_lifecycle::AgentTaskLifecycleStore,
}

impl CookStores<'_> {
    /// Every Cook report is built from the stores that own the Cook: there is
    /// no ambient report, so a report can never describe another installation.
    pub(crate) fn report(
        self,
        input: CookReportInput<'_>,
    ) -> AgentTaskRunResult<AgentTaskCookReport> {
        cook_report(self.recipe, self.lifecycle, input)
    }
}

pub(crate) fn cook_report(
    recipe_store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    input: CookReportInput<'_>,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let _finalization = homeboy_engine_primitives::phase_timing::PhaseTimer::current()
        .into_guard("mission.finalization");
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
    let ci_deferred = recipe_store
        .load_recipe(&cook_id)
        .ok()
        .is_some_and(|recipe| recipe.gate_policy["verify_ci"] == true);
    let awaiting_ci = ci_deferred
        && finalization
            .as_ref()
            .and_then(|value| value["pr_url"].as_str())
            .is_some();
    let status = if awaiting_ci {
        CookStatus::AwaitingCi
    } else {
        status
    };
    let disposition = if awaiting_ci {
        CookDisposition::InFlight
    } else {
        disposition
    };
    // Defense in depth, not a second source of truth: `disposition` remains
    // authoritative in release builds. This only catches an exit whose
    // declared disposition contradicts the status it reports, which is a
    // producer bug rather than a condition to recover from.
    debug_assert!(
        !status.is_in_flight() || disposition == CookDisposition::InFlight,
        "cook exit reported in-flight status {status:?} but declared {disposition:?}"
    );
    let history_run_ids = lifecycle_store
        .read_cook_index(&cook_id)
        .map(|index| {
            index
                .attempts
                .into_iter()
                .map(|attempt| attempt.run_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| {
            recipe_store
                .load_recipe(&cook_id)
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
            lifecycle_store
                .read_cook_index(&cook_id)
                .ok()
                .map(|index| index.latest_run_id)
        })
        .or_else(|| {
            // A recipe is persisted before its lifecycle record and Cook index. If
            // materialization fails in that window, its final immutable attempt is
            // still the only durable identity we can safely report.
            recipe_store
                .load_recipe(&cook_id)
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
    let selected_candidate =
        cook_selected_candidate_provenance(lifecycle_store, &cook_id, &invocation_run_ids);
    let failure_context = (exit_code != 0)
        .then(|| {
            cook_failure_context_with_stores(
                recipe_store,
                lifecycle_store,
                &cook_id,
                latest_run_id.as_deref(),
                status.as_str(),
            )
        })
        .flatten();
    let notes = provider_route_notes_for_run(lifecycle_store, latest_run_id.as_deref());
    let mut report = AgentTaskRunResult {
        value: AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id,
            latest_run_id,
            history_run_ids,
            invocation_run_ids,
            status,
            disposition,
            attempts,
            finalization,
            intentional_no_change: None,
            selected_candidate,
            stop_reason,
            terminal_phase: None,
            terminal_failure_classification: None,
            primary_failure: None,
            provider_route_notes: notes,
            failure_context,
            report_stores: Some((recipe_store.clone(), lifecycle_store.clone())),
        },
        exit_code,
    };
    if report.exit_code != 0 {
        if let Some(run_id) = report.value.latest_run_id.clone() {
            if let Ok(record) = lifecycle_store.read_record(&run_id) {
                super::cook::apply_resource_guard_stop(
                    &mut report,
                    &record.metadata,
                    &record.run_id,
                    record.runner_id(),
                );
            }
        }
    }
    report
}

/// Rebind reports returned through `CookService` to its authoritative stores.
/// Cook's legacy report constructors remain ambient entry points, but the public
/// runtime owns explicit stores and must replace every derived durable projection
/// before the result can be serialized or handed to a caller.
pub(crate) fn bind_report_to_stores(
    report: &mut AgentTaskCookReport,
    recipe_store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    failure_context_required: bool,
) {
    report.history_run_ids = lifecycle_store
        .read_cook_index(&report.cook_id)
        .map(|index| {
            index
                .attempts
                .into_iter()
                .map(|attempt| attempt.run_id)
                .collect()
        })
        .unwrap_or_else(|_| {
            recipe_store
                .load_recipe(&report.cook_id)
                .map(|recipe| {
                    recipe
                        .attempts
                        .into_iter()
                        .map(|attempt| attempt.run_id)
                        .collect()
                })
                .unwrap_or_default()
        });
    if report.latest_run_id.is_none() {
        report.latest_run_id = lifecycle_store
            .read_cook_index(&report.cook_id)
            .ok()
            .map(|index| index.latest_run_id)
            .or_else(|| {
                recipe_store
                    .load_recipe(&report.cook_id)
                    .ok()
                    .and_then(|recipe| recipe.attempts.last().map(|attempt| attempt.run_id.clone()))
            });
    }
    report.selected_candidate = cook_selected_candidate_provenance(
        lifecycle_store,
        &report.cook_id,
        &report.invocation_run_ids,
    );
    let enriched_failure_context = report.failure_context.take().filter(|context| {
        matches!(
            context.reason_code.as_str(),
            "provider_timeout" | "review_form_timeout" | "provider_rotation_exhausted"
        )
    });
    report.failure_context = failure_context_required
        .then(|| {
            cook_failure_context_with_stores(
                recipe_store,
                lifecycle_store,
                &report.cook_id,
                report.latest_run_id.as_deref(),
                report.status.as_str(),
            )
        })
        .flatten();
    if let (Some(context), Some(enriched)) =
        (report.failure_context.as_mut(), enriched_failure_context)
    {
        context.phase = enriched.phase;
        context.reason_code = enriched.reason_code;
        context.diagnostic = enriched.diagnostic;
        if matches!(
            context.reason_code.as_str(),
            "provider_timeout" | "review_form_timeout"
        ) {
            context.recovery_legal = enriched.recovery_legal;
            context.recovery_reason = enriched.recovery_reason;
            context.legal_actions = enriched.legal_actions;
            context.next_actions = enriched.next_actions;
            context.recovery_step = enriched.recovery_step;
        }
    }
    report.provider_route_notes =
        provider_route_notes_for_run(lifecycle_store, report.latest_run_id.as_deref());
    report.report_stores = Some((recipe_store.clone(), lifecycle_store.clone()));
}

/// One bounded operator-facing line per provider route the run skipped as
/// unready before dispatch, prefixed once with the route that actually ran
/// (#15881). The projection reads the durable run record and its admission
/// plan; without either, a run rendered no route notes.
fn provider_route_notes_for_run(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: Option<&str>,
) -> Vec<String> {
    let store = lifecycle_store;
    let Some(run_id) = run_id else {
        return Vec::new();
    };
    let Ok(record) = store.read_record(run_id) else {
        return Vec::new();
    };
    let Ok(plan) = agent_task_lifecycle::load_plan_in_store(store, run_id) else {
        return Vec::new();
    };
    let mut notes = crate::agent_task_route_notes::provider_route_notes(&record, Some(&plan));
    if let Some(evidence) = record.metadata.get("superseded_candidate") {
        notes.push(format!(
            "Superseded candidate of {}; preserved in Git stash {}.",
            evidence["superseded_run_id"].as_str().unwrap_or("unknown"),
            evidence["stash_oid"].as_str().unwrap_or("unknown"),
        ));
    }
    notes
}

/// `select_cook_candidate` deliberately selects across the whole Cook index
/// with no invocation scope — adopting the best candidate across attempts is
/// what a Cook is for. But a report can therefore say `latest_run_id: <this
/// run>` while `selected_candidate.run_id` names a prior session's run, and
/// `cook_failure_context` prefers that cross-invocation id when it builds
/// recovery commands. The orchestrator could not previously tell the two apart.
/// `invocation_scoped` states it explicitly. The selection itself is unchanged.
fn cook_selected_candidate_provenance(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    invocation_run_ids: &[String],
) -> Option<Value> {
    let selection = lifecycle_store.select_cook_candidate(cook_id).ok()?;
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
    let record = lifecycle_store.read_record(&selection.run_id).ok()?;
    if let Ok(Some(promotion)) = recorded_promotion_report(&record.metadata) {
        value["applied_promotion"] = serde_json::json!({
            "identity": promotion.pointer("/candidate/tree_sha"),
            "destination": promotion.get("to_worktree"),
            "fingerprint": promotion.pointer("/provenance/candidate"),
        });
    }
    Some(value)
}

/// Map a zero-budget aggregate diagnostic onto a pre-provider cause.
///
/// Harvest and other scheduler refusals fail before provider dispatch, so the
/// Cook report would otherwise fall through to `phase=provider` with no class.
pub(crate) fn pre_provider_diagnostic_cause<'a>(
    provider_executions_consumed: u64,
    diagnostics: impl IntoIterator<Item = &'a crate::agent_task::AgentTaskDiagnostic>,
) -> Option<(String, String, Value)> {
    if provider_executions_consumed > 0 {
        return None;
    }
    let diagnostic = diagnostics.into_iter().find(|item| {
        item.class.starts_with("agent_task.committed_harvest_")
            || item.data["pre_provider"] == Value::Bool(true)
    })?;
    let phase = if diagnostic
        .class
        .starts_with("agent_task.committed_harvest_")
    {
        "committed_harvest_preflight"
    } else {
        "pre_provider"
    };
    Some((
        phase.to_string(),
        diagnostic.class.clone(),
        json!({
            "class": diagnostic.class,
            "message": diagnostic.message,
            "data": diagnostic.data,
        }),
    ))
}

/// Build recovery coordinates from durable controller records only. Provider
/// failures contribute only a bounded, redacted causal command projection;
/// expanded output remains behind `diagnose`.
pub fn cook_failure_context(
    cook_id: &str,
    latest_run_id: Option<&str>,
    status: &str,
) -> Option<super::AgentTaskCookFailureContext> {
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().ok()?;
    let recipe_store = super::cook_recipe::CookRecipeStore::for_lifecycle_store(&lifecycle_store);
    cook_failure_context_with_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        latest_run_id,
        status,
    )
}

pub(crate) fn cook_failure_context_with_stores(
    recipe_store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    latest_run_id: Option<&str>,
    status: &str,
) -> Option<super::AgentTaskCookFailureContext> {
    cook_failure_context_with_probe_policy(
        recipe_store,
        lifecycle_store,
        cook_id,
        latest_run_id,
        status,
        true,
    )
}

pub(crate) fn cook_failure_context_with_probe_policy(
    recipe_store: &super::cook_recipe::CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    latest_run_id: Option<&str>,
    status: &str,
    live_probes: bool,
) -> Option<super::AgentTaskCookFailureContext> {
    let recipe = recipe_store.load_recipe(cook_id).ok()?;
    let chronological_latest_run_id = latest_run_id
        .map(str::to_string)
        .or_else(|| recipe.attempts.last().map(|attempt| attempt.run_id.clone()))?;
    let selection = lifecycle_store.select_cook_candidate(cook_id).ok();
    let selected_run_id = selection
        .as_ref()
        .filter(|selection| !selection.incomplete && !selection.run_id.is_empty())
        .map(|selection| selection.run_id.clone());
    // Candidate selection intentionally spans Cook history, but recovery is an
    // operation on this invocation. Its legality, phase, diagnostics, and every
    // emitted command must therefore come from this exact durable record.
    let record_run_id = chronological_latest_run_id.as_str();
    let record = lifecycle_store.read_record(record_run_id).ok();
    let provider_executions_consumed = recipe
        .attempts
        .iter()
        // Recipe entries are historical attempt identities. `status` resolves a
        // Cook ID alias to its latest attempt, which can count that later
        // provider execution again when an earlier preflight failure used the
        // Cook ID as its run ID.
        .filter_map(|attempt| lifecycle_store.read_record(&attempt.run_id).ok())
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
        .map(|record| format!("{:?}", record.state()))
        .unwrap_or_else(|| "recipe_persisted_without_lifecycle_record".to_string());
    let recovery_legal = record
        .as_ref()
        .is_some_and(|record| record.metadata.get("superseded_by_run_id").is_none());
    let promotion_claim = lifecycle_store
        .operation_claim(record_run_id, &format!("promote:{record_run_id}"))
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
    // Display only: an unreadable report is named by the controller diagnostic.
    let promotion = record
        .as_ref()
        .and_then(|record| recorded_promotion_report(&record.metadata).ok().flatten());
    let delivery = record
        .as_ref()
        .and_then(|record| super::CookDelivery::from_run_metadata(&record.metadata));
    let finalization_claim = delivery
        .as_ref()
        .and_then(|delivery| delivery.candidate.as_ref()?.candidate.as_ref())
        .map(|candidate| candidate.tree_sha.as_str())
        .and_then(|sha| {
            lifecycle_store
                .operation_claim(record_run_id, &format!("finalize:{record_run_id}:{sha}"))
                .ok()
                .flatten()
        })
        .or_else(|| {
            lifecycle_store
                .operation_claim(record_run_id, &format!("finalize:{record_run_id}"))
                .ok()
                .flatten()
        });
    let finalization_diagnostic = finalization_claim
        .as_ref()
        .filter(|claim| claim.state == agent_task_lifecycle::ClaimState::Failed)
        .and_then(|claim| claim.result.clone());
    let promotion_gate_failed = delivery.as_ref().is_some_and(|delivery| {
        delivery.blocker_code() == Some(super::CookDeliveryBlockerCode::GateFailed)
    });
    let cook_status = CookStatus::from_status(status).or_else(|| {
        record.as_ref().and_then(|record| {
            CookStatus::from_status(&agent_task_lifecycle::CookRunState::of(record).status)
        })
    });
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
    let live_reserve_failure = record
        .as_ref()
        .and_then(|record| record.metadata.get("pre_execution_failure"))
        .and_then(|failure| {
            if live_probes {
                reprobe_capacity_reserve_failure(failure)
            } else {
                (failure["error_code"] == "resource.capacity_reserve").then(|| failure.clone())
            }
        });
    let pre_execution_diagnostic = record.as_ref().and_then(|record| {
        let failure = record.metadata.get("pre_execution_failure")?;
        let details = failure.get("details")?;
        if failure.get("error_code").and_then(Value::as_str) == Some("resource.capacity_reserve") {
            return live_reserve_failure.as_ref().map(|failure| {
                serde_json::json!({
                    "code": "resource.capacity_reserve",
                    "message": failure.get("message"),
                    "details": failure.get("details"),
                })
            });
        }
        details
            .get("worktree_provider_failure")
            .cloned()
            .map(|evidence| {
                serde_json::json!({
                    "code": failure.get("error_code"),
                    "message": failure.get("message"),
                    "worktree_provider_failure": evidence,
                })
            })
    });
    let pre_provider_cause =
        lifecycle_store
            .read_aggregate(record_run_id)
            .ok()
            .and_then(|aggregate| {
                pre_provider_diagnostic_cause(
                    provider_executions_consumed,
                    aggregate
                        .outcomes
                        .iter()
                        .flat_map(|outcome| &outcome.diagnostics),
                )
            });
    let (phase, reason_code, diagnostic) = if status == "no_candidate" {
        (
            "candidate_selection".to_string(),
            "no_candidate".to_string(),
            Some(serde_json::json!({
                "class": "agent_task.no_candidate",
                "message": "verified intentional no-change evidence exists, but task policy requires a substantive candidate patch",
            })),
        )
    } else if blocking_claim.is_some() {
        (
            "promotion".to_string(),
            "operation_in_progress".to_string(),
            None,
        )
    } else if let Some(diagnostic) = controller_diagnostic.as_ref() {
        (
            "promotion".to_string(),
            diagnostic
                .pointer("/deepest_cause/code")
                .or_else(|| diagnostic.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("promotion_failure")
                .to_string(),
            Some(diagnostic.clone()),
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
        || cook_status.is_some_and(|status| status.is_gate_failure())
        || status == "deterministic_gate_failure"
    {
        let diagnostic = promotion
            .as_ref()
            .and_then(|promotion| gate_failure_diagnostic(promotion, record_run_id))
            .or_else(|| {
                Some(json!({
                    "class": "agent_task.promotion_gate_failed",
                    "message": "Deterministic promotion gate failed",
                }))
            });
        (
            "deterministic_gate".to_string(),
            "gate_failed".to_string(),
            diagnostic,
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
    } else if matches!(status, "review_form_timeout") {
        (
            "review_form".to_string(),
            "review_form_timeout".to_string(),
            None,
        )
    } else if let Some((phase, class, diagnostic)) = pre_provider_cause {
        (phase, class, Some(diagnostic))
    } else {
        (
            "provider".to_string(),
            lifecycle_state.to_ascii_lowercase(),
            None,
        )
    };
    let recovery_actions = capacity_reserve_recovery_actions(live_reserve_failure.as_ref())
        .or_else(|| {
            if !live_probes {
                return None;
            }
            dirty_candidate_adoption_recovery_actions(
                &recipe,
                record.as_ref(),
                cook_id,
                &chronological_latest_run_id,
                record
                    .as_ref()
                    .map(super::cook_recovery_command_prefix_for_record),
            )
        })
        .or_else(|| {
            if !live_probes {
                return None;
            }
            unfingerprinted_timeout_worktree_recovery(
                &recipe,
                record.as_ref(),
                &chronological_latest_run_id,
                diagnostic.as_ref(),
                lifecycle_store,
                record
                    .as_ref()
                    .map(super::cook_recovery_command_prefix_for_record),
            )
        })
        .unwrap_or_else(|| {
            let retry_refusal = record.as_ref().map(|record| {
                super::retry_admission_in_root(lifecycle_store, &record.run_id)
                    .err()
                    .map(|error| error.message)
            });
            let retry_admitted = matches!(retry_refusal, Some(None));
            let pending_operator_feedback =
                crate::agent_task_feedback::has_pending(lifecycle_store, cook_id);
            use agent_task_lifecycle::cook_run_state::cook_settlement_in_store;
            let recipes = super::cook_recipe::CookRecipeStore::for_lifecycle_store(lifecycle_store);
            let settlement = record.as_ref().and_then(|record| {
                cook_settlement_in_store(&recipes, cook_id, &record.run_id).ok()
            });
            let continuation_admitted = continuation_action_admitted(
                record
                    .as_ref()
                    .and_then(|record| record.metadata.get("cook_continuation_admission")),
                retry_admitted
                    || (cook_status.is_some_and(|status| status.is_gate_failure())
                        && settlement.as_ref().is_some_and(|s| s.pending())),
            );
            let involuntary_cancellation_retry_admitted = retry_admitted
                && record
                    .as_ref()
                    .is_some_and(cancelled_without_operator_request);
            let retry_refusal = retry_refusal.flatten();
            let delivered_candidate = delivered_candidate_facts(record.as_ref(), delivery.as_ref());
            // The adopt observation only matters for a terminal `no_changes`
            // attempt; skip the worktree inspection for every other state.
            let adoptable_candidate_ref = (live_probes && status == "no_changes")
                .then(|| no_changes_adoption_candidate(&recipe))
                .flatten();
            // The kernel driver lock is the one liveness fact: a live driver
            // owns every transition, so recovery has no step to take, which
            // is what diagnose reports for the same Cook (#17494).
            let admission = record.as_ref().and_then(|record| {
                super::lab_admission::queued_admission_in_store(record, lifecycle_store)
            });
            let live_driver = admission
                .as_ref()
                .map(|wait| wait.live_driver)
                .unwrap_or_else(|| {
                    super::cook_recipe::CookRecipeStore::for_lifecycle_store(lifecycle_store)
                        .foreign_cook_driver(cook_id)
                        .is_ok_and(|driver| driver.is_some())
                });
            // The previous `recover`'s no-progress marker is a planner input,
            // so every surface agrees with what `recover` performs (#17650).
            let no_progress = super::cook_recovery::matching_no_progress(
                lifecycle_store,
                cook_id,
                record.as_ref(),
            );
            let provider_outcome_missing = lifecycle_store
                .read_record_with_aggregate_bounded(record_run_id)
                .is_ok_and(|(_, aggregate)| {
                    aggregate.is_none_or(|aggregate| aggregate.outcomes.is_empty())
                });
            let continuation_refusal = record
                .as_ref()
                .and_then(|record| continuation_candidate_refusal(lifecycle_store, record));
            // Whether recovery may act is the record's run state, whatever
            // label the caller rendered: `status` and `recover` therefore
            // project one classification (#17976, #17384).
            cook_recovery_actions_with_prefix(
                status,
                record.as_ref().map(agent_task_lifecycle::CookRunState::of),
                &chronological_latest_run_id,
                lifecycle_store
                    .read_record(&chronological_latest_run_id)
                    .is_ok_and(|record| {
                        admission.as_ref().is_some_and(|wait| {
                            !wait.live_driver && wait.liveness.is_reconcilable()
                        }) || agent_task_lifecycle::disconnected_runner_recovery_available(&record)
                    }),
                live_driver,
                recovery_legal,
                blocking_claim.is_some(),
                pending_operator_feedback,
                if provider_outcome_missing || pending_operator_feedback {
                    retry_admitted
                } else {
                    continuation_admitted
                },
                retry_refusal.as_deref(),
                involuntary_cancellation_retry_admitted,
                exact_checkpoint_candidate_mismatch(&diagnostic),
                settlement.as_ref().is_some_and(|s| s.failed()),
                settlement.as_ref().is_some_and(|s| s.completed()),
                continuation_refusal.as_deref(),
                provider_outcome_missing,
                agent_incomplete_reason(lifecycle_store, record_run_id).as_deref(),
                adoptable_candidate_ref.as_deref(),
                delivered_candidate,
                no_progress.as_ref(),
                record.as_ref().and_then(lab_handoff_runtime_recovery),
                record
                    .as_ref()
                    .map(super::cook_recovery_command_prefix_for_record),
            )
        });
    let promotion_provenance = promotion;
    Some(super::AgentTaskCookFailureContext {
        cook_id: cook_id.to_string(),
        latest_run_id: chronological_latest_run_id,
        selected_run_id,
        selected_task_id: selection
            .as_ref()
            .and_then(|selection| selection.selected_task_id.clone()),
        selected_candidate_sha: selection
            .as_ref()
            .and_then(|selection| selection.selected_candidate_sha.clone()),
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
        recovery_step: Some(recovery_actions.step),
    })
}

/// Project the first failed controller-owned gate into the Cook-wide bounded
/// cause. Provider output is evidence of candidate production, not the cause
/// after promotion has reached deterministic verification.
fn gate_failure_diagnostic(promotion: &Value, run_id: &str) -> Option<Value> {
    let gate = promotion
        .get("deterministic_gates")
        .or_else(|| promotion.get("gate_results"))
        .and_then(Value::as_array)?
        .iter()
        .find(|gate| {
            matches!(
                gate.get("status").and_then(Value::as_str),
                Some("failed" | "failure")
            )
        })?;
    let evidence = gate.get("failure_evidence");
    let termination = gate
        .get("termination")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let exit_code = gate.get("exit_code").and_then(Value::as_i64).or_else(|| {
        evidence
            .and_then(|evidence| evidence.get("exit_code"))
            .and_then(Value::as_i64)
    });
    let message = if termination == "no_progress" {
        "Deterministic gate made no progress before its deadline; inspect the last progress marker and output tail, then split or increase the gate's progress cadence.".to_string()
    } else {
        evidence
            .and_then(|evidence| {
                evidence
                    .get("summary")
                    .or_else(|| evidence.get("agent_feedback"))
            })
            .and_then(Value::as_str)
            .or_else(|| gate.get("message").and_then(Value::as_str))
            .or_else(|| {
                evidence
                    .and_then(|evidence| evidence.get("stderr_tail"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("Deterministic promotion gate failed")
            .to_string()
    };
    let command = gate
        .get("command")
        .or_else(|| evidence.and_then(|evidence| evidence.get("command")));
    let output_tail = |field: &str| {
        evidence
            .and_then(|evidence| evidence.get(field))
            .and_then(Value::as_str)
            .map(|value| bounded_gate_detail(&homeboy_core::redaction::redact_string(value)))
    };
    let private_summary_only = gate.get("visibility").and_then(Value::as_str) == Some("private")
        && gate.get("reveal_policy").and_then(Value::as_str) != Some("full_evidence");
    let command = (!private_summary_only)
        .then(|| {
            command.map(|value| {
                bounded_gate_detail(&homeboy_core::redaction::redact_string(&value.to_string()))
            })
        })
        .flatten();
    let classification = evidence
        .and_then(|evidence| evidence.get("classification"))
        .and_then(Value::as_str);
    let infrastructure_failure = matches!(
        classification,
        Some("gate_declaration" | "execution_budget" | "zero_tests_selected" | "environment")
    );
    Some(json!({
        "class": if infrastructure_failure {
            "agent_task.gate_infrastructure_failed"
        } else {
            "agent_task.promotion_gate_failed"
        },
        "message": bounded_gate_detail(&homeboy_core::redaction::redact_string(&message)),
        "details": {
            "gate": gate.get("id").or_else(|| gate.get("name")),
            "classification": classification,
            "agent_feedback": if private_summary_only { None } else { output_tail("agent_feedback") },
            "command": command,
            "exit_code": exit_code,
            "termination": termination,
            "timed_out": termination == "timed_out" || termination == "no_progress",
            "elapsed_ms": gate.get("elapsed_ms"),
            "last_progress_ms_ago": gate.get("last_progress_ms_ago"),
            "stdout_tail": if private_summary_only { None } else { output_tail("stdout_tail") },
            "stderr_tail": if private_summary_only { None } else { output_tail("stderr_tail") },
            "candidate_checkout": gate.get("candidate_checkout"),
            "evidence_ref": format!("homeboy://agent-task/run/{run_id}/gates#gate={}",
                gate.get("id").and_then(Value::as_str).unwrap_or("unknown")),
        },
    }))
}

fn bounded_gate_detail(value: &str) -> String {
    const LIMIT: usize = 4096;
    if value.len() <= LIMIT {
        return value.to_string();
    }
    let mut boundary = LIMIT;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…[truncated]", &value[..boundary])
}

#[cfg(test)]
mod gate_failure_diagnostic_tests {
    use super::gate_failure_diagnostic;
    use serde_json::json;

    #[test]
    fn setup_gate_diagnostic_surfaces_digest_without_candidate_blame() {
        let promotion = json!({"deterministic_gates": [{
            "id": "gate-2", "status": "failed", "exit_code": 2,
            "failure_evidence": {
                "classification": "gate_declaration",
                "summary": "Homeboy gate setup failed",
                "stdout_tail": "dependency installation failed",
                "stderr_tail": "missing tool",
                "agent_feedback": "Repair the gate environment"
            }
        }]});
        let diagnostic = gate_failure_diagnostic(&promotion, "run").unwrap();
        assert_eq!(diagnostic["class"], "agent_task.gate_infrastructure_failed");
        assert_eq!(
            diagnostic["details"]["stdout_tail"],
            "dependency installation failed"
        );
        assert_eq!(
            diagnostic["details"]["agent_feedback"],
            "Repair the gate environment"
        );
    }

    #[test]
    fn failed_gate_diagnostic_is_bounded_actionable_and_candidate_bound() {
        let promotion = json!({
            "deterministic_gates": [{
                "id": "gate-1", "status": "failed", "command": ["sh", "-lc", "cargo test"],
                "exit_code": 125, "termination": "no_progress", "elapsed_ms": 12000,
                "last_progress_ms_ago": 11000,
                "candidate_checkout": {"commit":"abc", "tree":"tree"},
                "failure_evidence": {"command":"cargo test", "exit_code":125,
                    "stdout_tail":"last test output", "stderr_tail":"stalled"}
            }]
        });
        let diagnostic = gate_failure_diagnostic(&promotion, "cook-run").expect("failure detail");
        let details = &diagnostic["details"];
        assert_eq!(details["exit_code"], 125);
        assert_eq!(details["termination"], "no_progress");
        assert_eq!(details["timed_out"], true);
        assert_eq!(details["stdout_tail"], "last test output");
        assert_eq!(details["candidate_checkout"]["commit"], "abc");
        assert_eq!(
            details["evidence_ref"],
            "homeboy://agent-task/run/cook-run/gates#gate=gate-1"
        );
        assert!(diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("no progress"));
        assert!(!diagnostic.to_string().contains("provider transcript"));
    }
}

/// A cancelled attempt whose recorded provenance is anything but an operator's
/// `agent-task cancel` request.
fn cancelled_without_operator_request(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    record.state() == homeboy_lifecycle_contract::RunExecutionState::Cancelled
        && record
            .metadata
            .pointer("/cancellation_provenance/cause")
            .and_then(Value::as_str)
            .is_some_and(|cause| cause != "operator_requested")
}

fn continuation_action_admitted(admission: Option<&Value>, fallback: bool) -> bool {
    admission
        .map(|admission| admission["first_authoritative_denial"].is_null())
        .unwrap_or(fallback)
}

/// A persisted failure is normally not trusted to manufacture shell commands.
/// Reserve admission actions are core-owned, explicitly read-only, and retain
/// their structured identifier, so the terminal Cook notification can safely
/// forward the read-only inventory actions: the scoped worktree artifacts and
/// the shared Cargo targets that inventory cannot see
/// (Extra-Chill/homeboy#15474). The confirmation-gated apply action stays in
/// durable diagnostic evidence.
///
/// Re-measure the same filesystem and reserve that refused admission. Missing
/// destinations are normal before materialization; probe their nearest ancestor.
/// Keep unknown measurements blocked rather than treating them as cleared.
fn reprobe_capacity_reserve_failure(failure: &Value) -> Option<Value> {
    reprobe_capacity_reserve_failure_with(failure, |path| {
        let mut root = Path::new(path);
        while !root.exists() {
            root = root.parent()?;
        }
        homeboy_core::observation::disk_budget::disk_budget(
            root,
            "Cook recovery reserve admission",
            "filesystem capacity is not measurable",
        )
        .available_bytes
    })
}

fn reprobe_capacity_reserve_failure_with(
    failure: &Value,
    probe: impl FnOnce(&str) -> Option<u64>,
) -> Option<Value> {
    if failure.get("error_code").and_then(Value::as_str) != Some("resource.capacity_reserve") {
        return None;
    }
    let mut live = failure.clone();
    let filesystem = failure
        .pointer("/details/filesystem")
        .and_then(Value::as_str);
    let reserve = failure
        .pointer("/details/reserve_bytes")
        .and_then(Value::as_u64);
    let available = filesystem.and_then(probe);
    if let (Some(filesystem), Some(reserve), Some(available)) = (filesystem, reserve, available) {
        if available >= reserve {
            return None;
        }
        let error =
            homeboy_core::Error::capacity_reserve(homeboy_core::error::CapacityReserveDetails {
                filesystem: filesystem.to_string(),
                available_bytes: available,
                reserve_bytes: reserve,
                shortfall_bytes: reserve.saturating_sub(available),
            });
        live["message"] = json!(error.message);
        live["details"]["available_bytes"] = json!(available);
        live["details"]["shortfall_bytes"] = json!(reserve.saturating_sub(available));
    } else {
        live["message"] = json!("Filesystem reserve capacity could not be measured; inspect the filesystem before recovering.");
        live["details"]["available_bytes"] = Value::Null;
        live["details"]["shortfall_bytes"] = Value::Null;
    }
    Some(live)
}

fn capacity_reserve_recovery_actions(failure: Option<&Value>) -> Option<CookRecoveryActions> {
    let failure = failure?;
    let actions = failure
        .pointer("/details/_homeboy_actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|action| {
            action
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("capacity.reserve."))
                && action.get("program").and_then(Value::as_str) == Some("homeboy")
                && action.get("safety").and_then(Value::as_str) == Some("read_only")
        })
        .filter_map(|action| {
            let label = action.get("label")?.as_str()?;
            let args = action
                .get("args")?
                .as_array()?
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            Some(super::AgentTaskCookRecoveryAction {
                action: label.to_string(),
                command: format!("homeboy {}", quote_args(&args)),
            })
        })
        .collect::<Vec<_>>();
    Some(CookRecoveryActions {
        step: super::CookRecoveryStep::none(
            failure["message"]
                .as_str()
                .unwrap_or("Filesystem reserve capacity is not measurable."),
        ),
        reason: failure["message"]
            .as_str()
            .unwrap_or("Filesystem reserve capacity is not measurable.")
            .to_string(),
        legal_actions: actions.clone(),
        next_actions: actions,
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
    prefix: Option<String>,
) -> Option<CookRecoveryActions> {
    let record = record?;
    let task = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)?
        .plan
        .tasks
        .first()?;
    if record.metadata["provider_executions_consumed"]
        .as_u64()
        .unwrap_or_default()
        != 0
        || record.metadata["pre_execution_failure"]["details"]["dirty_candidate_adoption"]["reason"]
            != "first_provider_admission"
    {
        return None;
    }
    agent_task_lifecycle::candidate_adoption_recovery_outcome(record, task)?;
    let workspace = record.metadata["pre_execution_failure"]["details"]["dirty_candidate_adoption"]
        ["workspace"]
        .as_str()?;
    let options = super::cook_recipe::reconstruct_options_without_dispatcher(recipe).ok()?;
    let model = task.executor.model().unwrap_or("operator-finalized");
    let prefix = prefix.unwrap_or_else(|| super::cook_recovery_command_prefix(run_id));
    let actions = vec![
        super::AgentTaskCookRecoveryAction {
            action: "commit_candidate".to_string(),
            command: format!(
                "git -C {} add -A && git -C {} commit -m {}",
                quote_arg(workspace),
                quote_arg(workspace),
                quote_arg(&cook_commit_message(
                    &options.finalization.commit_message,
                    &options.finalization.title,
                    None,
                )),
            ),
        },
        super::AgentTaskCookRecoveryAction {
            action: "review_candidate".to_string(),
            command: format!("{prefix} agent-task review {}", quote_arg(run_id)),
        },
        super::AgentTaskCookRecoveryAction {
            action: "adopt_candidate".to_string(),
            command: format!(
                "{prefix} agent-task recover {} --candidate-ref HEAD --adoption-model {}",
                quote_arg(cook_id),
                quote_arg(model),
            ),
        },
    ];
    Some(CookRecoveryActions {
        step: super::CookRecoveryStep::none(
            "Commit the dirty candidate, review it, then adopt HEAD with `agent-task recover`.",
        ),
        reason: "The first provider admission refused a dirty checkout. Commit the immutable candidate, record its tracked review, then adopt HEAD through the durable Cook gates without provider patch replay.".to_string(),
        legal_actions: actions.clone(),
        next_actions: actions,
    })
}

/// Timeout left a real worktree candidate that could not be bound to its
/// producing run. Retry and recover --artifact-id are both ineligible
/// until an operator inspects that checkout.
fn unfingerprinted_timeout_worktree_recovery(
    recipe: &super::AgentTaskCookRecipe,
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    run_id: &str,
    diagnostic: Option<&Value>,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    prefix: Option<String>,
) -> Option<CookRecoveryActions> {
    let message = diagnostic.and_then(|diagnostic| {
        diagnostic
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| {
                diagnostic
                    .pointer("/deepest_cause/message")
                    .and_then(Value::as_str)
            })
    })?;
    if !message.contains("fingerprinted artifact") {
        return None;
    }
    let worktree = timeout_worktree_path(recipe, record, run_id, lifecycle_store)?;
    let prefix = prefix.unwrap_or_else(|| super::cook_recovery_command_prefix(run_id));
    let command = |args: &[&str]| super::cook_recovery_command_with_prefix(&prefix, args);
    let inspect = super::AgentTaskCookRecoveryAction {
        action: "inspect_worktree".to_string(),
        command: format!("git -C {} status", quote_arg(&worktree)),
    };
    Some(CookRecoveryActions {
        step: super::CookRecoveryStep::none(
            "Inspect the unfingerprinted timeout worktree; retry is not eligible.",
        ),
        reason: "Timeout left uncommitted worktree changes that could not be fingerprinted as the producing run's patch. Inspect the worktree; retry is not eligible.".to_string(),
        legal_actions: vec![
            super::AgentTaskCookRecoveryAction {
                action: "status".to_string(),
                command: command(&["status", run_id]),
            },
            super::AgentTaskCookRecoveryAction {
                action: "diagnose".to_string(),
                command: command(&["diagnose", run_id]),
            },
            inspect.clone(),
        ],
        next_actions: vec![inspect],
    })
}

fn timeout_worktree_path(
    recipe: &super::AgentTaskCookRecipe,
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    run_id: &str,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Option<String> {
    let from_artifact = lifecycle_store
        .read_aggregate(run_id)
        .ok()
        .and_then(|aggregate| {
            aggregate.outcomes.iter().find_map(|outcome| {
                outcome.artifacts.iter().find_map(|artifact| {
                    artifact
                        .metadata
                        .get("workspace_root")
                        .and_then(Value::as_str)
                        .filter(|root| !root.is_empty())
                        .map(str::to_string)
                })
            })
        });
    from_artifact
        .or_else(|| {
            recipe
                .attempts
                .iter()
                .find(|attempt| attempt.run_id == run_id)
                .and_then(|attempt| attempt.plan.tasks.first())
                .and_then(|task| task.workspace.root.clone())
                .filter(|root| !root.is_empty())
        })
        .or_else(|| {
            record
                .and_then(|record| record.metadata.get("source_checkout"))
                .and_then(|checkout| checkout.get("path").or_else(|| checkout.get("root")))
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            super::reconstruct_options(recipe)
                .ok()?
                .workspace
                .source_worktree_path
                .map(|path| path.display().to_string())
                .filter(|path| !path.is_empty())
        })
}

/// The `incomplete_reason` of the attempt's recorded review form when the agent
/// declared the task incomplete. Unreadable evidence yields `None`, leaving the
/// step to the remaining recovery facts.
fn agent_incomplete_reason(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Option<String> {
    let outcome = selected_outcome_for_attempt_in_store(lifecycle_store, run_id).ok()?;
    crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(&outcome.outputs)
        .ok()
        .flatten()?
        .incomplete_reason()
        .map(str::to_string)
}

/// [`agent_incomplete_reason`] for recovery steps outside this module.
pub(crate) fn attempt_agent_incomplete_reason(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Option<String> {
    agent_incomplete_reason(lifecycle_store, run_id)
}

/// Facts about the attempt's applied candidate that select the replacement
/// gate and publication steps.
#[derive(Debug, Clone, Copy, Default)]
struct DeliveredCandidateFacts {
    /// Applied candidate whose gates failed or never completed, without
    /// replacement proof.
    applied_gate_failure: bool,
    regate_ready: bool,
    /// Verified, unpublished candidate.
    verified_unpublished: bool,
}

/// HEAD of the Cook's destination worktree when the worktree's branch holds
/// commits ahead of the recorded immutable task base (#16181). A terminal
/// `no_changes` attempt in that state is adoptable: the operator's verified
/// commits are the candidate the provider could not produce. Only a Cook
/// sourced with `--cwd` records a source worktree path; every other shape,
/// an absent base, or an unreadable worktree keeps today's recovery. The
/// `adopt` step itself still validates cleanliness and lineage.
fn no_changes_adoption_candidate(recipe: &super::AgentTaskCookRecipe) -> Option<String> {
    let worktree = recipe
        .finalization
        .get("source_worktree_path")?
        .as_str()
        .map(std::path::PathBuf::from)?;
    if !worktree.is_dir() {
        return None;
    }
    let base = super::cook_attempt_workspace::recorded_cook_base(recipe)?;
    head_ahead_of_base(&worktree, &base)
}

/// HEAD of `worktree` when `base` is a strict ancestor of it. Ahead of base
/// means at least one commit between the two; unrelated histories and a HEAD
/// equal to base are not adoptable candidates.
pub(crate) fn head_ahead_of_base(worktree: &std::path::Path, base: &str) -> Option<String> {
    let head = git_stdout(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_string();
    git_success(worktree, &["merge-base", "--is-ancestor", base, &head])?;
    let ahead = git_stdout(
        worktree,
        &["rev-list", "--count", &format!("{base}..{head}")],
    )?;
    ahead
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|count| *count > 0)?;
    Some(head)
}

fn git_success(dir: &std::path::Path, args: &[&str]) -> Option<()> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .ok()
        .filter(|status| status.success())
        .map(|_| ())
}

fn git_stdout(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn delivered_candidate_facts(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    delivery: Option<&super::CookDelivery>,
) -> DeliveredCandidateFacts {
    let Some(record) = record else {
        return DeliveredCandidateFacts::default();
    };
    let Some(promotion) = persisted_promotion_from_record(&record.run_id, record)
        .ok()
        .flatten()
    else {
        return DeliveredCandidateFacts::default();
    };
    let applied_gate_failure = applied_candidate_needs_replacement_gates(&promotion);
    let verified_unpublished = promotion.applied_with_passing_verdict()
        && promotion
            .candidate
            .as_ref()
            .is_some_and(|candidate| !candidate.tree_sha.is_empty())
        && !delivery.is_some_and(super::CookDelivery::published);
    // An agent-declared incomplete form does not hide regate: an unjudged
    // candidate is re-gated first, and the review-form path stays open (#17802).
    let regate_ready = applied_gate_failure
        && promotion.finalization_blocking_gates().all(|gate| {
            matches!(
                gate.status,
                crate::agent_task_gate::AgentTaskGateStatus::Deferred
                    | crate::agent_task_gate::AgentTaskGateStatus::Unavailable
                    | crate::agent_task_gate::AgentTaskGateStatus::Skipped
            )
        });
    DeliveredCandidateFacts {
        applied_gate_failure,
        regate_ready,
        verified_unpublished,
    }
}

/// Whether the attempt's applied candidate still has no passing verdict: its
/// gates failed, deferred or never completed and no replacement proof made
/// them green. Publication refuses such a candidate, so nothing may select
/// publication ahead of the verification that clears it (#17494).
pub(crate) fn applied_candidate_awaits_verification(run_id: &str) -> bool {
    persisted_promotion_for_attempt(run_id)
        .ok()
        .flatten()
        .is_some_and(|promotion| applied_candidate_needs_replacement_gates(&promotion))
}

struct CookRecoveryActions {
    legal_actions: Vec<super::AgentTaskCookRecoveryAction>,
    next_actions: Vec<super::AgentTaskCookRecoveryAction>,
    reason: String,
    /// The one step `agent-task recover` performs for this state.
    step: super::CookRecoveryStep,
}

#[cfg(test)]
/// Standard Cook actions are built once. Lab runtime repair is legal only for
/// the exact failed admission record, so it is kept out of `next_actions`.
fn cook_recovery_actions(
    status: &str,
    run_id: &str,
    recovery_legal: bool,
    blocking_claim: bool,
    retry_admitted: bool,
    exact_checkpoint_candidate_mismatch: bool,
    lab_runtime_recovery: Option<agent_task_lifecycle::AgentTaskLabRuntimeRecovery>,
) -> CookRecoveryActions {
    let record = agent_task_lifecycle::exact_record(run_id).ok();
    cook_recovery_actions_with_prefix(
        status,
        record.as_ref().map(agent_task_lifecycle::CookRunState::of),
        run_id,
        record
            .as_ref()
            .is_some_and(agent_task_lifecycle::disconnected_runner_recovery_available),
        false,
        recovery_legal,
        blocking_claim,
        false,
        retry_admitted,
        None,
        false,
        exact_checkpoint_candidate_mismatch,
        false,
        false,
        None,
        false,
        None,
        None,
        DeliveredCandidateFacts::default(),
        None,
        lab_runtime_recovery,
        None,
    )
}

fn cook_recovery_actions_with_prefix(
    status: &str,
    run_state: Option<agent_task_lifecycle::CookRunState>,
    run_id: &str,
    disconnected_runner: bool,
    live_driver: bool,
    recovery_legal: bool,
    blocking_claim: bool,
    pending_operator_feedback: bool,
    retry_admitted: bool,
    retry_refusal: Option<&str>,
    involuntary_cancellation_retry_admitted: bool,
    exact_checkpoint_candidate_mismatch: bool,
    continuation_rearm_required: bool,
    continuation_completed: bool,
    continuation_refusal: Option<&str>,
    provider_outcome_missing: bool,
    agent_incomplete_reason: Option<&str>,
    adoptable_candidate_ref: Option<&str>,
    delivered_candidate: DeliveredCandidateFacts,
    no_progress: Option<&super::cook_recovery::CookRecoveryNoProgress>,
    lab_runtime_recovery: Option<agent_task_lifecycle::AgentTaskLabRuntimeRecovery>,
    prefix: Option<String>,
) -> CookRecoveryActions {
    // The mutating action is never classified here: it is the one recovery
    // step `agent-task recover` performs, rendered as that command.
    let step =
        super::cook_recovery::cook_recovery_step(&super::cook_recovery::CookRecoveryInputs {
            status,
            run_state,
            recovery_legal,
            disconnected_runner,
            live_driver,
            blocking_claim,
            pending_operator_feedback,
            retry_admitted,
            retry_refusal,
            involuntary_cancellation_retry_admitted,
            exact_checkpoint_candidate_mismatch,
            continuation_rearm_required,
            continuation_completed,
            continuation_refusal,
            provider_outcome_missing,
            run_id,
            agent_incomplete_reason,
            adoptable_candidate_ref,
            applied_gate_failure: delivered_candidate.applied_gate_failure,
            regate_ready: delivered_candidate.regate_ready,
            verified_unpublished: delivered_candidate.verified_unpublished,
            no_progress,
        });
    if !recovery_legal {
        let reason = match &step {
            super::CookRecoveryStep::None { reason } => reason.clone(),
            _ => unreachable!("an illegal recovery has no step"),
        };
        return CookRecoveryActions {
            legal_actions: Vec::new(),
            next_actions: Vec::new(),
            reason,
            step,
        };
    }

    let prefix = prefix.unwrap_or_else(|| super::cook_recovery_command_prefix(run_id));
    let command = |args: &[&str]| super::cook_recovery_command_with_prefix(&prefix, args);
    let mut actions = vec![
        super::AgentTaskCookRecoveryAction {
            action: "status".to_string(),
            command: command(&["status", run_id]),
        },
        super::AgentTaskCookRecoveryAction {
            action: "diagnose".to_string(),
            command: command(&["diagnose", run_id]),
        },
    ];
    actions.extend(step.recovery_actions(&prefix, run_id));
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
    let reason = format!("Legal recovery actions for this Cook state: {names}.");
    CookRecoveryActions {
        reason,
        legal_actions: actions,
        next_actions,
        step,
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
    fn reserve_recovery_uses_live_capacity() {
        let recorded = json!({
            "error_code": "resource.capacity_reserve",
            "message": "recorded shortfall",
            "details": {
                "filesystem": "/recorded/filesystem",
                "available_bytes": 90,
                "reserve_bytes": 100,
                "shortfall_bytes": 10
            }
        });
        for available in [100, 150] {
            let live = reprobe_capacity_reserve_failure_with(&recorded, |path| {
                assert_eq!(path, "/recorded/filesystem");
                Some(available)
            });
            assert!(capacity_reserve_recovery_actions(live.as_ref()).is_none());
        }
        let live = reprobe_capacity_reserve_failure_with(&recorded, |_| Some(95)).unwrap();
        assert_eq!(live["details"]["available_bytes"], 95);
        assert_eq!(live["details"]["shortfall_bytes"], 5);
        let actions = capacity_reserve_recovery_actions(Some(&live)).unwrap();
        assert!(actions.reason.contains("95 bytes available"));
        assert!(actions.reason.contains("5 bytes short"));
        let unknown = reprobe_capacity_reserve_failure_with(&recorded, |_| None).unwrap();
        assert!(unknown["details"]["available_bytes"].is_null());
        assert!(capacity_reserve_recovery_actions(Some(&unknown)).is_some());
        assert_eq!(recorded["details"]["available_bytes"], 90);
    }

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
            ("cancelled", true, false, vec!["status", "diagnose"]),
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
            ("review_ready", false, false, vec!["status", "diagnose"]),
        ];

        for (status, retry_available, exact_checkpoint_candidate_mismatch, expected) in cases {
            let recovery = cook_recovery_actions(
                status,
                "cook-state-matrix-attempt-1",
                true,
                false,
                retry_available,
                exact_checkpoint_candidate_mismatch,
                None,
            );
            let actions = recovery
                .legal_actions
                .iter()
                .map(|action| action.action.as_str())
                .collect::<Vec<_>>();
            assert_eq!(actions, expected, "{status}");
            assert_eq!(
                recovery.legal_actions[0].command,
                "homeboy agent-task status cook-state-matrix-attempt-1",
                "status recovery must contain only supported CLI arguments"
            );
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
            }));
            // The one mutating action is always the `recover` entry point, and
            // it is offered exactly when the step decision names a step.
            assert_eq!(
                recovery.legal_actions.iter().any(|action| action.command
                    == "homeboy agent-task recover cook-state-matrix-attempt-1"),
                !recovery.step.is_none(),
                "{status} must advertise recover exactly when it can advance"
            );
            assert_eq!(
                recovery.step.action_name().is_some(),
                expected.len() > 2,
                "{status} step and legal actions must agree"
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
                    "homeboy agent-task status checkpoint-mismatch-attempt-1".to_string()
                ),
                (
                    "diagnose".to_string(),
                    "homeboy agent-task diagnose checkpoint-mismatch-attempt-1".to_string()
                ),
                (
                    "fork_replacement".to_string(),
                    "homeboy agent-task recover checkpoint-mismatch-attempt-1".to_string()
                ),
            ]
        );
        assert_eq!(
            actions(&recovery.next_actions),
            actions(&recovery.legal_actions)
        );
    }

    #[test]
    fn recovery_actions_match_continuation_admission_for_failure_states() {
        let actions = |status, admitted| {
            cook_recovery_actions(
                status,
                "recovery-state-attempt-1",
                true,
                false,
                admitted,
                false,
                None,
            )
            .legal_actions
            .into_iter()
            .map(|action| action.action)
            .collect::<Vec<_>>()
        };

        assert_eq!(actions("gate_failed", false), vec!["status", "diagnose"]);
        assert!(actions("gate_failed", true).contains(&"resume".to_string()));
        assert!(actions("durable_failure", true).contains(&"resume".to_string()));
        assert!((cook_recovery_actions(
            "gate_failed",
            "recovery-state-attempt-1",
            false,
            false,
            true,
            false,
            None,
        )
        .legal_actions)
            .iter()
            .all(|action| action.action != "resume"));
    }

    #[test]
    fn denied_continuation_admission_withholds_resume_even_when_retry_is_admitted() {
        let admission = serde_json::json!({
            "schema": "homeboy/cook-continuation-admission/v1",
            "first_authoritative_denial": "live_owner_in_progress"
        });

        assert!(!continuation_action_admitted(Some(&admission), true));
        assert!(continuation_action_admitted(
            Some(&serde_json::json!({
                "schema": "homeboy/cook-continuation-admission/v1",
                "first_authoritative_denial": null
            })),
            false
        ));
        assert!(continuation_action_admitted(None, true));
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
    fn patch_evidence_does_not_select_a_recovery_candidate() {
        let recovery = cook_recovery_actions(
            "promotion_failed",
            "ambiguous-attempt-1",
            true,
            false,
            false,
            false,
            None,
        );

        assert_eq!(
            recovery
                .legal_actions
                .iter()
                .map(|action| action.command.as_str())
                .collect::<Vec<_>>(),
            vec![
                "homeboy agent-task status ambiguous-attempt-1",
                "homeboy agent-task diagnose ambiguous-attempt-1",
                "homeboy agent-task recover ambiguous-attempt-1",
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
            None,
        );
        assert_eq!(
            recovery
                .legal_actions
                .last()
                .map(|action| action.command.as_str()),
            Some("homeboy agent-task recover ambiguous-attempt-1")
        );
        assert!(!recovery
            .legal_actions
            .iter()
            .any(|action| action.command.contains("mime-shaped")));
    }
}

#[cfg(test)]
mod pre_provider_cause_tests {
    use super::*;
    use crate::agent_task::AgentTaskDiagnostic;

    #[test]
    fn harvest_dirty_workspace_is_a_shared_pre_provider_cause() {
        let diagnostic = AgentTaskDiagnostic {
            class: "agent_task.committed_harvest_dirty_workspace".to_string(),
            message: "refusing committed-change harvest from a workspace with pre-existing uncommitted changes"
                .to_string(),
            data: json!({ "status": " M user-edit.txt" }),
        };

        let (phase, class, value) =
            pre_provider_diagnostic_cause(0, std::iter::once(&diagnostic)).expect("cause");

        assert_eq!(phase, "committed_harvest_preflight");
        assert_eq!(class, "agent_task.committed_harvest_dirty_workspace");
        assert_eq!(value["message"], diagnostic.message);
        assert!(pre_provider_diagnostic_cause(1, std::iter::once(&diagnostic)).is_none());
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

#[cfg(test)]
mod replacement_gate_supervision_tests {
    use super::replacement_gate_supervision;
    use std::time::Duration;

    /// Regression for #15856: the declared no-progress and wall-clock budgets
    /// reach replacement gates instead of the gate runner's 300s default.
    #[test]
    fn replacement_gates_use_declared_budgets() {
        let gates = crate::agent_task_gate::VerifyGateOptions {
            gate_timeout_seconds: 2100,
            gate_no_progress_timeout_seconds: 1500,
            ..Default::default()
        };
        let supervision = replacement_gate_supervision(&gates);
        assert_eq!(supervision.timeout, Duration::from_secs(2100));
        assert_eq!(supervision.no_progress_timeout, Duration::from_secs(1500));
        assert_eq!(
            supervision.heartbeat_interval,
            gates.gate_heartbeat_interval()
        );
    }
}

#[cfg(test)]
mod replacement_green_gate_status_tests {
    use super::*;

    fn replacement(gates: Value) -> AgentTaskPromotionReport {
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": {"kind": "aggregate", "task_id": "task", "run_id": "run-16236"},
            "to_worktree": "homeboy@16236",
            "target": {"worktree": "homeboy@16236", "path": "/tmp/homeboy-16236"},
            "candidate": {"base_sha": "base", "candidate_sha": "patch", "tree_sha": "sha"},
            "deterministic_gates": gates,
            "operator_notification": {"status": "completed", "message": "complete"},
            "verified_base": {"base": "main", "sha": "verified-base"},
            "provenance": {}
        }))
        .expect("replacement report fixture")
    }

    #[test]
    fn non_green_replacement_gate_is_named_with_exit_code_and_tail() {
        let report = replacement(serde_json::json!([
            {"id": "gate-0", "status": "succeeded", "command": ["sh", "-c", "true"], "exit_code": 0},
            {"id": "gate-1", "status": "failed", "command": ["sh", "-c", "cargo test"],
             "exit_code": 101, "stdout": "running 3 tests\ntest it_breaks ... FAILED",
             "stderr": "thread 'it_breaks' panicked: marker-16236"}
        ]));

        let error = validate_replacement_proof_finalization_eligibility("run-16236", &report)
            .expect_err("non-green replacement gate is rejected");

        assert!(
            error
                .message
                .contains("failed `green_gate_status` (gate `gate-1` exited 101)"),
            "{}",
            error.message
        );
        assert_eq!(
            error.details["failed_eligibility_predicate"],
            "green_gate_status"
        );
        assert_eq!(error.details["gate_id"], "gate-1");
        let failing = error.details["failing_gates"]
            .as_array()
            .expect("failing gates");
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0]["gate_id"], "gate-1");
        assert_eq!(failing[0]["status"], "failed");
        assert_eq!(failing[0]["exit_code"], 101);
        assert_eq!(failing[0]["executed"], true);
        assert!(failing[0]["stdout_tail"]
            .as_str()
            .unwrap()
            .contains("it_breaks ... FAILED"));
        assert!(failing[0]["stderr_tail"]
            .as_str()
            .unwrap()
            .contains("marker-16236"));
    }

    #[test]
    fn unexecuted_replacement_names_status_and_transport_cause() {
        let report = replacement(serde_json::json!([
            {"id": "gate-1", "status": "unavailable", "command": ["true"],
             "exit_code": 0, "stderr": "[runner.lab_transport_failure] Lab gate runner admission is stale or unavailable"}
        ]));
        let error = replacement_green_gate_status_error("run", &report);
        assert!(error.message.contains("unavailable"));
        assert!(error.message.contains("runner.lab_transport_failure"));
        assert!(!error.message.contains("exited 0"));
        assert_eq!(error.details["failing_gates"][0]["executed"], false);
    }

    #[test]
    fn replacement_gate_tail_is_bounded() {
        let huge = "x".repeat(64 * 1024);
        let report = replacement(serde_json::json!([
            {"id": "gate-1", "status": "failed", "command": ["sh", "-c", "cargo test"],
             "exit_code": 101, "stderr": huge}
        ]));

        let error = validate_replacement_proof_finalization_eligibility("run-16236", &report)
            .expect_err("non-green replacement gate is rejected");

        let tail = error.details["failing_gates"][0]["stderr_tail"]
            .as_str()
            .unwrap();
        assert!(tail.len() <= 4096 + "…[truncated]".len());
    }
}
