use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::core::agent_task_timeout_artifacts::is_actionable_patch_artifact;
use crate::core::{Error, Result};

pub const AGENT_TASK_PROMOTION_REPORT_SCHEMA: &str = "homeboy/agent-task-promotion-report/v1";

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
    pub dmc_commands: Vec<AgentTaskPromotionCommandReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<AgentTaskPromotionCommandReport>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provenance: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskPromotionStatus {
    DryRun,
    Applied,
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
    promote_with_runner(options, &mut DmcPromotionRunner)
}

fn promote_with_runner(
    options: AgentTaskPromotionOptions,
    runner: &mut impl AgentTaskPromotionRunner,
) -> Result<AgentTaskPromotionReport> {
    validate_worktree_handle(&options.to_worktree)?;
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
    let changed_files = validate_patch(&patch)?;

    let mut dmc_commands = Vec::new();
    let mut applied_worktree_path = None;
    if !options.dry_run {
        let worktree_path = match runner.worktree_path(&options.to_worktree)? {
            Some(path) => path,
            None => {
                dmc_commands.append(&mut runner.ensure_worktree(&options.to_worktree)?);
                runner.worktree_path(&options.to_worktree)?.ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "to_worktree",
                        format!(
                            "managed worktree {} was not found after creation",
                            options.to_worktree
                        ),
                        None,
                        None,
                    )
                })?
            }
        };
        dmc_commands.push(runner.apply_patch(&options.to_worktree, &patch_path)?);
        applied_worktree_path = Some(worktree_path);
    }

    let mut verification = Vec::new();
    if !options.dry_run && !options.verify.is_empty() {
        let worktree_path = applied_worktree_path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "to_worktree",
                format!("managed worktree {} was not found", options.to_worktree),
                None,
                None,
            )
        })?;
        for command in &options.verify {
            verification.push(runner.verify(worktree_path, command)?);
        }
    }

    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: if options.dry_run {
            AgentTaskPromotionStatus::DryRun
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
        dmc_commands,
        verification,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
        }),
    })
}

trait AgentTaskPromotionRunner {
    fn worktree_path(&mut self, handle: &str) -> Result<Option<PathBuf>>;
    fn ensure_worktree(&mut self, handle: &str) -> Result<Vec<AgentTaskPromotionCommandReport>>;
    fn apply_patch(
        &mut self,
        handle: &str,
        patch_path: &Path,
    ) -> Result<AgentTaskPromotionCommandReport>;
    fn verify(&mut self, cwd: &Path, command: &str) -> Result<AgentTaskPromotionCommandReport>;
}

struct DmcPromotionRunner;

impl AgentTaskPromotionRunner for DmcPromotionRunner {
    fn worktree_path(&mut self, handle: &str) -> Result<Option<PathBuf>> {
        dmc_worktree_path(handle)
    }

    fn ensure_worktree(&mut self, handle: &str) -> Result<Vec<AgentTaskPromotionCommandReport>> {
        ensure_worktree(handle)
    }

    fn apply_patch(
        &mut self,
        handle: &str,
        patch_path: &Path,
    ) -> Result<AgentTaskPromotionCommandReport> {
        apply_patch_with_dmc(handle, patch_path)
    }

    fn verify(&mut self, cwd: &Path, command: &str) -> Result<AgentTaskPromotionCommandReport> {
        run_verification_command(cwd, command)
    }
}

fn promotion_report_schema() -> String {
    AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string()
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

fn validate_patch(patch: &str) -> Result<Vec<String>> {
    if patch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "patch",
            "promotion refuses an empty patch artifact",
            None,
            None,
        ));
    }

    let mut changed_files = Vec::new();
    for line in patch.lines() {
        if let Some(path) = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "))
        {
            let path = path.trim();
            if path == "/dev/null" {
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
    }

    if changed_files.is_empty() {
        return Err(Error::validation_invalid_argument(
            "patch",
            "promotion requires a unified diff with changed file headers",
            None,
            None,
        ));
    }

    Ok(changed_files)
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

fn validate_worktree_handle(handle: &str) -> Result<()> {
    let (repo, branch) = split_worktree_handle(handle)?;
    if repo.is_empty() || branch.is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "worktree handle must use <repo>@<branch-slug>",
            None,
            None,
        ));
    }
    Ok(())
}

