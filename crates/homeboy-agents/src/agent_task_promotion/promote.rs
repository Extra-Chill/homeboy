use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::agent_task_gate::{
    AgentTaskGateRevealPolicy, AgentTaskGateStatus, AgentTaskGateVisibility,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::agent_task_timeout_artifacts::{
    is_actionable_patch_artifact, is_empty_patch_artifact, is_patch_artifact_kind,
};
use homeboy_core::gate::HomeboyGateResult;
use homeboy_core::{Error, Result};

use super::apply::{
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspaceProvider,
    ExternalPromotionWorkspaceProvider, TrustedUnpushedCandidateDestination,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
};
use super::committed_changes::{committed_changes_patch, CommittedChangesPatch};
use super::patch::write_normalized_patch;
pub(crate) use super::patch::{normalize_promotion_patch, validate_artifact_content};
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandReport, AgentTaskPromotionNotification,
    AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionSource,
    AgentTaskPromotionStatus, AgentTaskPromotionTarget, AgentTaskPromotionVerifiedBase,
    AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

mod gate_run;

use gate_run::PromotionGateRun;

thread_local! {
    static GATE_SUPERVISION: RefCell<Option<Arc<crate::agent_task_gate::GateSupervision>>> = const { RefCell::new(None) };
}

pub(crate) fn with_gate_supervision<T>(
    supervision: crate::agent_task_gate::GateSupervision,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    GATE_SUPERVISION.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "promotion gate supervision scopes cannot nest"
        );
        *slot.borrow_mut() = Some(Arc::new(supervision));
        let result = operation();
        *slot.borrow_mut() = None;
        result
    })
}

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
    if let Some(runner_id) = crate::agent_task_lifecycle::execution_runner_id() {
        report.provenance["lab_offload"] = json!({
            "runner_id": runner_id,
            "source_aggregate": report.source.path,
            "source_artifact": report.patch_artifact.path,
            "target_worktree": report.to_worktree,
            "target_workspace": report.target.path,
        });
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
    validate_resume_candidate(&options, target_path, previous, &outcome, &artifact)?;
    let normalized_patch = normalize_promotion_patch(&patch, &options.to_worktree)?;
    let command_evidence = vec![verify_patch_is_present(
        target_path,
        &normalized_patch.content,
    )?];
    let mut provider = ExternalPromotionWorkspaceProvider::from_options(&options);
    let verified_base = capture_declared_base(target_path, options.base_ref.as_deref())?;
    let gates = run_promotion_gates(&options, &mut provider, target_path)?;
    let target =
        AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), Some(target_path));
    let candidate = if gates.status.patch_promoted() {
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
        changed_files: persisted_changed_files(normalized_patch.changed_files, candidate.as_ref()),
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        verified_base,
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
    let previous_status = previous.get("status").and_then(Value::as_str);
    if !matches!(
        previous_status,
        Some("gate_failed" | "verification_pending")
    ) && !(options.gates.rerun_completed_gates && previous_status == Some("applied"))
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume requires a durable post-apply promotion or an explicit completed-gate rerun for an applied promotion",
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
    Ok(())
}

