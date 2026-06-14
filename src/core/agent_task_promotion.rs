use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_gate::{
    run_gate_command, run_gate_command_with_policy, AgentTaskGateReport, AgentTaskGateRevealPolicy,
    AgentTaskGateStatus, AgentTaskGateVisibility,
};
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::core::agent_task_timeout_artifacts::is_actionable_patch_artifact;
use crate::core::gate::HomeboyGateResult;
use crate::core::{Error, Result};

pub const AGENT_TASK_PROMOTION_REPORT_SCHEMA: &str = "homeboy/agent-task-promotion-report/v1";
const PROMOTION_PROVIDER_COMMAND_ENV: &str = "HOMEBOY_AGENT_TASK_PROMOTION_COMMAND";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionOptions {
    pub source: String,
    pub source_path: Option<PathBuf>,
    pub to_worktree: String,
    pub task_id: Option<String>,
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: Vec<String>,
    #[serde(default)]
    pub private_verify: Vec<String>,
    #[serde(default = "default_private_gate_reveal")]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPromotionReport {
    #[serde(default = "promotion_report_schema")]
    pub schema: String,
    pub status: AgentTaskPromotionStatus,
    pub source: AgentTaskPromotionSource,
    pub to_worktree: String,
    pub patch_artifact: AgentTaskPromotionArtifactRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_evidence: Vec<AgentTaskPromotionCommandReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deterministic_gates: Vec<AgentTaskGateReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<HomeboyGateResult>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provenance: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskPromotionStatus {
    DryRun,
    Applied,
    GateFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionSource {
    pub kind: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionArtifactRef {
    pub id: String,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionCommandReport {
    pub command: Vec<String>,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

pub fn promote(options: AgentTaskPromotionOptions) -> Result<AgentTaskPromotionReport> {
    let mut provider = ExternalPromotionWorkspaceProvider::from_options(&options);
    promote_with_provider(options, &mut provider)
}

fn promote_with_provider(
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
    if !options.dry_run && (!options.verify.is_empty() || !options.private_verify.is_empty()) {
        let worktree_path = applied_worktree_path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "to_worktree",
                format!("managed worktree {} was not found", options.to_worktree),
                None,
                None,
            )
        })?;
        for (index, command) in options.verify.iter().enumerate() {
            deterministic_gates.push(provider.verify(
                worktree_path,
                index + 1,
                command,
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            )?);
        }
        let private_offset = deterministic_gates.len();
        for (index, command) in options.private_verify.iter().enumerate() {
            deterministic_gates.push(provider.verify(
                worktree_path,
                private_offset + index + 1,
                command,
                AgentTaskGateVisibility::Private,
                options.private_gate_reveal,
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

    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: if options.dry_run {
            AgentTaskPromotionStatus::DryRun
        } else if has_gate_failure {
            AgentTaskPromotionStatus::GateFailed
        } else {
            AgentTaskPromotionStatus::Applied
        },
        source: AgentTaskPromotionSource {
            kind: source_kind,
            task_id: outcome.task_id.clone(),
            path: options
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
        },
        to_worktree: options.to_worktree,
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
        }),
    })
}

const AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-request/v1";
const AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA: &str =
    "homeboy/agent-task-promotion-apply-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTaskPromotionApplyRequest {
    schema: String,
    to_workspace: String,
    patch_path: String,
    changed_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTaskPromotionApplyResponse {
    #[serde(default)]
    schema: String,
    workspace_path: String,
    #[serde(default)]
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTaskPromotionWorkspace {
    path: PathBuf,
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

trait AgentTaskPromotionWorkspaceProvider {
    fn apply_patch(
        &mut self,
        request: AgentTaskPromotionApplyRequest,
    ) -> Result<AgentTaskPromotionWorkspace>;
    fn verify(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
    ) -> Result<AgentTaskGateReport>;
}

struct ExternalPromotionWorkspaceProvider {
    command: Option<String>,
}

impl ExternalPromotionWorkspaceProvider {
    fn from_options(options: &AgentTaskPromotionOptions) -> Self {
        Self {
            command: options
                .provider_command
                .clone()
                .or_else(|| std::env::var(PROMOTION_PROVIDER_COMMAND_ENV).ok()),
        }
    }
}

impl AgentTaskPromotionWorkspaceProvider for ExternalPromotionWorkspaceProvider {
    fn apply_patch(
        &mut self,
        request: AgentTaskPromotionApplyRequest,
    ) -> Result<AgentTaskPromotionWorkspace> {
        let command = self.command.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion_provider",
                format!(
                    "agent-task promotion requires a workspace provider command; pass --provider-command or set {PROMOTION_PROVIDER_COMMAND_ENV}"
                ),
                None,
                None,
            )
        })?;
        run_provider_command(command, &request)
    }

    fn verify(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
    ) -> Result<AgentTaskGateReport> {
        if visibility == AgentTaskGateVisibility::Visible
            && reveal_policy == AgentTaskGateRevealPolicy::FullEvidence
        {
            return run_gate_command(cwd, index, command);
        }

        run_gate_command_with_policy(cwd, index, command, visibility, reveal_policy)
    }
}

fn promotion_report_schema() -> String {
    AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string()
}

fn default_private_gate_reveal() -> AgentTaskGateRevealPolicy {
    AgentTaskGateRevealPolicy::SummaryOnly
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

fn select_patch_artifact(
    outcome: &AgentTaskOutcome,
    artifact_id: Option<&str>,
) -> Result<AgentTaskArtifact> {
    let artifacts: Vec<AgentTaskArtifact> = outcome
        .artifacts
        .iter()
        .filter(|artifact| artifact_id.is_none_or(|expected| artifact.id == expected))
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect();

    match artifacts.len() {
        1 => Ok(artifacts.into_iter().next().unwrap()),
        0 => Err(Error::validation_invalid_argument(
            "artifact_id",
            "no matching non-empty patch artifact was found; inspect the agent result or transcript for diagnosis",
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
struct NormalizedPromotionPatch {
    content: String,
    changed_files: Vec<String>,
}

fn normalize_promotion_patch(
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

fn validate_artifact_content(artifact: &AgentTaskArtifact, patch: &str) -> Result<()> {
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

fn run_provider_command(
    command: &str,
    request: &AgentTaskPromotionApplyRequest,
) -> Result<AgentTaskPromotionWorkspace> {
    let request_json = serde_json::to_vec(request).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion provider request".to_string()),
            None,
        )
    })?;
    let mut process = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("start agent-task promotion provider command".to_string()),
            )
        })?;
    process
        .stdin
        .as_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "provider command stdin was not available".to_string(),
                Some("write agent-task promotion provider request".to_string()),
            )
        })?
        .write_all(&request_json)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write agent-task promotion provider request".to_string()),
            )
        })?;
    let output = process.wait_with_output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("run agent-task promotion provider command".to_string()),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let report = AgentTaskPromotionCommandReport {
        command: vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "command",
            format!(
                "promotion provider command failed with exit code {}: {}",
                exit_code,
                report.command.join(" ")
            ),
            None,
            Some(vec![report.stderr.clone()]),
        ));
    }
    let response: AgentTaskPromotionApplyResponse =
        serde_json::from_str(&report.stdout).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task promotion provider response".to_string()),
                Some(report.stdout.clone()),
            )
        })?;
    if !response.schema.is_empty() && response.schema != AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA
    {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.response.schema",
            format!(
                "expected {}, got {}",
                AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA, response.schema
            ),
            None,
            None,
        ));
    }
    if response.workspace_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "promotion_provider.response.workspace_path",
            "promotion provider response must include a workspace_path",
            None,
            None,
        ));
    }

    let mut command_evidence = response.command_evidence;
    if command_evidence.is_empty() {
        command_evidence.push(report);
    }
    Ok(AgentTaskPromotionWorkspace {
        path: PathBuf::from(response.workspace_path),
        command_evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
        AGENT_TASK_OUTCOME_SCHEMA,
    };

    const VALID_PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

    #[derive(Debug, Default)]
    struct FakePromotionWorkspaceProvider {
        workspace_path: Option<PathBuf>,
        apply_calls: Vec<AgentTaskPromotionApplyRequest>,
        applied_patch_contents: Vec<String>,
        verify_calls: Vec<(
            PathBuf,
            String,
            AgentTaskGateVisibility,
            AgentTaskGateRevealPolicy,
        )>,
    }

    impl AgentTaskPromotionWorkspaceProvider for FakePromotionWorkspaceProvider {
        fn apply_patch(
            &mut self,
            request: AgentTaskPromotionApplyRequest,
        ) -> Result<AgentTaskPromotionWorkspace> {
            self.applied_patch_contents.push(
                std::fs::read_to_string(&request.patch_path).unwrap_or_else(|_| String::new()),
            );
            self.apply_calls.push(request.clone());
            let path = self.workspace_path.clone().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "to_worktree",
                    "fake workspace provider could not resolve the requested workspace",
                    None,
                    None,
                )
            })?;
            Ok(AgentTaskPromotionWorkspace {
                path,
                command_evidence: vec![command_report(vec![
                    "fake-workspace-provider",
                    "apply-patch",
                    request.to_workspace.as_str(),
                ])],
            })
        }

        fn verify(
            &mut self,
            cwd: &Path,
            index: usize,
            command: &str,
            visibility: AgentTaskGateVisibility,
            reveal_policy: AgentTaskGateRevealPolicy,
        ) -> Result<AgentTaskGateReport> {
            self.verify_calls.push((
                cwd.to_path_buf(),
                command.to_string(),
                visibility,
                reveal_policy,
            ));
            Ok(AgentTaskGateReport::new(
                format!("gate-{index}"),
                vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
                0,
                String::new(),
                String::new(),
                None,
                visibility,
                reveal_policy,
                crate::core::agent_task_gate::AgentTaskGateEnvironment::default(),
            ))
        }
    }

    fn command_report(parts: Vec<&str>) -> AgentTaskPromotionCommandReport {
        AgentTaskPromotionCommandReport {
            command: parts.into_iter().map(str::to_string).collect(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn sha256_hex(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn write_patch_source(temp: &tempfile::TempDir) -> (PathBuf, String) {
        let patch_path = temp.path().join("changes.patch");
        std::fs::write(&patch_path, VALID_PATCH).expect("write patch");
        let source_path = temp.path().join("outcome.json");
        let source = serde_json::json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-1",
            "status": "succeeded",
            "artifacts": [{
                "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                "id": "patch",
                "kind": "patch",
                "path": "changes.patch",
                "size_bytes": VALID_PATCH.len(),
                "sha256": sha256_hex(VALID_PATCH)
            }]
        })
        .to_string();
        (source_path, source)
    }

    #[test]
    fn validate_patch_extracts_safe_changed_files() {
        let patch =
            normalize_promotion_patch(VALID_PATCH, "repo@promoted-task").expect("valid patch");

        assert_eq!(patch.changed_files, vec!["src/lib.rs"]);
        assert_eq!(patch.content, VALID_PATCH);
    }

    #[test]
    fn normalize_promotion_patch_strips_lab_sandbox_workspace_prefix() {
        let patch = "diff --git a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n--- a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n+++ b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

        let normalized = normalize_promotion_patch(patch, "homeboy@promoted-task")
            .expect("sandbox-prefixed patch normalizes");

        assert_eq!(normalized.changed_files, vec!["src/lib.rs"]);
        assert_eq!(
            normalized.content,
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
        );
    }

    #[test]
    fn normalize_promotion_patch_leaves_unrelated_workspace_paths() {
        let patch = "diff --git a/workspace/fixture.txt b/workspace/fixture.txt\n--- a/workspace/fixture.txt\n+++ b/workspace/fixture.txt\n@@ -1 +1 @@\n-old\n+new\n";

        let normalized = normalize_promotion_patch(patch, "homeboy@promoted-task")
            .expect("unrelated workspace path remains repo-relative");

        assert_eq!(normalized.changed_files, vec!["workspace/fixture.txt"]);
        assert_eq!(normalized.content, patch);
    }

    #[test]
    fn validate_patch_rejects_empty_patch() {
        let err = normalize_promotion_patch("\n\t", "repo@promoted-task")
            .expect_err("empty patch rejected");

        assert!(err.message.contains("empty patch"));
    }

    #[test]
    fn select_patch_artifact_rejects_empty_patch_metadata() {
        let outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: Some("patch.diff".to_string()),
                path: Some("patch.diff".to_string()),
                url: None,
                mime: Some("text/x-patch".to_string()),
                size_bytes: Some(0),
                sha256: Some(
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
                ),
                metadata: serde_json::json!({ "role": "patch" }),
            }],
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        let err = select_patch_artifact(&outcome, None).expect_err("empty patch rejected");

        assert!(err.message.contains("non-empty patch artifact"));
        assert!(err.message.contains("agent result or transcript"));
    }

    #[test]
    fn validate_patch_rejects_path_traversal() {
        let patch = "--- a/src/lib.rs\n+++ b/../secret\n@@ -1 +1 @@\n-old\n+new\n";

        let err = normalize_promotion_patch(patch, "repo@promoted-task")
            .expect_err("unsafe path rejected");

        assert!(err.message.contains("unsafe patch path"));
    }

    #[test]
    fn normalize_promotion_patch_rejects_repo_sandbox_without_relative_suffix() {
        let patch = "diff --git a/workspace/homeboy-refactor b/workspace/homeboy-refactor\n--- a/workspace/homeboy-refactor\n+++ b/workspace/homeboy-refactor\n@@ -1 +1 @@\n-old\n+new\n";

        let err = normalize_promotion_patch(patch, "homeboy@promoted-task")
            .expect_err("repo sandbox path without suffix rejected");

        assert!(err.message.contains("no repo-relative suffix"));
    }

    #[test]
    fn validate_artifact_content_rejects_sha_mismatch() {
        let artifact = AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            path: Some("changes.patch".to_string()),
            url: None,
            mime: None,
            size_bytes: Some(VALID_PATCH.len() as u64),
            sha256: Some("0".repeat(64)),
            metadata: Value::Null,
        };

        let err = validate_artifact_content(&artifact, VALID_PATCH).expect_err("sha rejected");

        assert!(err.message.contains("sha256 mismatch"));
    }

    #[test]
    fn select_patch_artifact_requires_unambiguous_patch() {
        let outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch-a".to_string(),
                    kind: "patch".to_string(),
                    name: None,
                    path: Some("a.patch".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                },
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch-b".to_string(),
                    kind: "diff".to_string(),
                    name: None,
                    path: Some("b.patch".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                },
            ],
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        let err = select_patch_artifact(&outcome, None).expect_err("ambiguous patch rejected");
        assert!(err.message.contains("multiple patch artifacts"));

        let artifact = select_patch_artifact(&outcome, Some("patch-b")).expect("selected patch");
        assert_eq!(artifact.id, "patch-b");
    }

    #[test]
    fn promote_dry_run_reports_selected_patch_without_provider_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source_path, source) = write_patch_source(&temp);

        let report = promote(AgentTaskPromotionOptions {
            source,
            source_path: Some(source_path),
            to_worktree: "repo@promoted-task".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: true,
            verify: Vec::new(),
            private_verify: Vec::new(),
            private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            provider_command: None,
        })
        .expect("dry-run promotion report");

        assert_eq!(report.status, AgentTaskPromotionStatus::DryRun);
        assert_eq!(report.source.task_id, "task-1");
        assert_eq!(report.patch_artifact.id, "patch");
        assert_eq!(report.changed_files, vec!["src/lib.rs"]);
        assert!(report.command_evidence.is_empty());
        assert!(report.deterministic_gates.is_empty());
    }

    #[test]
    fn promote_applies_patch_with_fake_workspace_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree_path = temp.path().join("controlled-worktree");
        let (source_path, source) = write_patch_source(&temp);
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(worktree_path.clone()),
            ..Default::default()
        };

        let report = promote_with_provider(
            AgentTaskPromotionOptions {
                source,
                source_path: Some(source_path),
                to_worktree: "repo@controlled-worktree".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                verify: vec!["cargo test --lib agent_task_promotion".to_string()],
                private_verify: vec!["cargo test --lib hidden".to_string()],
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                provider_command: None,
            },
            &mut provider,
        )
        .expect("applied promotion report");

        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(report.changed_files, vec!["src/lib.rs"]);
        assert_eq!(
            report.provenance["worktree_path"].as_str(),
            Some(worktree_path.to_str().expect("utf-8 temp path"))
        );
        assert_eq!(provider.apply_calls.len(), 1);
        assert_eq!(
            provider.apply_calls[0].to_workspace,
            "repo@controlled-worktree"
        );
        assert!(provider.apply_calls[0]
            .patch_path
            .ends_with("changes.patch"));
        assert_eq!(provider.apply_calls[0].changed_files, vec!["src/lib.rs"]);
        assert_eq!(
            provider.verify_calls,
            vec![
                (
                    worktree_path.clone(),
                    "cargo test --lib agent_task_promotion".to_string(),
                    AgentTaskGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                ),
                (
                    worktree_path,
                    "cargo test --lib hidden".to_string(),
                    AgentTaskGateVisibility::Private,
                    AgentTaskGateRevealPolicy::SummaryOnly,
                )
            ]
        );
        assert_eq!(report.command_evidence.len(), 1);
        assert_eq!(
            report.command_evidence[0].command[0],
            "fake-workspace-provider"
        );
        assert_eq!(report.deterministic_gates.len(), 2);
        assert_eq!(report.deterministic_gates[0].id, "gate-1");
        assert_eq!(
            report.deterministic_gates[1].visibility,
            AgentTaskGateVisibility::Private
        );
    }

    #[test]
    fn promote_applies_normalized_lab_sandbox_patch_with_fake_workspace_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let patch = "diff --git a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n--- a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n+++ b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let patch_path = temp.path().join("changes.patch");
        std::fs::write(&patch_path, patch).expect("write patch");
        let source_path = temp.path().join("outcome.json");
        let source = serde_json::json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-1",
            "status": "succeeded",
            "artifacts": [{
                "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                "id": "patch",
                "kind": "patch",
                "path": "changes.patch",
                "size_bytes": patch.len(),
                "sha256": sha256_hex(patch)
            }]
        })
        .to_string();
        std::fs::write(&source_path, &source).expect("write source");
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(temp.path().join("worktree")),
            ..Default::default()
        };

        let report = promote_with_provider(
            AgentTaskPromotionOptions {
                source,
                source_path: Some(source_path),
                to_worktree: "homeboy@promoted-task".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                verify: Vec::new(),
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                provider_command: None,
            },
            &mut provider,
        )
        .expect("promote applies normalized patch");

        assert_eq!(report.changed_files, vec!["src/lib.rs"]);
        assert_eq!(provider.apply_calls[0].changed_files, vec!["src/lib.rs"]);
        assert_ne!(
            provider.apply_calls[0].patch_path,
            patch_path.display().to_string()
        );
        let provider_patch = &provider.applied_patch_contents[0];
        assert!(provider_patch.contains("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(!provider_patch.contains("workspace/homeboy-refactor-command-contract-boundaries"));
    }

    #[test]
    fn promote_requires_provider_for_apply() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source_path, source) = write_patch_source(&temp);

        let err = promote(AgentTaskPromotionOptions {
            source,
            source_path: Some(source_path),
            to_worktree: "repo@controlled-worktree".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            verify: Vec::new(),
            private_verify: Vec::new(),
            private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            provider_command: None,
        })
        .expect_err("missing provider rejected");

        assert!(err.message.contains("workspace provider command"));
    }

    #[test]
    fn promotion_report_serializes_generic_command_evidence() {
        let report = AgentTaskPromotionReport {
            schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
            status: AgentTaskPromotionStatus::Applied,
            source: AgentTaskPromotionSource {
                kind: "outcome".to_string(),
                task_id: "task-1".to_string(),
                path: None,
            },
            to_worktree: "repo@controlled-worktree".to_string(),
            patch_artifact: AgentTaskPromotionArtifactRef {
                id: "patch".to_string(),
                kind: "patch".to_string(),
                path: "changes.patch".to_string(),
                sha256: None,
            },
            changed_files: vec!["src/lib.rs".to_string()],
            command_evidence: vec![command_report(vec![
                "fake-workspace-provider",
                "apply-patch",
            ])],
            deterministic_gates: Vec::new(),
            gate_results: Vec::new(),
            provenance: Value::Null,
        };

        let value = serde_json::to_value(report).expect("serialize report");

        assert_eq!(
            value["command_evidence"][0]["command"][0].as_str(),
            Some("fake-workspace-provider")
        );
    }

    #[test]
    fn provider_command_response_supplies_workspace_and_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let response_path = temp.path().join("response.json");
        std::fs::write(
            &response_path,
            serde_json::json!({
                "schema": AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
                "workspace_path": temp.path().join("workspace").display().to_string(),
                "command_evidence": [{
                    "command": ["provider", "apply"],
                    "exit_code": 0
                }]
            })
            .to_string(),
        )
        .expect("write response");

        let request = AgentTaskPromotionApplyRequest {
            schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
            to_workspace: "target-workspace".to_string(),
            patch_path: temp.path().join("changes.patch").display().to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
        };
        let workspace = run_provider_command(&format!("cat {}", response_path.display()), &request)
            .expect("provider response");

        assert!(workspace.path.ends_with("workspace"));
        assert_eq!(
            workspace.command_evidence[0].command,
            vec!["provider", "apply"]
        );
    }
}