fn split_worktree_handle(handle: &str) -> Result<(&str, &str)> {
    handle.split_once('@').ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            "worktree handle must use <repo>@<branch-slug>",
            None,
            None,
        )
    })
}

fn ensure_worktree(handle: &str) -> Result<Vec<AgentTaskPromotionCommandReport>> {
    if dmc_worktree_path(handle)?.is_some() {
        return Ok(Vec::new());
    }

    let (repo, branch) = split_worktree_handle(handle)?;
    let command = vec![
        "studio".to_string(),
        "wp".to_string(),
        "datamachine-code".to_string(),
        "workspace".to_string(),
        "worktree".to_string(),
        "add".to_string(),
        repo.to_string(),
        branch.to_string(),
    ];
    Ok(vec![run_command(command, None)?])
}

fn apply_patch_with_dmc(
    handle: &str,
    patch_path: &Path,
) -> Result<AgentTaskPromotionCommandReport> {
    run_command(
        vec![
            "studio".to_string(),
            "wp".to_string(),
            "datamachine-code".to_string(),
            "workspace".to_string(),
            "patch".to_string(),
            "apply".to_string(),
            handle.to_string(),
            format!("--patch=@{}", patch_path.display()),
            "--format=json".to_string(),
        ],
        None,
    )
}

fn dmc_worktree_path(handle: &str) -> Result<Option<PathBuf>> {
    let (repo, _) = split_worktree_handle(handle)?;
    let report = run_command(
        vec![
            "studio".to_string(),
            "wp".to_string(),
            "datamachine-code".to_string(),
            "workspace".to_string(),
            "worktree".to_string(),
            "list".to_string(),
            repo.to_string(),
            "--format=json".to_string(),
        ],
        None,
    )?;
    let rows: Value = serde_json::from_str(&report.stdout).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("datamachine-code worktree list output".to_string()),
            Some(report.stdout.clone()),
        )
    })?;
    let rows = rows.as_array().ok_or_else(|| {
        Error::validation_invalid_argument(
            "datamachine-code worktree list output",
            "expected a JSON array of worktree rows",
            None,
            Some(vec![report.stdout.clone()]),
        )
    })?;

    Ok(rows
        .iter()
        .find(|row| row.get("handle").and_then(Value::as_str) == Some(handle))
        .and_then(|row| row.get("path").and_then(Value::as_str))
        .map(PathBuf::from))
}

fn run_verification_command(cwd: &Path, command: &str) -> Result<AgentTaskPromotionCommandReport> {
    run_command(
        vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
        Some(cwd),
    )
}

