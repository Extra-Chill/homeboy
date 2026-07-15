use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_gate::{
    AgentTaskGateRevealPolicy, AgentTaskGateStatus, AgentTaskGateVisibility,
};
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::core::agent_task_timeout_artifacts::{
    is_actionable_patch_artifact, is_empty_patch_artifact,
};
use crate::core::gate::HomeboyGateResult;
use crate::core::{Error, Result};

use super::apply::{
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspaceProvider,
    ExternalPromotionWorkspaceProvider, AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
};
use super::committed_changes::{committed_changes_patch, CommittedChangesPatch};
use super::patch::write_normalized_patch;
pub(crate) use super::patch::{normalize_promotion_patch, validate_artifact_content};
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

mod gate_run;

use gate_run::PromotionGateRun;

pub fn promote(options: AgentTaskPromotionOptions) -> Result<AgentTaskPromotionReport> {
    let mut provider = ExternalPromotionWorkspaceProvider::from_options(&options);
    let mut report = promote_with_provider(options, &mut provider)?;
    if let Some(provenance) = provider.provenance() {
        report.provenance["worktree_provider"] = provenance.clone();
    }
    if let Ok(runner_id) = std::env::var("HOMEBOY_LAB_RUNNER_ID") {
        if !runner_id.trim().is_empty() {
            report.provenance["lab_offload"] = json!({
                "runner_id": runner_id,
                "source_aggregate": report.source.path,
                "source_artifact": report.patch_artifact.path,
                "target_worktree": report.to_worktree,
                "target_workspace": report.target.path,
            });
        }
    }
    Ok(report)
}

pub(crate) fn promote_with_provider(
    options: AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
) -> Result<AgentTaskPromotionReport> {
    validate_workspace_handle(&options.to_worktree)?;
    let source_value: Value = serde_json::from_str(&options.source).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion source".to_string()),
            Some(options.source.clone()),
        )
    })?;
    let (source_kind, outcome) = select_outcome(source_value, options.task_id.as_deref())?;

    if !matches!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::CandidateRecoverable
    ) {
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "promotion requires a succeeded or recoverable-candidate outcome; task {} has status {:?}",
                outcome.task_id, outcome.status
            ),
            None,
            None,
        ));
    }

    let artifact = match select_patch_artifact(&outcome, options.artifact_id.as_deref()) {
        Ok(artifact) => artifact,
        Err(error) if options.artifact_id.is_none() && !outcome_has_patch_artifacts(&outcome) => {
            if let Some(committed_patch) = committed_changes_patch(&options)? {
                return promote_committed_changes(
                    &options,
                    provider,
                    &source_kind,
                    &outcome,
                    None,
                    committed_patch,
                );
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let patch_path = resolve_artifact_path(&artifact, options.source_path.as_deref())?;
    let patch = std::fs::read_to_string(&patch_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read patch artifact {}", patch_path.display())),
        )
    })?;
    validate_artifact_content(&artifact, &patch)?;
    if patch.trim().is_empty() {
        if let Some(committed_patch) = committed_changes_patch(&options)? {
            return promote_committed_changes(
                &options,
                provider,
                &source_kind,
                &outcome,
                Some(&artifact),
                committed_patch,
            );
        }
        let status = AgentTaskPromotionStatus::NoChanges;
        let target = AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), None);
        let operator_notification = promotion_notification(status, &target);

        return Ok(AgentTaskPromotionReport {
            schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
            status,
            source: promotion_source(&source_kind, &outcome, &options),
            to_worktree: options.to_worktree,
            target,
            patch_artifact: AgentTaskPromotionArtifactRef {
                id: artifact.id,
                kind: artifact.kind,
                path: patch_path.display().to_string(),
                sha256: artifact.sha256,
            },
            changed_files: Vec::new(),
            command_evidence: Vec::new(),
            deterministic_gates: Vec::new(),
            gate_results: Vec::new(),
            provenance: json!({
                "source_schema": outcome.schema,
                "artifact_metadata": artifact.metadata,
                "worktree_path": null,
                "dependencies_materialized": false,
            }),
            operator_notification,
        });
    }
    let normalized_patch = normalize_promotion_patch(&patch, &options.to_worktree)?;
    let changed_files = normalized_patch.changed_files.clone();

    let mut command_evidence = Vec::new();
    let mut applied_worktree_path = None;
    {
        let normalized_patch_file;
        let provider_patch_path = if normalized_patch.content == patch {
            patch_path.display().to_string()
        } else {
            normalized_patch_file = write_normalized_patch(&normalized_patch.content)?;
            normalized_patch_file.path().display().to_string()
        };
        let target = provider.apply_patch(AgentTaskPromotionApplyRequest {
            schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
            to_workspace: options.to_worktree.clone(),
            patch: Some(normalized_patch.content.clone()),
            patch_path: provider_patch_path,
            changed_files: changed_files.clone(),
            dry_run: options.dry_run,
        })?;
        command_evidence.extend(target.command_evidence);
        if !options.dry_run {
            applied_worktree_path = Some(target.path);
        }
    }

    let gates = if let Some(worktree_path) = applied_worktree_path.as_deref() {
        run_promotion_gates(&options, provider, worktree_path)?
    } else {
        PromotionGateRun::without_gates(options.dry_run)
    };
    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        applied_worktree_path.as_deref(),
    );
    let operator_notification = promotion_notification(gates.status, &target);
    let candidate = if gates.status == AgentTaskPromotionStatus::Applied {
        applied_worktree_path
            .as_deref()
            .map(|path| {
                crate::core::agent_task_promotion::candidate_fingerprint(
                    &path.display().to_string(),
                )
            })
            .transpose()?
    } else {
        None
    };

    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: gates.status,
        source: promotion_source(&source_kind, &outcome, &options),
        to_worktree: options.to_worktree,
        target,
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: artifact.id,
            kind: artifact.kind,
            path: patch_path.display().to_string(),
            sha256: artifact.sha256,
        },
        changed_files,
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
            "worktree_path": applied_worktree_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "candidate": candidate,
        }),
        operator_notification,
    })
}

