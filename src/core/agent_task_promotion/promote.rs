use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

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
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

pub fn promote(options: AgentTaskPromotionOptions) -> Result<AgentTaskPromotionReport> {
    let mut provider = ExternalPromotionWorkspaceProvider::from_options(&options);
    promote_with_provider(options, &mut provider)
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

    if outcome.status != AgentTaskOutcomeStatus::Succeeded {
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "promotion requires a succeeded outcome; task {} has status {:?}",
                outcome.task_id, outcome.status
            ),
            None,
            None,
        ));
    }

    let artifact = select_patch_artifact(&outcome, options.artifact_id.as_deref())?;
    let patch_path = resolve_artifact_path(&artifact, options.source_path.as_deref())?;
    let patch = std::fs::read_to_string(&patch_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read patch artifact {}", patch_path.display())),
        )
    })?;
    validate_artifact_content(&artifact, &patch)?;
    if patch.trim().is_empty() {
        let status = AgentTaskPromotionStatus::NoChanges;
        let target = AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), None);
        let operator_notification = promotion_notification(status, &target);

        return Ok(AgentTaskPromotionReport {
            schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
            status,
            source: AgentTaskPromotionSource {
                kind: source_kind,
                task_id: outcome.task_id.clone(),
                run_id: options.source_run_id,
                path: options
                    .source_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            },
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
    if !options.dry_run {
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
            patch_path: provider_patch_path,
            changed_files: changed_files.clone(),
        })?;
        command_evidence.extend(target.command_evidence);
        applied_worktree_path = Some(target.path);
    }

    let mut deterministic_gates = Vec::new();
    let mut dependencies_materialized = false;
    if !options.dry_run
        && (!options.gates.verify.is_empty() || !options.gates.private_verify.is_empty())
    {
        let worktree_path = applied_worktree_path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "to_worktree",
                format!("managed worktree {} was not found", options.to_worktree),
                None,
                None,
            )
        })?;
        // Materialize dependencies via the component's resolved dependency
        // providers (install + build steps) in the freshly created worktree
        // before running the verify gates. Without this, a fresh worktree has
        // no installed dependency artifacts and gates fatal on missing
        // dependencies, masking the real pass/fail signal (#3771).
        dependencies_materialized =
            crate::core::hygiene::materialize_worktree_dependencies(Path::new(worktree_path))?;
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
    }
    let has_gate_failure = deterministic_gates
        .iter()
        .any(|gate| gate.status == AgentTaskGateStatus::Failed);
    let gate_results = deterministic_gates
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();

    let status = status_for_report(options.dry_run, has_gate_failure);
    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        applied_worktree_path.as_deref(),
    );
    let operator_notification = promotion_notification(status, &target);

    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status,
        source: AgentTaskPromotionSource {
            kind: source_kind,
            task_id: outcome.task_id.clone(),
            run_id: options.source_run_id,
            path: options
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
        },
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
        deterministic_gates,
        gate_results,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
            "worktree_path": applied_worktree_path,
            "dependencies_materialized": dependencies_materialized,
        }),
        operator_notification,
    })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedPromotionPatch {
    pub(crate) content: String,
    pub(crate) changed_files: Vec<String>,
}

pub(crate) fn normalize_promotion_patch(
    patch: &str,
    target_workspace: &str,
) -> Result<NormalizedPromotionPatch> {
    if patch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "patch",
            "promotion refuses an empty patch artifact",
            None,
            None,
        ));
    }

    let repo_slug = target_workspace_repo_slug(target_workspace);
    let mut changed_files = Vec::new();
    let mut normalized_lines = Vec::new();
    for line in patch.lines() {
        let normalized_line = normalize_patch_header_line(line, &repo_slug)?;
        if let Some(path) = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "))
        {
            let path = normalized_line
                .strip_prefix("+++ ")
                .or_else(|| normalized_line.strip_prefix("--- "))
                .unwrap_or(path)
                .trim();
            if path == "/dev/null" {
                normalized_lines.push(normalized_line);
                continue;
            }
            let path = path
                .strip_prefix("a/")
                .or_else(|| path.strip_prefix("b/"))
                .unwrap_or(path);
            validate_patch_path(path)?;
            if !changed_files.iter().any(|existing| existing == path) {
                changed_files.push(path.to_string());
            }
        }
        normalized_lines.push(normalized_line);
    }

    if changed_files.is_empty() {
        return Err(Error::validation_invalid_argument(
            "patch",
            "promotion requires a unified diff with changed file headers",
            None,
            None,
        ));
    }

    let mut content = normalized_lines.join("\n");
    if patch.ends_with('\n') {
        content.push('\n');
    }

    Ok(NormalizedPromotionPatch {
        content,
        changed_files,
    })
}

