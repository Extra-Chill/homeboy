use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::agent_task_gate::{
    AgentTaskGateReport, AgentTaskGateRevealPolicy, AgentTaskGateVisibility,
};
use homeboy_core::git::output_allow_empty;
use homeboy_core::worktree_provider;
use homeboy_core::{Error, Result};

use super::types::AgentTaskPromotionCommandReport;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrustedUnpushedCandidateDestination {
    pub(crate) path: PathBuf,
    pub(crate) head: String,
}

pub(crate) const AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-request/v1";
pub(crate) const AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-response/v1";

/// Confirm that the controller can promote into a validated direct Git
/// workspace or an active native managed workspace before it spends a provider
/// attempt.
pub fn preflight_managed_workspace(to_workspace: &str) -> Result<()> {
    if Path::new(to_workspace).is_dir() {
        return validate_provider_workspace_path(Path::new(to_workspace));
    }
    worktree_provider::resolve_native_worktree_mutation_target(to_workspace)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            "promotion requires an active native Homeboy worktree",
            Some(to_workspace.to_string()),
            None,
        )
    })?;
    Ok(())
}

/// Apply a promotion-provider request to an already materialized Git workspace.
///
/// Lab uses this adapter after it has safely staged the target workspace on the
/// runner, so promotion stays within the runner without requiring a controller
/// workspace provider.
pub fn apply_materialized_workspace_patch(workspace: &Path, request_json: &str) -> Result<String> {
    let request: AgentTaskPromotionApplyRequest =
        serde_json::from_str(request_json).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task promotion provider request".to_string()),
                None,
            )
        })?;
    if request.schema != AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.request.schema",
            format!(
                "expected {}, got {}",
                AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, request.schema
            ),
            None,
            None,
        ));
    }
    validate_provider_workspace_path(workspace)?;
    // The provider can run in a different materialized workspace from the
    // producer, so prefer the patch carried through the stdin request.
    let inline_patch = request
        .patch
        .as_deref()
        .map(|patch| {
            let mut file = NamedTempFile::new().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("create inline agent-task promotion patch".to_string()),
                )
            })?;
            file.write_all(patch.as_bytes()).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("write inline agent-task promotion patch".to_string()),
                )
            })?;
            Ok::<_, Error>(file)
        })
        .transpose()?;
    let patch_path = inline_patch
        .as_ref()
        .map(|file| file.path().to_string_lossy().into_owned())
        .unwrap_or_else(|| request.patch_path.clone());
    if request
        .trusted_unpushed_candidate_destination
        .as_ref()
        .is_some_and(|trusted| trusted_candidate_destination_matches(workspace, trusted))
        && patch_is_already_applied(workspace, &patch_path)?
    {
        return serde_json::to_string(&AgentTaskPromotionApplyResponse {
            schema: AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA.to_string(),
            workspace_path: workspace.display().to_string(),
            command_evidence: Vec::new(),
        })
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize promotion provider response".to_string()),
            )
        });
    }
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["apply", "--whitespace=nowarn", &patch_path]);
    if request.dry_run {
        command.arg("--check");
    }
    let output = command.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("apply agent-task promotion patch".to_string()),
        )
    })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.patch",
            format!(
                "could not apply promotion patch in {}: {}",
                workspace.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(request.patch_path),
            None,
        ));
    }
    serde_json::to_string(&AgentTaskPromotionApplyResponse {
        schema: AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA.to_string(),
        workspace_path: workspace.display().to_string(),
        command_evidence: Vec::new(),
    })
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize promotion provider response".to_string()),
        )
    })
}

fn trusted_candidate_destination_matches(
    workspace: &Path,
    trusted: &TrustedUnpushedCandidateDestination,
) -> bool {
    let Ok(workspace) = std::fs::canonicalize(workspace) else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(&trusted.path) else {
        return false;
    };
    workspace == path
        && Command::new("git")
            .args([
                "-C",
                workspace.to_str().unwrap_or_default(),
                "rev-parse",
                "--verify",
                "HEAD^{commit}",
            ])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .is_some_and(|output| String::from_utf8_lossy(&output.stdout).trim() == trusted.head)
}

fn patch_is_already_applied(workspace: &Path, patch_path: &str) -> Result<bool> {
    Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args([
            "apply",
            "--reverse",
            "--check",
            "--whitespace=nowarn",
            patch_path,
        ])
        .status()
        .map(|status| status.success())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("verify already-applied agent-task promotion patch".to_string()),
            )
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionApplyRequest {
    pub(crate) schema: String,
    pub(crate) to_workspace: String,
    /// Inline patch payload for providers that do not share artifact storage
    /// with the producer. `patch_path` remains provenance and a legacy fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) patch: Option<String>,
    pub(crate) patch_path: String,
    pub(crate) changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gate_feedback_baseline: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trusted_unpushed_candidate_destination: Option<TrustedUnpushedCandidateDestination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionApplyResponse {
    #[serde(default)]
    schema: String,
    workspace_path: String,
    #[serde(default)]
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentTaskPromotionWorkspace {
    pub(crate) path: PathBuf,
    pub(crate) command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

pub(crate) fn apply_patch(
    request: AgentTaskPromotionApplyRequest,
    materialized_workspace: Option<&Path>,
) -> Result<AgentTaskPromotionWorkspace> {
    let path = if let Some(path) = materialized_workspace {
        path.to_path_buf()
    } else if Path::new(&request.to_workspace).is_dir() {
        PathBuf::from(&request.to_workspace)
    } else {
        worktree_provider::resolve_native_worktree_mutation_target(&request.to_workspace)?
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "to_worktree",
                    "promotion requires an active native Homeboy worktree",
                    Some(request.to_workspace.clone()),
                    None,
                )
            })?
            .path
    };
    let response = apply_materialized_workspace_patch(
        &path,
        &serde_json::to_string(&request)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )?;
    let response: AgentTaskPromotionApplyResponse = serde_json::from_str(&response)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(AgentTaskPromotionWorkspace {
        path,
        command_evidence: response.command_evidence,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "gate execution requires explicit runtime isolation inputs"
)]
pub(crate) fn verify_with_runtime_tmpdir(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: &Path,
    gate_environment: &crate::agent_task_gate::AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[crate::agent_task_gate::AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    crate::agent_task_gate::run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        Some(runtime_tmpdir),
        gate_environment,
        package_artifacts,
    )
}

fn validate_provider_workspace_path(path: &Path) -> Result<()> {
    match output_allow_empty(path, &["rev-parse", "--is-inside-work-tree"]) {
        Some(value) if value == "true" => Ok(()),
        _ => Err(Error::validation_invalid_argument(
            "promotion_provider.response.workspace_path",
            format!(
                "promotion provider response workspace_path is not a git worktree: {}",
                path.display()
            ),
            None,
            None,
        )),
    }
}