fn validate_resume_candidate(
    options: &AgentTaskPromotionOptions,
    target_path: &Path,
    previous: &Value,
    outcome: &AgentTaskOutcome,
    artifact: &AgentTaskArtifact,
) -> Result<()> {
    if previous.pointer("/source/task_id").and_then(Value::as_str) != Some(&outcome.task_id)
        || previous
            .pointer("/patch_artifact/id")
            .and_then(Value::as_str)
            != Some(&artifact.id)
        || previous
            .pointer("/patch_artifact/kind")
            .and_then(Value::as_str)
            != Some(&artifact.kind)
        || previous
            .pointer("/patch_artifact/sha256")
            .and_then(Value::as_str)
            != artifact.sha256.as_deref()
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume source task or patch artifact does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    let expected_inputs = json!({
        "base_ref": options.base_ref,
        "task_base_sha": options.task_base_sha,
        "candidate_ref": options.candidate_ref,
    });
    if previous
        .pointer("/provenance/resume_inputs")
        .is_some_and(|recorded| recorded != &expected_inputs)
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume base or candidate input does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(target_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !status.status.success() {
        return Err(Error::validation_invalid_argument(
            "path",
            "promotion resume target is not an accessible Git worktree",
            Some(target_path.display().to_string()),
            None,
        ));
    }
    let expected = previous
        .pointer("/provenance/candidate")
        .filter(|candidate| !candidate.is_null())
        .cloned();
    if !status.stdout.is_empty() && expected.is_none() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume found a dirty target without an exact post-apply candidate fingerprint; rerun from a clean target",
            None,
            None,
        ));
    }
    if let Some(expected) = expected {
        let expected = serde_json::from_value(expected).map_err(|_| {
            Error::validation_invalid_argument(
                "promotion",
                "promotion resume durable candidate fingerprint is invalid",
                None,
                None,
            )
        })?;
        let actual = crate::agent_task_promotion::candidate_fingerprint(
            target_path.to_string_lossy().as_ref(),
        )?;
        if actual != expected {
            return Err(Error::validation_invalid_argument(
                "promotion",
                "promotion resume target differs from the exact checkpointed applied candidate",
                Some(target_path.display().to_string()),
                None,
            ));
        }
    }
    if let Some(recorded_base) = previous.get("verified_base") {
        let current_base = serde_json::to_value(capture_declared_base(
            target_path,
            options.base_ref.as_deref(),
        )?)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
        if &current_base != recorded_base {
            return Err(Error::validation_invalid_argument(
                "base_ref",
                "promotion resume declared base no longer matches the durable post-apply promotion",
                None,
                None,
            ));
        }
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

    let failed_candidate_adoption = options.candidate_ref.is_some()
        && outcome.status == AgentTaskOutcomeStatus::Failed
        && has_pre_provider_transport_recovery_eligibility(&outcome);
    if !matches!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded
            | AgentTaskOutcomeStatus::CandidateRecoverable
            | AgentTaskOutcomeStatus::NoOp
    ) && !failed_candidate_adoption
    {
        let problem = if options.candidate_ref.is_some()
            && outcome.status == AgentTaskOutcomeStatus::Failed
        {
            "immutable candidate adoption requires explicit durable pre-provider transport recovery eligibility; legacy or provider/test failures remain ineligible. Retry the cook or record a new transport failure through Homeboy."
                .to_string()
        } else {
            format!(
                "promotion requires a succeeded, recoverable-candidate, or no-op outcome; task {} has status {:?}",
                outcome.task_id, outcome.status
            )
        };
        return Err(Error::validation_invalid_argument(
            "source", problem, None, None,
        ));
    }

    if outcome.status == AgentTaskOutcomeStatus::NoOp {
        let committed_patch = committed_changes_patch(&options)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "source",
                "no-op promotion requires an audited committed candidate after the task base",
                None,
                None,
            )
        })?;
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

    // Adoption supplies an immutable commit candidate. It intentionally bypasses
    // provider artifact selection, but still uses the ordinary promotion/gates
    // implementation and durable checkpoint.
    if options.candidate_ref.is_some() {
        let committed_patch = committed_changes_patch(&options)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate_ref",
                "candidate revision contains no changes after the recorded task base",
                options.candidate_ref.clone(),
                None,
            )
        })?;
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

    let artifact = match if outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable {
        select_recoverable_patch_artifact(&outcome, &options)
    } else {
        select_patch_artifact(&outcome, options.artifact_id.as_deref())
    } {
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
    let promotion_chain_baseline =
        promotion_chain_baseline_for_artifact(&source_for_provenance, &outcome, &artifact)?;
    let destination_baseline = promotion_chain_baseline
        .as_ref()
        .or(gate_feedback_baseline.as_ref())
        .cloned();
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
        let worktree_path = options.source_worktree_path.as_deref();
        let target =
            AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), worktree_path);
        let gates = if let Some(worktree_path) = worktree_path {
            run_promotion_gates(&options, provider, worktree_path)?
        } else {
            PromotionGateRun::without_gates(options.dry_run)
        };
        let status = match gates.status {
            AgentTaskPromotionStatus::Applied => AgentTaskPromotionStatus::VerifiedNoChanges,
            AgentTaskPromotionStatus::GateFailed => AgentTaskPromotionStatus::NoChangesGateFailed,
            _ => AgentTaskPromotionStatus::NoChanges,
        };
        let operator_notification = promotion_notification(status, &target);
        let verified_revision = target.head.clone();
        let candidate = target
            .path
            .as_deref()
            .map(crate::agent_task_promotion::candidate_fingerprint)
            .transpose()?;

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
            deterministic_gates: gates.deterministic_gates,
            gate_results: gates.gate_results,
            verified_base: None,
            provenance: json!({
                "source_schema": outcome.schema,
                "artifact_metadata": artifact.metadata,
                "worktree_path": worktree_path,
                "verified_revision": verified_revision,
                "dependencies_materialized": gates.dependencies_materialized,
                "candidate": candidate,
            }),
            operator_notification,
        });
    }
    let normalized_patch = normalize_promotion_patch(&patch, &options.to_worktree)?;
    let changed_files = normalized_patch.changed_files.clone();

    // Validate the declared remote base BEFORE mutating the target worktree.
    // Promotion must be atomic around base validation: a nonexistent declared
    // base (e.g. `--base trunk` on a repo whose base is `main`) has to fail with
    // no patch applied and no durable post-apply state recorded, so the same
    // artifact/target can be retried with a corrected base (#9400).
    let pre_apply_verified_base = if !options.dry_run {
        match resolve_promotion_target_path(&options.to_worktree)? {
            Some(target_path) => capture_declared_base(&target_path, options.base_ref.as_deref())?,
            // No pre-apply Homeboy-managed target path resolves (e.g. a
            // provider-owned destination); fall back to validating against the
            // applied worktree below, preserving prior behavior for that path.
            None => None,
        }
    } else {
        None
    };
    // A declared base with no resolvable pre-apply target path still needs
    // validation after apply; an empty/absent base has nothing to verify.
    let base_verified_before_apply = pre_apply_verified_base.is_some()
        || options
            .base_ref
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty);

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
            gate_feedback_baseline: destination_baseline,
            dry_run: options.dry_run,
            trusted_unpushed_candidate_destination: None,
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
    let post_apply = if let Some(worktree_path) = applied_worktree_path.as_deref() {
        let report = post_apply_report(
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
        )?;
        checkpoint(&report)?;
        Some(report)
    } else {
        None
    };
    let verified_base = if let Some(worktree_path) = applied_worktree_path.as_deref() {
        // Reuse the base verified before apply; only re-capture when a
        // provider-owned destination had no resolvable pre-apply target path.
        let verified_base = if base_verified_before_apply {
            pre_apply_verified_base
        } else {
            capture_declared_base(worktree_path, options.base_ref.as_deref())?
        };
        (
            run_promotion_gates(&options, provider, worktree_path)?,
            verified_base,
        )
    } else {
        (PromotionGateRun::without_gates(options.dry_run), None)
    };
    let (gates, verified_base) = verified_base;
    let operator_notification = promotion_notification(gates.status, &target);
    // Gates can create incidental files. Feedback must retain the identity that
    // was captured immediately after applying the provider candidate.
    let candidate = post_apply
        .as_ref()
        .and_then(|report| report.provenance.get("candidate").cloned())
        .and_then(|value| serde_json::from_value(value).ok());
    let gate_feedback_baseline = post_apply
        .as_ref()
        .and_then(|report| report.provenance.get("gate_feedback_baseline").cloned());

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
        changed_files: persisted_changed_files(changed_files, candidate.as_ref()),
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        verified_base,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
            "worktree_path": applied_worktree_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "candidate": candidate,
            "destination_baseline": candidate,
            "prior_baseline": promotion_chain_baseline,
            "gate_feedback_baseline": gate_feedback_baseline,
        }),
        operator_notification,
    })
}

