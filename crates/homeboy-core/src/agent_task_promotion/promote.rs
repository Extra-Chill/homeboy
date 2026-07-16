use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::agent_task_gate::{
    AgentTaskGateRevealPolicy, AgentTaskGateStatus, AgentTaskGateVisibility,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::agent_task_timeout_artifacts::{is_actionable_patch_artifact, is_empty_patch_artifact};
use crate::gate::HomeboyGateResult;
use crate::{Error, Result};

use super::apply::{
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspaceProvider,
    ExternalPromotionWorkspaceProvider, AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
};
use super::committed_changes::{committed_changes_patch, CommittedChangesPatch};
use super::patch::write_normalized_patch;
pub(crate) use super::patch::{normalize_promotion_patch, validate_artifact_content};
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandReport, AgentTaskPromotionNotification,
    AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionSource,
    AgentTaskPromotionStatus, AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

mod gate_run;

use gate_run::PromotionGateRun;

pub fn promote(options: AgentTaskPromotionOptions) -> Result<AgentTaskPromotionReport> {
    promote_with_checkpoint(options, |_| Ok(()))
}

/// Promote a patch while recording the recoverable post-apply boundary before
/// dependency materialization or verification is attempted.
pub fn promote_with_checkpoint(
    options: AgentTaskPromotionOptions,
    mut checkpoint: impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
) -> Result<AgentTaskPromotionReport> {
    let mut provider = ExternalPromotionWorkspaceProvider::from_options(&options);
    let mut report = promote_with_provider_and_checkpoint(options, &mut provider, &mut checkpoint)?;
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

/// Rebuild a full proof for a clean candidate from a durable post-apply report.
/// The reverse apply check proves the original artifact remains in the candidate
/// before any gate result is trusted.
pub fn resume_promoted_patch(
    options: AgentTaskPromotionOptions,
    target_path: &Path,
    previous: &Value,
) -> Result<AgentTaskPromotionReport> {
    validate_resume_provenance(&options, target_path, previous)?;
    let source_value: Value = serde_json::from_str(&options.source).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion source".to_string()),
            Some(options.source.clone()),
        )
    })?;
    let (source_kind, outcome) = select_outcome(source_value, options.task_id.as_deref())?;
    let artifact = select_patch_artifact(&outcome, options.artifact_id.as_deref())?;
    let patch_path = resolve_artifact_path(
        &artifact,
        &outcome.task_id,
        options.source_run_id.as_deref(),
        options.source_path.as_deref(),
    )?;
    let patch = std::fs::read_to_string(&patch_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read patch artifact {}", patch_path.display())),
        )
    })?;
    validate_artifact_content(&artifact, &patch)?;
    let normalized_patch = normalize_promotion_patch(&patch, &options.to_worktree)?;
    let command_evidence = vec![verify_patch_is_present(
        target_path,
        &normalized_patch.content,
    )?];
    let mut provider = ExternalPromotionWorkspaceProvider::from_options(&options);
    let gates = run_promotion_gates(&options, &mut provider, target_path)?;
    let target =
        AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), Some(target_path));
    let candidate = if gates.status == AgentTaskPromotionStatus::Applied {
        Some(crate::agent_task_promotion::candidate_fingerprint(
            &target_path.display().to_string(),
        )?)
    } else {
        None
    };
    let mut report = AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: gates.status,
        source: promotion_source(&source_kind, &outcome, &options),
        to_worktree: options.to_worktree,
        target: target.clone(),
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: artifact.id,
            kind: artifact.kind,
            path: patch_path.display().to_string(),
            sha256: artifact.sha256,
        },
        changed_files: normalized_patch.changed_files,
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
            "worktree_path": target_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "candidate": candidate,
            "resumed_post_apply_promotion": true,
        }),
        operator_notification: promotion_notification(gates.status, &target),
    };
    if let Some(provenance) = provider.provenance() {
        report.provenance["worktree_provider"] = provenance.clone();
    }
    Ok(report)
}