fn outcome_has_patch_artifacts(outcome: &AgentTaskOutcome) -> bool {
    outcome
        .artifacts
        .iter()
        .any(|artifact| is_actionable_patch_artifact(artifact) || is_empty_patch_artifact(artifact))
}

fn promote_committed_changes(
    options: &AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
    source_kind: &str,
    outcome: &AgentTaskOutcome,
    artifact: Option<&AgentTaskArtifact>,
    committed_patch: CommittedChangesPatch,
) -> Result<AgentTaskPromotionReport> {
    let normalized_patch = normalize_promotion_patch(
        &std::fs::read_to_string(&committed_patch.patch_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "read committed changes promotion patch {}",
                    committed_patch.patch_path.display()
                )),
            )
        })?,
        &options.to_worktree,
    )?;
    let mut command_evidence = Vec::new();
    let target = provider.apply_patch(AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: options.to_worktree.clone(),
        patch: Some(normalized_patch.content.clone()),
        patch_path: committed_patch.patch_path.display().to_string(),
        changed_files: normalized_patch.changed_files.clone(),
        dry_run: options.dry_run,
    })?;
    command_evidence.extend(target.command_evidence);
    let applied_worktree_path = (!options.dry_run).then_some(target.path);
    let gates = if let Some(path) = applied_worktree_path.as_deref() {
        run_promotion_gates(options, provider, path)?
    } else {
        PromotionGateRun::without_gates(options.dry_run)
    };
    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        applied_worktree_path.as_deref(),
    );
    let operator_notification = promotion_notification(gates.status, &target);

    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: gates.status,
        source: promotion_source(source_kind, outcome, options),
        to_worktree: options.to_worktree.clone(),
        target,
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: "committed-changes".to_string(),
            kind: "patch".to_string(),
            path: committed_patch.patch_path.display().to_string(),
            sha256: Some(committed_patch.sha256),
        },
        changed_files: normalized_patch.changed_files,
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.map(|artifact| artifact.metadata.clone()).unwrap_or(Value::Null),
            "worktree_path": applied_worktree_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "change_source": "local_commits",
            "base_ref": committed_patch.base_ref,
            "commit_range": committed_patch.commit_range,
            "commits": committed_patch.commits,
        }),
        operator_notification,
    })
}