fn promotion_chain_baseline_for_artifact(
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
    let source_provenance = source_canonical_artifact(source, &outcome.task_id, &canonical.id)?
        .and_then(|artifact| {
            artifact.pointer("/metadata/source_provenance/verified_cook_baseline")
        });
    let provenance = source_provenance.or_else(|| {
        canonical
            .metadata
            .pointer("/source_provenance/verified_cook_baseline")
    });
    let Some(provenance) = provenance else {
        return Ok(None);
    };
    if source_provenance.is_some()
        && canonical
            .metadata
            .pointer("/source_provenance/verified_cook_baseline")
            != source_provenance
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "canonical patch artifact source baseline provenance changed during source deserialization",
            Some(canonical.id.clone()),
            None,
        ));
    }
    let source_tree = provenance
        .get("baseline_tree")
        .and_then(Value::as_str)
        .filter(|tree| valid_git_object_id(tree))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                "follow-up patch artifact has no valid verified source tree",
                Some(canonical.id.clone()),
                None,
            )
        })?;
    let prior_sha256 = provenance
        .get("promoted_patch_artifact_sha256")
        .and_then(Value::as_str)
        .filter(|sha256| valid_sha256(sha256))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                "follow-up patch artifact has no valid prior promoted artifact identity",
                Some(canonical.id.clone()),
                None,
            )
        })?;
    Ok(Some(json!({
        "schema": "homeboy/agent-task-promotion-chain-baseline/v1",
        "source_tree": source_tree,
        "prior_patch_artifact": {
            "sha256": prior_sha256,
            "source_run_id": provenance.get("source_run_id"),
            "source_task_id": provenance.get("source_task_id"),
        },
    })))
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn has_pre_provider_transport_recovery_eligibility(outcome: &AgentTaskOutcome) -> bool {
    let eligibility = outcome
        .metadata
        .get("candidate_adoption_recovery")
        .or_else(|| outcome.outputs.get("candidate_adoption_recovery"));
    eligibility
        .filter(|value| crate::agent_task_lifecycle::is_pre_provider_transport_recovery(value))
        .is_some()
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
    canonical_patch_kind(&artifact.kind).is_some()
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

#[cfg(test)]
mod declared_base_tests {
    use super::*;

    fn git(path: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git runs")
            .success());
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git runs");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn declared_base_capture_is_immune_to_unrelated_fetch_head_activity() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("base"), "base").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "main"]);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        let main_sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        git(repo.path(), &["checkout", "-b", "other"]);
        std::fs::write(repo.path().join("other"), "other").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "other"]);
        git(repo.path(), &["push", "origin", "other"]);
        git(repo.path(), &["checkout", "main"]);

        let captured = capture_declared_base(repo.path(), Some("main"))
            .expect("capture main")
            .expect("declared base");
        git(repo.path(), &["fetch", "origin", "refs/heads/other"]);

        assert_eq!(captured.base, "main");
        assert_eq!(captured.sha, main_sha);
        assert!(git_output(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/homeboy/promotion/base"
            ],
        )
        .is_empty());
    }
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
        trusted_unpushed_candidate_destination: options.candidate_ref.as_ref().map(|_| {
            TrustedUnpushedCandidateDestination {
                path: options
                    .source_worktree_path
                    .clone()
                    .expect("candidate has source workspace"),
                head: committed_patch.candidate.clone(),
            }
        }),
    })?;
    command_evidence.extend(target.command_evidence);
    let applied_worktree_path = (!options.dry_run).then_some(target.path);
    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        applied_worktree_path.as_deref(),
    );
    let post_apply = if let Some(path) = applied_worktree_path.as_deref() {
        let report = post_apply_report(
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
        )?;
        checkpoint(&report)?;
        Some(report)
    } else {
        None
    };
    let verified_base = if let Some(path) = applied_worktree_path.as_deref() {
        let verified_base = capture_declared_base(path, options.base_ref.as_deref())?;
        (run_promotion_gates(options, provider, path)?, verified_base)
    } else {
        (PromotionGateRun::without_gates(options.dry_run), None)
    };
    let (gates, verified_base) = verified_base;
    let operator_notification = promotion_notification(gates.status, &target);
    let candidate = post_apply
        .as_ref()
        .and_then(|report| report.provenance.get("candidate").cloned())
        .and_then(|value| serde_json::from_value(value).ok());
    let gate_feedback_baseline = post_apply
        .as_ref()
        .and_then(|report| report.provenance.get("gate_feedback_baseline").cloned());

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
        changed_files: persisted_changed_files(normalized_patch.changed_files, candidate.as_ref()),
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        verified_base,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.map(|artifact| artifact.metadata.clone()).unwrap_or(Value::Null),
            "worktree_path": applied_worktree_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "change_source": "local_commits",
            "base_ref": committed_patch.base_ref,
            "commit_range": committed_patch.commit_range,
            "commits": committed_patch.commits,
            "candidate": candidate,
            "destination_baseline": candidate,
            "gate_feedback_baseline": gate_feedback_baseline,
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
    homeboy_core::hygiene::materialize_worktree_dependencies(worktree_path)?;
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
    let runtime_tmpdir = match homeboy_core::engine::run_dir::RunDir::create().and_then(|run_dir| {
        homeboy_core::engine::invocation::InvocationGuard::acquire(
            &run_dir,
            &homeboy_core::engine::invocation::InvocationRequirements::default(),
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
    let supervision = GATE_SUPERVISION.with(|slot| slot.borrow().clone());
    let result = if let Some(supervision) = supervision.as_deref() {
        crate::agent_task_gate::run_gate_command_with_supervision(
            worktree_path,
            index,
            command,
            visibility,
            reveal_policy,
            Some(&runtime_tmpdir.context().tmp_dir),
            Some(supervision),
        )
    } else {
        provider.verify_with_runtime_tmpdir(
            worktree_path,
            index,
            command,
            visibility,
            reveal_policy,
            &runtime_tmpdir.context().tmp_dir,
        )
    };
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

/// Resolve the Homeboy-managed target worktree path before the patch is
/// applied, so the declared base can be validated against it without mutating
/// the working tree (#9400). Returns `None` for a non-Homeboy destination
/// (provider-owned), where the pre-apply path is not known and validation falls
/// back to the applied worktree.
fn resolve_promotion_target_path(to_worktree: &str) -> Result<Option<PathBuf>> {
    let Some(record) = homeboy_core::worktree::resolve_workspace_ref_if_present(to_worktree)?
    else {
        return Ok(None);
    };
    if record.state() != &homeboy_core::worktree::TaskWorktreeState::Active {
        return Ok(None);
    }
    let path = PathBuf::from(record.path());
    Ok(path.is_dir().then_some(path))
}

fn capture_declared_base(
    worktree_path: &Path,
    base_ref: Option<&str>,
) -> Result<Option<AgentTaskPromotionVerifiedBase>> {
    let Some(base_ref) = base_ref.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let observed = Command::new("git")
        .args([
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{base_ref}"),
        ])
        .current_dir(worktree_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !observed.status.success() {
        return Err(Error::validation_invalid_argument(
            "base_ref",
            format!(
                "could not capture declared base `{base_ref}` before promotion gates: {}",
                String::from_utf8_lossy(&observed.stderr).trim()
            ),
            None,
            None,
        ));
    }
    let sha = String::from_utf8_lossy(&observed.stdout)
        .split_whitespace()
        .next()
        .filter(|sha| !sha.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "base_ref",
                format!(
                    "declared base `{base_ref}` was not found on origin before promotion gates"
                ),
                None,
                None,
            )
        })?
        .to_string();
    let fetch = Command::new("git")
        .args([
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "origin",
            &sha,
        ])
        .current_dir(worktree_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !fetch.status.success() {
        return Err(Error::validation_invalid_argument("base_ref", format!("could not materialize observed declared base `{base_ref}` at {sha}; retry promotion: {}", String::from_utf8_lossy(&fetch.stderr).trim()), None, None));
    }
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{sha}^{{commit}}")])
        .current_dir(worktree_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "base_ref",
            format!("could not resolve declared base `{base_ref}` before promotion gates"),
            None,
            None,
        ));
    }
    Ok(Some(AgentTaskPromotionVerifiedBase {
        base: base_ref.to_string(),
        sha: String::from_utf8_lossy(&output.stdout).trim().to_string(),
    }))
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
                "patch promoted into {}; finalize from {} using the verified_base_sha recorded in this promotion report",
                target.worktree, target_path
            ),
            resumable_blocker: None,
            next_command: None,
        },
        AgentTaskPromotionStatus::GateFailed => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "patch promoted, but deterministic gates failed".to_string(),
            resumable_blocker: Some(
                "run `homeboy agent-task gate-feedback` with the promotion report, then retry the follow-up request".to_string(),
            ),
            next_command: None,
        },
        AgentTaskPromotionStatus::VerifiedNoChanges => AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: "provider produced no patch; the pinned candidate workspace passed deterministic verification".to_string(),
            resumable_blocker: None,
            next_command: None,
        },
        AgentTaskPromotionStatus::NoChangesGateFailed => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "provider produced no patch, but deterministic verification failed in the pinned candidate workspace".to_string(),
            resumable_blocker: Some("repair the candidate and rerun Cook so the declared verification gates pass".to_string()),
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
) -> Result<AgentTaskPromotionReport> {
    let status = AgentTaskPromotionStatus::VerificationPending;
    let operator_notification = promotion_notification(status, &target);
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(&worktree_path.display().to_string())
            .ok();
    let gate_feedback_baseline = match candidate.as_ref() {
        Some(crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { .. }) => Some(json!({
            "schema": "homeboy/agent-task-gate-feedback-baseline/v1",
            "current_diff": candidate_current_diff(worktree_path)?,
        })),
        _ => None,
    };
    // A base observation is recorded when the target can reach its remote. The
    // post-apply checkpoint must still survive a transient remote failure so it
    // can protect the already-applied candidate for recovery.
    let verified_base = capture_declared_base(worktree_path, options.base_ref.as_deref())
        .ok()
        .flatten();
    Ok(AgentTaskPromotionReport {
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
        verified_base,
        provenance: json!({
            "source_schema": source_schema,
            "artifact_metadata": artifact_metadata,
            "worktree_path": worktree_path,
            "dependencies_materialized": false,
            "post_apply": true,
            "candidate": candidate,
            "destination_baseline": candidate,
            "gate_feedback_baseline": gate_feedback_baseline,
            "resume_inputs": {
                "base_ref": options.base_ref,
                "task_base_sha": options.task_base_sha,
                "candidate_ref": options.candidate_ref,
            },
        }),
        operator_notification,
    })
}