fn write_normalized_patch(content: &str) -> Result<NamedTempFile> {
    let mut file = NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create normalized promotion patch".to_string()),
        )
    })?;
    file.write_all(content.as_bytes()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "write normalized promotion patch {}",
                file.path().display()
            )),
        )
    })?;
    Ok(file)
}

fn normalize_patch_header_line(line: &str, repo_slug: &str) -> Result<String> {
    if let Some(rest) = line.strip_prefix("diff --git ") {
        let mut parts = rest.split_whitespace();
        let Some(old_path) = parts.next() else {
            return Ok(line.to_string());
        };
        let Some(new_path) = parts.next() else {
            return Ok(line.to_string());
        };
        if parts.next().is_some() {
            return Ok(line.to_string());
        }
        return Ok(format!(
            "diff --git {} {}",
            normalize_prefixed_diff_path(old_path, repo_slug)?,
            normalize_prefixed_diff_path(new_path, repo_slug)?
        ));
    }

    for prefix in ["--- ", "+++ "] {
        if let Some(path) = line.strip_prefix(prefix) {
            return Ok(format!(
                "{prefix}{}",
                normalize_prefixed_diff_path(path.trim(), repo_slug)?
            ));
        }
    }

    for prefix in ["rename from ", "rename to ", "copy from ", "copy to "] {
        if let Some(path) = line.strip_prefix(prefix) {
            return Ok(format!(
                "{prefix}{}",
                normalize_sandbox_path(path.trim(), repo_slug)?
            ));
        }
    }

    Ok(line.to_string())
}

fn normalize_prefixed_diff_path(path: &str, repo_slug: &str) -> Result<String> {
    if path == "/dev/null" {
        return Ok(path.to_string());
    }
    if let Some(path) = path.strip_prefix("a/") {
        return Ok(format!("a/{}", normalize_sandbox_path(path, repo_slug)?));
    }
    if let Some(path) = path.strip_prefix("b/") {
        return Ok(format!("b/{}", normalize_sandbox_path(path, repo_slug)?));
    }
    normalize_sandbox_path(path, repo_slug)
}

fn normalize_sandbox_path(path: &str, repo_slug: &str) -> Result<String> {
    let Some(rest) = path.strip_prefix("workspace/") else {
        return Ok(path.to_string());
    };
    let Some((sandbox, repo_relative)) = rest.split_once('/') else {
        if sandbox_belongs_to_repo(rest, repo_slug) {
            return Err(Error::validation_invalid_argument(
                "patch",
                format!("Lab sandbox patch path has no repo-relative suffix: {path}"),
                None,
                Some(vec![
                    "Expected paths shaped like workspace/<sandbox-worktree>/<repo-relative-path>.".to_string(),
                    "Regenerate the patch from the repository root or include Lab workspace mapping metadata.".to_string(),
                ]),
            ));
        }
        return Ok(path.to_string());
    };
    if !sandbox_belongs_to_repo(sandbox, repo_slug) {
        return Ok(path.to_string());
    }
    validate_patch_path(repo_relative)?;
    Ok(repo_relative.to_string())
}

fn sandbox_belongs_to_repo(sandbox: &str, repo_slug: &str) -> bool {
    sandbox == repo_slug
        || sandbox
            .strip_prefix(repo_slug)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('@'))
}

fn target_workspace_repo_slug(handle: &str) -> String {
    handle
        .split('@')
        .next()
        .unwrap_or(handle)
        .trim()
        .to_string()
}

pub(crate) fn validate_artifact_content(artifact: &AgentTaskArtifact, patch: &str) -> Result<()> {
    if let Some(expected_size) = artifact.size_bytes {
        let actual_size = patch.len() as u64;
        if expected_size != actual_size {
            return Err(Error::validation_invalid_argument(
                "artifact.size_bytes",
                format!(
                    "patch artifact size mismatch: expected {expected_size} bytes, read {actual_size} bytes"
                ),
                Some(artifact.id.clone()),
                None,
            ));
        }
    }

    if let Some(expected_sha256) = artifact.sha256.as_deref() {
        let mut hasher = Sha256::new();
        hasher.update(patch.as_bytes());
        let actual_sha256 = format!("{:x}", hasher.finalize());
        if expected_sha256 != actual_sha256 {
            return Err(Error::validation_invalid_argument(
                "artifact.sha256",
                format!(
                    "patch artifact sha256 mismatch: expected {expected_sha256}, read {actual_sha256}"
                ),
                Some(artifact.id.clone()),
                None,
            ));
        }
    }

    Ok(())
}

fn validate_patch_path(path: &str) -> Result<()> {
    let invalid = path.starts_with('/')
        || path.starts_with("../")
        || path.contains("/../")
        || path == ".."
        || path.starts_with(".git/")
        || path.contains("/.git/");
    if invalid {
        return Err(Error::validation_invalid_argument(
            "patch",
            format!("promotion refuses unsafe patch path: {path}"),
            None,
            None,
        ));
    }
    Ok(())
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
