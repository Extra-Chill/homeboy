use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::core::agent_task_timeout_artifacts::is_actionable_patch_artifact;
use crate::core::{Error, Result};

pub const AGENT_TASK_PROMOTION_REPORT_SCHEMA: &str = "homeboy/agent-task-promotion-report/v1";
pub const AGENT_TASK_GATE_REPORT_SCHEMA: &str = "homeboy/agent-task-gate-report/v1";

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
    pub deterministic_gates: Vec<AgentTaskGateReport>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateReport {
    #[serde(default = "gate_report_schema")]
    pub schema: String,
    pub id: String,
    pub status: AgentTaskGateStatus,
    pub command: Vec<String>,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<AgentTaskGateFailureEvidence>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateFailureEvidence {
    pub summary: String,
    pub command: String,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout_tail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr_tail: String,
    pub agent_feedback: String,
}

pub fn promote(options: AgentTaskPromotionOptions) -> Result<AgentTaskPromotionReport> {
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
    let changed_files = validate_patch(&patch)?;

    let mut dmc_commands = Vec::new();
    if !options.dry_run {
        dmc_commands.append(&mut ensure_worktree(&options.to_worktree)?);
        dmc_commands.push(apply_patch_with_dmc(&options.to_worktree, &patch_path)?);
    }

    let mut deterministic_gates = Vec::new();
    if !options.dry_run && !options.verify.is_empty() {
        let worktree_path = dmc_worktree_path(&options.to_worktree)?;
        for (index, command) in options.verify.iter().enumerate() {
            deterministic_gates.push(run_gate_command(&worktree_path, index + 1, command)?);
        }
    }
    let has_gate_failure = deterministic_gates
        .iter()
        .any(|gate| gate.status == AgentTaskGateStatus::Failed);

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
        dmc_commands,
        deterministic_gates,
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
        }),
    })
}

fn promotion_report_schema() -> String {
    AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string()
}

fn gate_report_schema() -> String {
    AGENT_TASK_GATE_REPORT_SCHEMA.to_string()
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
    if dmc_worktree_path(handle).is_ok() {
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

fn dmc_worktree_path(handle: &str) -> Result<PathBuf> {
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
    rows.as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("handle").and_then(Value::as_str) == Some(handle))
        })
        .and_then(|row| row.get("path").and_then(Value::as_str))
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "to_worktree",
                format!("managed worktree {handle} was not found"),
                None,
                None,
            )
        })
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

fn run_gate_command(cwd: &Path, index: usize, command: &str) -> Result<AgentTaskGateReport> {
    let command_vec = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
    let mut process = Command::new(&command_vec[0]);
    process.args(&command_vec[1..]).current_dir(cwd);
    let output = process.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run deterministic gate {command}")),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = if output.status.success() {
        AgentTaskGateStatus::Succeeded
    } else {
        AgentTaskGateStatus::Failed
    };
    let failure_evidence = (status == AgentTaskGateStatus::Failed)
        .then(|| gate_failure_evidence(command, exit_code, &stdout, &stderr));

    Ok(AgentTaskGateReport {
        schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
        id: format!("gate-{index}"),
        status,
        command: command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
    })
}

fn gate_failure_evidence(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> AgentTaskGateFailureEvidence {
    let stdout_tail = text_tail(stdout, 20);
    let stderr_tail = text_tail(stderr, 20);
    let summary = format!("deterministic gate failed with exit code {exit_code}: {command}");
    let agent_feedback = format!(
        "A deterministic verification gate failed after the candidate patch was applied. Fix the code so `{command}` passes, using the captured stdout/stderr tails as the primary failure evidence."
    );

    AgentTaskGateFailureEvidence {
        summary,
        command: command.to_string(),
        exit_code,
        stdout_tail,
        stderr_tail,
        agent_feedback,
    }
}

fn text_tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
        AGENT_TASK_OUTCOME_SCHEMA,
    };

    #[test]
    fn validate_patch_extracts_safe_changed_files() {
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

        let files = validate_patch(patch).expect("valid patch");

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
        let patch_path = temp.path().join("changes.patch");
        std::fs::write(
            &patch_path,
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .expect("write patch");
        let source_path = temp.path().join("outcome.json");
        let source = serde_json::json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-1",
            "status": "succeeded",
            "artifacts": [{
                "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                "id": "patch",
                "kind": "patch",
                "path": "changes.patch"
            }]
        })
        .to_string();

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
        assert!(report.deterministic_gates.is_empty());
    }

    #[test]
    fn gate_command_reports_success_without_failure_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(temp.path(), 1, "printf 'ok'").expect("gate report");

        assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
        assert_eq!(report.id, "gate-1");
        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.stdout, "ok");
        assert!(report.failure_evidence.is_none());
    }

    #[test]
    fn gate_command_normalizes_failure_for_agent_feedback() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(
            temp.path(),
            2,
            "printf 'line one\\nline two\\n'; printf 'boom\\n' >&2; exit 42",
        )
        .expect("gate report");
        let evidence = report.failure_evidence.expect("failure evidence");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.exit_code, 42);
        assert_eq!(
            evidence.command,
            "printf 'line one\\nline two\\n'; printf 'boom\\n' >&2; exit 42"
        );
        assert_eq!(evidence.stdout_tail, "line one\nline two");
        assert_eq!(evidence.stderr_tail, "boom");
        assert!(evidence.summary.contains("deterministic gate failed"));
        assert!(evidence.agent_feedback.contains("Fix the code"));
    }
}