/// Capture the complete candidate delta without changing the destination index.
fn candidate_current_diff(worktree_path: &Path) -> Result<String> {
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create gate-feedback candidate index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    for arguments in [vec!["read-tree", "HEAD"], vec!["add", "--all"]] {
        let output = Command::new("git")
            .args(arguments)
            .env("GIT_INDEX_FILE", &index_path)
            .current_dir(worktree_path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
    }
    let output = Command::new("git")
        .args(["diff", "--cached", "--binary", "--full-index", "HEAD"])
        .env("GIT_INDEX_FILE", index_path)
        .current_dir(worktree_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn persisted_changed_files(
    patch_changed_files: Vec<String>,
    candidate: Option<&crate::agent_task_promotion::AgentTaskPromotionCandidate>,
) -> Vec<String> {
    match candidate {
        Some(crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint })
            if !fingerprint.changed_files.is_empty() =>
        {
            fingerprint.changed_files.clone()
        }
        _ => patch_changed_files,
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

/// Select one recoverable patch after collapsing provider aliases and duplicate
/// representations. Content is normalized before hashing so sandbox wrappers do
/// not turn one patch into a false review choice.
fn select_recoverable_patch_artifact(
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionOptions,
) -> Result<AgentTaskArtifact> {
    let canonical = canonical_recoverable_patch_artifacts(outcome, options)?;
    match canonical.artifacts.len() {
        1 => Ok(canonical.artifacts.into_iter().next().expect("one canonical patch")),
        0 => Err(Error::new(
            homeboy_core::ErrorCode::ValidationInvalidArgument,
            "recoverable-candidate promotion found no readable actionable patch; reconcile or hydrate the run artifacts before retrying",
            json!({
                "field": "artifact_id",
                "next_action": "homeboy agent-task review <run-id> --to-worktree <managed-worktree>",
                "unavailable_artifacts": canonical.unavailable,
            }),
        )),
        _ => Err(Error::new(
            homeboy_core::ErrorCode::ValidationInvalidArgument,
            "recoverable-candidate promotion found distinct actionable patches; select one with --artifact-id",
            json!({
                "field": "artifact_id",
                "review_choices": canonical.artifacts.into_iter().map(|artifact| json!({
                    "id": artifact.id,
                    "kind": artifact.kind,
                    "sha256": artifact.sha256,
                    "canonical_identity": canonical_patch_identity(&artifact),
                })).collect::<Vec<_>>(),
            }),
        )),
    }
}

#[derive(Debug, Clone)]
pub struct CanonicalRecoverablePatchArtifacts {
    pub artifacts: Vec<AgentTaskArtifact>,
    pub unavailable: Vec<Value>,
}

/// Resolve and normalize recoverable provider artifacts into deterministic patch
/// choices. Both review and promotion use this contract after lifecycle
/// materialization, including controller projections and hydrated runner bytes.
pub fn canonical_recoverable_patch_artifacts(
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionOptions,
) -> Result<CanonicalRecoverablePatchArtifacts> {
    let mut candidates = outcome
        .artifacts
        .iter()
        .filter(|artifact| {
            options
                .artifact_id
                .as_deref()
                .is_none_or(|id| artifact.id == id)
        })
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.id.cmp(&right.id));

    let mut canonical = Vec::<(String, AgentTaskArtifact)>::new();
    let mut unavailable = Vec::new();
    for mut artifact in candidates {
        let Some(kind) = canonical_patch_kind(&artifact.kind) else {
            continue;
        };
        artifact.kind = kind.to_string();
        let path = match resolve_artifact_path(
            &artifact,
            &outcome.task_id,
            options.source_run_id.as_deref(),
            options.source_path.as_deref(),
        ) {
            Ok(path) => path,
            Err(error) => {
                unavailable.push(json!({ "id": artifact.id, "reason": error.message }));
                continue;
            }
        };
        let patch = match std::fs::read_to_string(&path) {
            Ok(patch) => patch,
            Err(error) => {
                unavailable
                    .push(json!({ "id": artifact.id, "path": path, "reason": error.to_string() }));
                continue;
            }
        };
        if let Err(error) = validate_artifact_content(&artifact, &patch) {
            unavailable.push(json!({ "id": artifact.id, "path": path, "reason": error.message }));
            continue;
        }
        let normalized = match normalize_promotion_patch(&patch, &options.to_worktree) {
            Ok(patch) => patch,
            Err(error) => {
                unavailable
                    .push(json!({ "id": artifact.id, "path": path, "reason": error.message }));
                continue;
            }
        };
        let digest = format!("{:x}", Sha256::digest(normalized.content.as_bytes()));
        let identity = canonical_patch_identity_with_digest(&artifact, &digest);
        if !canonical.iter().any(|(existing, _)| existing == &identity) {
            canonical.push((identity, artifact));
        }
    }

    Ok(CanonicalRecoverablePatchArtifacts {
        artifacts: canonical
            .into_iter()
            .map(|(_, artifact)| artifact)
            .collect(),
        unavailable,
    })
}

fn canonical_patch_identity(artifact: &AgentTaskArtifact) -> String {
    format!(
        "{}|{}",
        artifact.sha256.as_deref().unwrap_or("unavailable"),
        canonical_patch_provenance(artifact)
    )
}

fn canonical_patch_identity_with_digest(artifact: &AgentTaskArtifact, digest: &str) -> String {
    format!("{digest}|{}", canonical_patch_provenance(artifact))
}

fn canonical_patch_provenance(artifact: &AgentTaskArtifact) -> String {
    [
        "run_id",
        "task_id",
        "producer_attempt",
        "base_ref",
        "provider_backend",
        "repository_identity",
        "workspace_identity",
    ]
    .iter()
    .map(|key| {
        artifact
            .metadata
            .get(*key)
            .cloned()
            .unwrap_or(Value::Null)
            .to_string()
    })
    .collect::<Vec<_>>()
    .join("|")
}

fn canonical_patch_kind(kind: &str) -> Option<&'static str> {
    is_patch_artifact_kind(kind).then_some("patch")
}

fn resolve_artifact_path(
    artifact: &AgentTaskArtifact,
    task_id: &str,
    source_run_id: Option<&str>,
    source_path: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(run_id) = source_run_id {
        if let Some(projected) =
            crate::agent_task_lifecycle::verified_controller_artifact_projection_path(
                run_id, task_id, artifact,
            )?
        {
            return Ok(projected);
        }
    }
    let path = artifact.path.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact.path",
            "promotion patch artifact must provide a local path or a verified controller-side artifact projection",
            None,
            None,
        )
    })?;
    let path = PathBuf::from(path);
    if path.is_absolute() {
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