fn validate_resume_provenance(
    options: &AgentTaskPromotionOptions,
    target_path: &Path,
    previous: &Value,
) -> Result<()> {
    if !matches!(
        previous.get("status").and_then(Value::as_str),
        Some("gate_failed" | "verification_pending")
    ) {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume requires a durable post-apply promotion",
            None,
            None,
        ));
    }
    let previous_run = previous
        .get("source_run_id")
        .and_then(Value::as_str)
        .or_else(|| previous.pointer("/source/run_id").and_then(Value::as_str));
    if previous_run != options.source_run_id.as_deref() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume source run does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    if previous.get("to_worktree").and_then(Value::as_str) != Some(options.to_worktree.as_str())
        || previous.pointer("/target/worktree").and_then(Value::as_str)
            != Some(options.to_worktree.as_str())
        || previous.pointer("/target/path").and_then(Value::as_str)
            != Some(target_path.to_string_lossy().as_ref())
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume target does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(target_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err(Error::validation_invalid_argument(
            "path",
            "promotion resume requires a clean Git worktree",
            Some(target_path.display().to_string()),
            None,
        ));
    }
    Ok(())
}

fn verify_patch_is_present(
    target_path: &Path,
    patch: &str,
) -> Result<AgentTaskPromotionCommandReport> {
    use std::io::Write;

    let mut child = std::process::Command::new("git")
        .args(["apply", "--reverse", "--check", "-"])
        .current_dir(target_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(patch.as_bytes())
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let report = AgentTaskPromotionCommandReport {
        command: vec![
            "git".to_string(),
            "apply".to_string(),
            "--reverse".to_string(),
            "--check".to_string(),
            "-".to_string(),
        ],
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        capture: Default::default(),
    };
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume could not prove the recorded patch is present in the clean target",
            None,
            None,
        ));
    }
    Ok(report)
}

pub(crate) fn promote_with_provider(
    options: AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
) -> Result<AgentTaskPromotionReport> {
    promote_with_provider_and_checkpoint(options, provider, &mut |_| Ok(()))
}

pub(super) fn promote_with_provider_and_checkpoint(
    options: AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
) -> Result<AgentTaskPromotionReport> {
    validate_workspace_handle(&options.to_worktree)?;
    let source_value: Value = serde_json::from_str(&options.source).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion source".to_string()),
            Some(options.source.clone()),
        )
    })?;
    let source_for_provenance = source_value.clone();
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

    if outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable
        && outcome
            .artifacts
            .iter()
            .filter(|artifact| is_actionable_patch_artifact(artifact))
            .count()
            != 1
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "recoverable-candidate promotion requires exactly one actionable patch artifact",
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
                    checkpoint,
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
    if outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable
        && !has_recoverable_candidate_provenance(&options, &outcome, &artifact)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "recoverable-candidate promotion requires a fingerprinted artifact bound to its producing run, task, base, and workspace",
            Some(artifact.id.clone()),
            None,
        ));
    }
    let gate_feedback_baseline =
        gate_feedback_baseline_for_artifact(&source_for_provenance, &outcome, &artifact)?;
    let patch_path = resolve_artifact_path(
        &artifact,
        &outcome.task_id,
        options.source_run_id.as_deref(),
        options.source_path.as_deref(),
    )?;
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
                checkpoint,
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
            gate_feedback_baseline,
            dry_run: options.dry_run,
        })?;
        command_evidence.extend(target.command_evidence);
        if !options.dry_run {
            applied_worktree_path = Some(target.path);
        }
    }

    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        applied_worktree_path.as_deref(),
    );
    if let Some(worktree_path) = applied_worktree_path.as_deref() {
        checkpoint(&post_apply_report(
            &options,
            &source_kind,
            &outcome,
            AgentTaskPromotionArtifactRef {
                id: artifact.id.clone(),
                kind: artifact.kind.clone(),
                path: patch_path.display().to_string(),
                sha256: artifact.sha256.clone(),
            },
            changed_files.clone(),
            command_evidence.clone(),
            target.clone(),
            worktree_path,
            &outcome.schema,
            artifact.metadata.clone(),
        ))?;
    }
    let gates = if let Some(worktree_path) = applied_worktree_path.as_deref() {
        run_promotion_gates(&options, provider, worktree_path)?
    } else {
        PromotionGateRun::without_gates(options.dry_run)
    };
    let operator_notification = promotion_notification(gates.status, &target);
    let candidate = if gates.status == AgentTaskPromotionStatus::Applied {
        applied_worktree_path
            .as_deref()
            .map(|path| {
                crate::agent_task_promotion::candidate_fingerprint(&path.display().to_string())
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

fn gate_feedback_baseline_for_artifact(
    source: &Value,
    outcome: &AgentTaskOutcome,
    selected: &AgentTaskArtifact,
) -> Result<Option<Value>> {
    let canonical = outcome
        .artifacts
        .iter()
        .find(|artifact| artifact.id == selected.id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                "selected patch artifact is missing from canonical outcome artifacts",
                None,
                None,
            )
        })?;
    let source_baseline = source_canonical_artifact(source, &outcome.task_id, &canonical.id)?
        .and_then(|artifact| artifact.get("metadata"))
        .and_then(|metadata| metadata.get("gate_feedback_baseline"))
        .cloned();
    let baseline = source_baseline
        .clone()
        .or_else(|| canonical.metadata.get("gate_feedback_baseline").cloned());
    if let Some(raw) = source_baseline.as_ref() {
        if canonical.metadata.get("gate_feedback_baseline") != Some(raw) {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                "canonical patch artifact baseline provenance changed during source deserialization",
                Some(canonical.id.clone()),
                None,
            ));
        }
    }
    for typed in &outcome.typed_artifacts {
        if let Some(duplicate) = typed
            .artifact
            .as_ref()
            .filter(|artifact| artifact.id == canonical.id)
            .and_then(|artifact| artifact.metadata.get("gate_feedback_baseline"))
        {
            if baseline.as_ref() != Some(duplicate) {
                return Err(Error::validation_invalid_argument(
                    "artifact_id",
                    "typed patch artifact baseline provenance conflicts with the canonical artifact",
                    Some(canonical.id.clone()),
                    None,
                ));
            }
        }
    }
    Ok(baseline)
}