fn run_command(
    command: Vec<String>,
    cwd: Option<&Path>,
) -> Result<AgentTaskPromotionCommandReport> {
    let mut process = Command::new(&command[0]);
    process.args(&command[1..]);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    let output = process.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run {}", command.join(" "))),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let report = AgentTaskPromotionCommandReport {
        command,
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "command",
            format!(
                "promotion command failed with exit code {}: {}",
                exit_code,
                report.command.join(" ")
            ),
            None,
            Some(vec![report.stderr.clone()]),
        ));
    }
    Ok(report)
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
    struct FakePromotionRunner {
        worktree_path: Option<PathBuf>,
        ensure_creates_worktree: bool,
        ensure_calls: Vec<String>,
        apply_calls: Vec<(String, PathBuf)>,
        verify_calls: Vec<(PathBuf, String)>,
    }

    impl AgentTaskPromotionRunner for FakePromotionRunner {
        fn worktree_path(&mut self, _handle: &str) -> Result<Option<PathBuf>> {
            Ok(self.worktree_path.clone())
        }

        fn ensure_worktree(
            &mut self,
            handle: &str,
        ) -> Result<Vec<AgentTaskPromotionCommandReport>> {
            self.ensure_calls.push(handle.to_string());
            if self.ensure_creates_worktree {
                self.worktree_path = Some(PathBuf::from("/tmp/homeboy-controlled-worktree"));
            }
            Ok(vec![command_report(vec![
                "studio",
                "wp",
                "datamachine-code",
                "workspace",
                "worktree",
                "add",
                "repo",
                "controlled-worktree",
            ])])
        }

        fn apply_patch(
            &mut self,
            handle: &str,
            patch_path: &Path,
        ) -> Result<AgentTaskPromotionCommandReport> {
            self.apply_calls
                .push((handle.to_string(), patch_path.to_path_buf()));
            Ok(command_report(vec![
                "studio",
                "wp",
                "datamachine-code",
                "workspace",
                "patch",
                "apply",
                handle,
            ]))
        }

        fn verify(&mut self, cwd: &Path, command: &str) -> Result<AgentTaskPromotionCommandReport> {
            self.verify_calls
                .push((cwd.to_path_buf(), command.to_string()));
            Ok(command_report(vec!["sh", "-lc", command]))
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
        let files = validate_patch(VALID_PATCH).expect("valid patch");

        assert_eq!(files, vec!["src/lib.rs"]);
    }

    #[test]
    fn validate_patch_rejects_empty_patch() {
        let err = validate_patch("\n\t").expect_err("empty patch rejected");

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

        let err = validate_patch(patch).expect_err("unsafe path rejected");

        assert!(err.message.contains("unsafe patch path"));
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
    fn promote_dry_run_reports_selected_patch_without_dmc_mutation() {
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
        })
        .expect("dry-run promotion report");

        assert_eq!(report.status, AgentTaskPromotionStatus::DryRun);
        assert_eq!(report.source.task_id, "task-1");
        assert_eq!(report.patch_artifact.id, "patch");
        assert_eq!(report.changed_files, vec!["src/lib.rs"]);
        assert!(report.dmc_commands.is_empty());
    }

    #[test]
    fn promote_applies_patch_into_existing_controlled_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree_path = temp.path().join("controlled-worktree");
        let (source_path, source) = write_patch_source(&temp);
        let mut runner = FakePromotionRunner {
            worktree_path: Some(worktree_path.clone()),
            ..Default::default()
        };

        let report = promote_with_runner(
            AgentTaskPromotionOptions {
                source,
                source_path: Some(source_path),
                to_worktree: "repo@controlled-worktree".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                verify: vec!["cargo test --lib agent_task_promotion".to_string()],
            },
            &mut runner,
        )
        .expect("applied promotion report");

        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(report.changed_files, vec!["src/lib.rs"]);
        assert!(runner.ensure_calls.is_empty());
        assert_eq!(runner.apply_calls.len(), 1);
        assert_eq!(runner.apply_calls[0].0, "repo@controlled-worktree");
        assert!(runner.apply_calls[0].1.ends_with("changes.patch"));
        assert_eq!(
            runner.verify_calls,
            vec![(
                worktree_path,
                "cargo test --lib agent_task_promotion".to_string()
            )]
        );
        assert_eq!(report.dmc_commands.len(), 1);
        assert_eq!(report.verification.len(), 1);
    }

    #[test]
    fn promote_creates_missing_worktree_before_apply() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source_path, source) = write_patch_source(&temp);
        let mut runner = FakePromotionRunner {
            ensure_creates_worktree: true,
            ..Default::default()
        };

        let report = promote_with_runner(
            AgentTaskPromotionOptions {
                source,
                source_path: Some(source_path),
                to_worktree: "repo@controlled-worktree".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                verify: Vec::new(),
            },
            &mut runner,
        )
        .expect("applied promotion report");

        assert_eq!(runner.ensure_calls, vec!["repo@controlled-worktree"]);
        assert_eq!(runner.apply_calls.len(), 1);
        assert_eq!(report.dmc_commands.len(), 2);
    }
}