fn run_promotion_gates(
    options: &AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
    worktree_path: &Path,
) -> Result<PromotionGateRun> {
    if options.dry_run
        || (options.gates.verify.is_empty() && options.gates.private_verify.is_empty())
    {
        return Ok(PromotionGateRun::without_gates(options.dry_run));
    }

    // Materialize dependencies via the component's resolved dependency providers
    // before running verify gates so dependency misses do not mask gate signal.
    crate::core::hygiene::materialize_worktree_dependencies(worktree_path)?;
    let mut deterministic_gates = Vec::new();
    for (index, command) in options.gates.verify.iter().enumerate() {
        deterministic_gates.push(provider.verify(
            worktree_path,
            index + 1,
            command,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
        )?);
    }
    let private_offset = deterministic_gates.len();
    for (index, command) in options.gates.private_verify.iter().enumerate() {
        deterministic_gates.push(provider.verify(
            worktree_path,
            private_offset + index + 1,
            command,
            AgentTaskGateVisibility::Private,
            options.gates.private_gate_reveal,
        )?);
    }
    let has_gate_failure = deterministic_gates
        .iter()
        .any(|gate| gate.status == AgentTaskGateStatus::Failed);
    let gate_results = deterministic_gates
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();

    Ok(PromotionGateRun {
        status: status_for_report(options.dry_run, has_gate_failure),
        deterministic_gates,
        gate_results,
        dependencies_materialized: true,
    })
}

fn promotion_source(
    source_kind: &str,
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionOptions,
) -> AgentTaskPromotionSource {
    AgentTaskPromotionSource {
        kind: source_kind.to_string(),
        task_id: outcome.task_id.clone(),
        run_id: options.source_run_id.clone(),
        path: options
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
    }
}

fn status_for_report(dry_run: bool, has_gate_failure: bool) -> AgentTaskPromotionStatus {
    if dry_run {
        AgentTaskPromotionStatus::DryRun
    } else if has_gate_failure {
        AgentTaskPromotionStatus::GateFailed
    } else {
        AgentTaskPromotionStatus::Applied
    }
}

fn promotion_notification(
    status: AgentTaskPromotionStatus,
    target: &AgentTaskPromotionTarget,
) -> AgentTaskPromotionNotification {
    let target_path = target.path.as_deref().unwrap_or(target.worktree.as_str());
    match status {
        AgentTaskPromotionStatus::Applied => AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: format!(
                "patch promoted into {}; verify and finalize from {}",
                target.worktree, target_path
            ),
            resumable_blocker: None,
            next_command: Some(format!(
                "homeboy agent-task finalize-pr --run-id <run-id> --path {target_path} --title <title> --commit-message <message>"
            )),
        },
        AgentTaskPromotionStatus::GateFailed => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "patch promoted, but deterministic gates failed".to_string(),
            resumable_blocker: Some(
                "run `homeboy agent-task gate-feedback` with the promotion report, then retry the follow-up request".to_string(),
            ),
            next_command: None,
        },
        AgentTaskPromotionStatus::DryRun => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "dry run validated a patch artifact but did not apply it".to_string(),
            resumable_blocker: Some("rerun promote without `--dry-run` to apply the patch".to_string()),
            next_command: Some(format!(
                "homeboy agent-task promote <run-id> --to-worktree {}",
                target.worktree
            )),
        },
        AgentTaskPromotionStatus::NoChanges => AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: "provider completed successfully but produced an empty patch; nothing was promoted".to_string(),
            resumable_blocker: None,
            next_command: None,
        },
    }
}