fn source_canonical_artifact<'a>(
    source: &'a Value,
    task_id: &str,
    artifact_id: &str,
) -> Result<Option<&'a Value>> {
    let outcomes = if source.get("schema").and_then(Value::as_str)
        == Some(AGENT_TASK_OUTCOME_SCHEMA)
    {
        std::slice::from_ref(source)
    } else if source.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_AGGREGATE_SCHEMA) {
        source
            .get("outcomes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    } else {
        return Ok(None);
    };
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.get("task_id").and_then(Value::as_str) == Some(task_id));
    let artifact = outcome
        .and_then(|outcome| outcome.get("artifacts"))
        .and_then(Value::as_array)
        .and_then(|artifacts| {
            artifacts
                .iter()
                .find(|artifact| artifact.get("id").and_then(Value::as_str) == Some(artifact_id))
        });
    Ok(artifact)
}

fn outcome_has_patch_artifacts(outcome: &AgentTaskOutcome) -> bool {
    outcome
        .artifacts
        .iter()
        .any(|artifact| is_actionable_patch_artifact(artifact) || is_empty_patch_artifact(artifact))
}

fn has_recoverable_candidate_provenance(
    options: &AgentTaskPromotionOptions,
    outcome: &AgentTaskOutcome,
    artifact: &AgentTaskArtifact,
) -> bool {
    artifact.kind == "patch"
        && artifact.size_bytes.is_some_and(|size| size > 0)
        && artifact.sha256.as_deref().is_some_and(valid_sha256)
        && artifact.metadata.get("task_id").and_then(Value::as_str) == Some(&outcome.task_id)
        && artifact
            .metadata
            .get("producer_attempt")
            .is_some_and(Value::is_u64)
        && [
            "run_id",
            "base_ref",
            "provider_backend",
            "repository_identity",
            "workspace_identity",
        ]
        .iter()
        .all(|key| {
            artifact
                .metadata
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        && options.source_run_id.as_deref().is_none_or(|run_id| {
            artifact.metadata.get("run_id").and_then(Value::as_str) == Some(run_id)
        })
        && options.task_base_sha.as_deref().is_none_or(|base_ref| {
            artifact.metadata.get("base_ref").and_then(Value::as_str) == Some(base_ref)
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn promote_committed_changes(
    options: &AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
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
        gate_feedback_baseline: artifact
            .and_then(|artifact| artifact.metadata.get("gate_feedback_baseline"))
            .cloned(),
        dry_run: options.dry_run,
    })?;
    command_evidence.extend(target.command_evidence);
    let applied_worktree_path = (!options.dry_run).then_some(target.path);
    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        applied_worktree_path.as_deref(),
    );
    if let Some(path) = applied_worktree_path.as_deref() {
        checkpoint(&post_apply_report(
            options,
            source_kind,
            outcome,
            AgentTaskPromotionArtifactRef {
                id: "committed-changes".to_string(),
                kind: "patch".to_string(),
                path: committed_patch.patch_path.display().to_string(),
                sha256: Some(committed_patch.sha256.clone()),
            },
            normalized_patch.changed_files.clone(),
            command_evidence.clone(),
            target.clone(),
            path,
            &outcome.schema,
            artifact
                .map(|artifact| artifact.metadata.clone())
                .unwrap_or(Value::Null),
        ))?;
    }
    let gates = if let Some(path) = applied_worktree_path.as_deref() {
        run_promotion_gates(options, provider, path)?
    } else {
        PromotionGateRun::without_gates(options.dry_run)
    };
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
    crate::hygiene::materialize_worktree_dependencies(worktree_path)?;
    let mut deterministic_gates = Vec::new();
    for (index, command) in options.gates.verify.iter().enumerate() {
        deterministic_gates.push(run_promotion_gate(
            options,
            provider,
            worktree_path,
            index + 1,
            command,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
        )?);
    }
    let private_offset = deterministic_gates.len();
    for (index, command) in options.gates.private_verify.iter().enumerate() {
        deterministic_gates.push(run_promotion_gate(
            options,
            provider,
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

fn run_promotion_gate(
    options: &AgentTaskPromotionOptions,
    provider: &mut impl AgentTaskPromotionWorkspaceProvider,
    worktree_path: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
) -> Result<crate::agent_task_gate::AgentTaskGateReport> {
    let run_id = options
        .source_run_id
        .as_deref()
        .unwrap_or("unrecorded-promotion");
    let allocation = crate::controller_scratch::allocate_attempt(
        run_id,
        "promotion-verification",
        &format!("gate-{index}"),
        1,
    )?;
    let runtime_tmpdir = match crate::engine::run_dir::RunDir::create().and_then(|run_dir| {
        crate::engine::invocation::InvocationGuard::acquire(
            &run_dir,
            &crate::engine::invocation::InvocationRequirements::default(),
        )
    }) {
        Ok(runtime_tmpdir) => runtime_tmpdir,
        Err(error) => {
            crate::controller_scratch::release_attempt(
                &allocation,
                "verification_runtime_setup_failed",
                serde_json::json!({ "error": error.message }),
            )?;
            return Err(error);
        }
    };
    let result = provider.verify_with_runtime_tmpdir(
        worktree_path,
        index,
        command,
        visibility,
        reveal_policy,
        &runtime_tmpdir.context().tmp_dir,
    );
    let evidence = match &result {
        Ok(report) => serde_json::json!({
            "gate_id": report.id,
            "status": report.status,
            "exit_code": report.exit_code,
        }),
        Err(error) => serde_json::json!({ "error": error.message }),
    };
    let reason = match &result {
        Ok(report) if report.status == AgentTaskGateStatus::Succeeded => "verification_succeeded",
        Ok(_) => "verification_failed",
        Err(_) => "verification_execution_failed",
    };
    crate::controller_scratch::release_attempt(&allocation, reason, evidence)?;
    result
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
        AgentTaskPromotionStatus::VerificationPending => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "patch promoted; deterministic verification is pending".to_string(),
            resumable_blocker: Some("rerun promotion to resume verification without reapplying the patch".to_string()),
            next_command: None,
        },
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

fn post_apply_report(
    options: &AgentTaskPromotionOptions,
    source_kind: &str,
    outcome: &AgentTaskOutcome,
    patch_artifact: AgentTaskPromotionArtifactRef,
    changed_files: Vec<String>,
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
    target: AgentTaskPromotionTarget,
    worktree_path: &Path,
    source_schema: &str,
    artifact_metadata: Value,
) -> AgentTaskPromotionReport {
    let status = AgentTaskPromotionStatus::VerificationPending;
    let operator_notification = promotion_notification(status, &target);
    AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status,
        source: promotion_source(source_kind, outcome, options),
        to_worktree: options.to_worktree.clone(),
        target,
        patch_artifact,
        changed_files,
        command_evidence,
        deterministic_gates: Vec::new(),
        gate_results: Vec::new(),
        provenance: json!({
            "source_schema": source_schema,
            "artifact_metadata": artifact_metadata,
            "worktree_path": worktree_path,
            "dependencies_materialized": false,
            "post_apply": true,
        }),
        operator_notification,
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
    task_id: &str,
    source_run_id: Option<&str>,
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
        if let Some(run_id) = source_run_id {
            if let Some(projected) =
                crate::agent_task_lifecycle::verified_controller_artifact_projection_path(
                    run_id, task_id, artifact,
                )?
            {
                return Ok(projected);
            }
        }
        if !path.is_file() {
            return Err(Error::validation_invalid_argument(
                "artifact.path",
                "promotion could not find a verified controller-side artifact projection; reconcile the run on this controller before promoting its runner-produced artifact",
                Some(path.display().to_string()),
                None,
            ));
        }
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