fn select_outcome(source: Value, task_id: Option<&str>) -> Result<(String, AgentTaskOutcome)> {
    if source.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_OUTCOME_SCHEMA) {
        let outcome: AgentTaskOutcome = serde_json::from_value(source).map_err(|error| {
            Error::validation_invalid_json(error, Some("agent-task outcome".to_string()), None)
        })?;
        if let Some(expected) = task_id {
            if outcome.task_id != expected {
                return Err(Error::validation_invalid_argument(
                    "task_id",
                    format!(
                        "source outcome task_id is {}, not {expected}",
                        outcome.task_id
                    ),
                    None,
                    None,
                ));
            }
        }
        return Ok(("outcome".to_string(), outcome));
    }

    if source.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_AGGREGATE_SCHEMA) {
        let aggregate: AgentTaskAggregate = serde_json::from_value(source).map_err(|error| {
            Error::validation_invalid_json(error, Some("agent-task aggregate".to_string()), None)
        })?;
        let candidates: Vec<AgentTaskOutcome> = aggregate
            .outcomes
            .into_iter()
            .filter(|outcome| task_id.is_none_or(|expected| outcome.task_id == expected))
            .collect();
        return match candidates.len() {
            1 => Ok((
                "aggregate".to_string(),
                candidates.into_iter().next().unwrap(),
            )),
            0 => Err(Error::validation_invalid_argument(
                "task_id",
                "aggregate did not contain a matching outcome",
                None,
                None,
            )),
            _ => Err(Error::validation_invalid_argument(
                "task_id",
                "aggregate contains multiple outcomes; pass --task-id to select one",
                None,
                None,
            )),
        };
    }

    Err(Error::validation_invalid_argument(
        "source",
        "promotion source must be an agent-task outcome or aggregate JSON object",
        None,
        None,
    ))
}

pub(crate) fn select_patch_artifact(
    outcome: &AgentTaskOutcome,
    artifact_id: Option<&str>,
) -> Result<AgentTaskArtifact> {
    let artifacts: Vec<AgentTaskArtifact> = outcome
        .artifacts
        .iter()
        .filter(|artifact| artifact_id.is_none_or(|expected| artifact.id == expected))
        .filter(|artifact| {
            is_actionable_patch_artifact(artifact) || is_empty_patch_artifact(artifact)
        })
        .cloned()
        .collect();

    let actionable_artifacts: Vec<AgentTaskArtifact> = artifacts
        .iter()
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect();
    if !actionable_artifacts.is_empty() {
        return match actionable_artifacts.len() {
            1 => Ok(actionable_artifacts.into_iter().next().unwrap()),
            _ => Err(Error::validation_invalid_argument(
                "artifact_id",
                "multiple patch artifacts were found; pass --artifact-id to select one",
                None,
                None,
            )),
        };
    }

    match artifacts.len() {
        1 => Ok(artifacts.into_iter().next().unwrap()),
        0 => Err(Error::validation_invalid_argument(
            "artifact_id",
            "no matching patch artifact was found; inspect the agent result or transcript for diagnosis",
            None,
            None,
        )),
        _ => Err(Error::validation_invalid_argument(
            "artifact_id",
            "multiple patch artifacts were found; pass --artifact-id to select one",
            None,
            None,
        )),
    }
}

fn resolve_artifact_path(
    artifact: &AgentTaskArtifact,
    source_path: Option<&Path>,
) -> Result<PathBuf> {
    let path = artifact.path.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact.path",
            "promotion patch artifact must provide a local path",
            None,
            None,
        )
    })?;
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Ok(path);
    }
    if let Some(source_path) = source_path.and_then(Path::parent) {
        Ok(source_path.join(path))
    } else {
        Ok(path)
    }
}

fn validate_workspace_handle(handle: &str) -> Result<()> {
    if handle.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "target workspace handle must not be empty",
            None,
            None,
        ));
    }
    Ok(())
}
