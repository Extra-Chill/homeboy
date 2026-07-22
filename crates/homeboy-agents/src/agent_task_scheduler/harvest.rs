use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::*;

/// Git pathspecs excluding Homeboy-owned runner workspace metadata from captured
/// candidate patches. The runner writes `.homeboy/runner-workspace.json` (and
/// scratch under `.homeboy/`) after the snapshot baseline is established, so it
/// is checkout drift rather than provider output — including it produces a patch
/// that deletes/rewrites runner metadata and cannot be promoted cleanly. (#8534)
pub(super) const RUNNER_METADATA_EXCLUDE_PATHSPECS: &[&str] = &[
    ":(exclude).homeboy/runner-workspace.json",
    ":(exclude).homeboy/lab-at-files/**",
];

pub(super) fn harvest_committed_patch(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
) -> Result<(), HarvestError> {
    harvest_committed_patch_with_metadata(outcome, running, committed_change_metadata_for_range)
}

pub(super) fn harvest_uncommitted_patch(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
) -> Result<(), HarvestError> {
    let Some(base) = running.task_base_sha.as_deref() else {
        return Ok(());
    };
    let Some(root) = running.request.workspace.root.as_deref().map(Path::new) else {
        return Ok(());
    };
    persist_attempt_patch_artifacts(outcome, running, root)?;
    if outcome.artifacts.iter().any(is_actionable_patch_artifact) {
        return Ok(());
    }
    // This checkout belongs solely to this dispatch. Staging all changes makes
    // Git's binary patch generation include tracked, staged, and untracked files.
    // Homeboy-owned runner metadata is excluded from the diff so it never lands
    // in a candidate patch as checkout drift. (#8534)
    git_output(
        root,
        &[
            "add",
            "--all",
            "--",
            ".",
            RUNNER_METADATA_EXCLUDE_PATHSPECS[0],
            RUNNER_METADATA_EXCLUDE_PATHSPECS[1],
        ],
    )?;
    let mut uncommitted_patch_args = vec![
        "diff",
        "--cached",
        "--binary",
        "--full-index",
        "--find-renames",
        "HEAD",
        "--",
        ".",
    ];
    uncommitted_patch_args.extend_from_slice(RUNNER_METADATA_EXCLUDE_PATHSPECS);
    let patch = git_output_raw(root, &uncommitted_patch_args)?;
    if patch.trim().is_empty() {
        return Ok(());
    }
    let mut uncommitted_changed_args = vec!["diff", "--cached", "--name-only", "HEAD", "--", "."];
    uncommitted_changed_args.extend_from_slice(RUNNER_METADATA_EXCLUDE_PATHSPECS);
    let changed_files = git_changed_files(root, &uncommitted_changed_args)?;
    let path = attempt_patch_path(running, "uncommitted")?;
    std::fs::write(&path, patch.as_bytes()).map_err(|error| HarvestError::ArtifactWrite {
        path: path.clone(),
        message: error.to_string(),
    })?;
    outcome.artifacts.push(AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: attempt_patch_id(running, "uncommitted-changes"),
        kind: "patch".to_string(),
        name: Some("uncommitted-changes.patch".to_string()),
        label: Some("executor uncommitted changes".to_string()),
        role: Some("patch".to_string()),
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(patch_sha256(&patch)),
        metadata: serde_json::json!({
            "change_source": "uncommitted_attempt_workspace",
            "base_ref": base,
            "run_id": running.run_id.as_deref(),
            "task_id": &running.task_id,
            "producer_attempt": running.attempt,
            "provider_rotation_index": running.rotation_index,
            "provider_backend": running.request.executor.backend,
            "provider_model": running.request.executor.model(),
            "attempt_workspace": running.request.workspace.attempt,
            "source_provenance": running.source_provenance,
            "changed_files": changed_files,
        }),
    });
    Ok(())
}

fn persist_attempt_patch_artifacts(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
    attempt_root: &Path,
) -> Result<(), HarvestError> {
    for (index, artifact) in outcome.artifacts.iter_mut().enumerate() {
        if !is_actionable_patch_artifact(artifact) {
            continue;
        }
        if !artifact.metadata.is_object() {
            artifact.metadata = serde_json::json!({});
        }
        let Some(source) = artifact.path.as_deref().map(PathBuf::from) else {
            patch_artifact_metadata(artifact, running, false);
            continue;
        };
        if !source.starts_with(attempt_root) {
            patch_artifact_metadata(artifact, running, false);
            continue;
        }
        let contents = std::fs::read(&source).map_err(|error| HarvestError::ArtifactWrite {
            path: source.clone(),
            message: error.to_string(),
        })?;
        let path = attempt_patch_path(running, &format!("provider-{index}"))?;
        std::fs::write(&path, &contents).map_err(|error| HarvestError::ArtifactWrite {
            path: path.clone(),
            message: error.to_string(),
        })?;
        artifact.path = Some(path.display().to_string());
        artifact.size_bytes = Some(contents.len() as u64);
        artifact.sha256 = Some(patch_sha256(&contents));
        patch_artifact_metadata(artifact, running, true);
    }
    Ok(())
}

fn patch_artifact_metadata(
    artifact: &mut AgentTaskArtifact,
    running: &RunningTask,
    copied_from_attempt_workspace: bool,
) {
    let metadata = artifact
        .metadata
        .as_object_mut()
        .expect("patch artifact metadata object");
    metadata.insert(
        "source_provenance".to_string(),
        serde_json::json!(running.source_provenance),
    );
    if copied_from_attempt_workspace {
        metadata.extend(serde_json::Map::from_iter([
            ("run_id".to_string(), serde_json::json!(running.run_id)),
            ("task_id".to_string(), serde_json::json!(running.task_id)),
            (
                "producer_attempt".to_string(),
                serde_json::json!(running.attempt),
            ),
            (
                "change_source".to_string(),
                serde_json::json!("attempt_workspace_artifact"),
            ),
            (
                "provider_rotation_index".to_string(),
                serde_json::json!(running.rotation_index),
            ),
            (
                "provider_backend".to_string(),
                serde_json::json!(running.request.executor.backend),
            ),
            (
                "provider_model".to_string(),
                serde_json::json!(running.request.executor.model()),
            ),
        ]));
    }
    attach_gate_feedback_baseline_metadata(&mut artifact.metadata, running);
}

fn harvest_committed_patch_with_metadata(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
    collect_metadata: impl FnOnce(&Path, &str) -> Result<Vec<serde_json::Value>, HarvestError>,
) -> Result<(), HarvestError> {
    if outcome.status != AgentTaskOutcomeStatus::Succeeded
        || outcome.artifacts.iter().any(|artifact| {
            is_actionable_patch_artifact(artifact) || is_empty_patch_artifact(artifact)
        })
    {
        return Ok(());
    }
    let Some(base) = running.task_base_sha.as_deref() else {
        return Ok(());
    };
    let Some(attempt_root) = running.request.workspace.root.as_deref().map(Path::new) else {
        return Ok(());
    };

    // Resolve the harvest root resiliently: prefer the attempt scratch workspace,
    // but fall back to the durable source workspace when the attempt directory has
    // been reaped (e.g. multi-attempt cook lifecycle). A missing or non-repo cwd
    // is not fatal — it just means the attempt scratch is unavailable.
    let attempt_is_repo = git_is_repository(attempt_root)?;
    let mut root = if attempt_is_repo {
        attempt_root
    } else if let Some(source_root) = running
        .source_workspace_root
        .as_deref()
        .map(Path::new)
        .filter(|source_root| source_root != &attempt_root)
    {
        if git_is_repository(source_root)? {
            source_root
        } else {
            return Ok(());
        }
    } else {
        return Ok(());
    };

    let mut head = git_output(root, &["rev-parse", "HEAD"])?;
    if head == base {
        // When the attempt root was valid but had no new commits, check the
        // durable source workspace for commits beyond base.
        if root == attempt_root {
            if let Some(source_root) = running
                .source_workspace_root
                .as_deref()
                .map(Path::new)
                .filter(|source_root| source_root != &attempt_root)
            {
                if git_is_repository(source_root)? {
                    let source_head = git_output(source_root, &["rev-parse", "HEAD"])?;
                    if source_head != base {
                        root = source_root;
                        head = source_head;
                    } else {
                        return Ok(());
                    }
                } else {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }
    if !git_is_ancestor(root, base, "HEAD")? {
        return Err(HarvestError::UnrelatedHead {
            base: base.to_string(),
            head,
        });
    }
    let mut committed_patch_args = vec![
        "diff",
        "--binary",
        "--full-index",
        "--find-renames",
        base,
        "HEAD",
        "--",
        ".",
    ];
    committed_patch_args.extend_from_slice(RUNNER_METADATA_EXCLUDE_PATHSPECS);
    let patch = git_output_raw(root, &committed_patch_args)?;
    if patch.trim().is_empty() {
        return Ok(());
    }
    let path = attempt_patch_path(running, &format!("committed-{head}"))?;
    std::fs::write(&path, patch.as_bytes()).map_err(|error| HarvestError::ArtifactWrite {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let range = format!("{base}..HEAD");
    // Retain the patch before collecting optional commit metadata so a later
    // Git failure cannot strand the recoverable artifact.
    outcome.artifacts.push(committed_patch_artifact(
        running,
        &path,
        &patch,
        base,
        &range,
        Vec::new(),
    ));
    let commits = collect_metadata(root, &range)?;
    let mut committed_changed_args = vec!["diff", "--name-only", base, "HEAD", "--", "."];
    committed_changed_args.extend_from_slice(RUNNER_METADATA_EXCLUDE_PATHSPECS);
    let changed_files = git_changed_files(root, &committed_changed_args)?;
    outcome
        .artifacts
        .last_mut()
        .expect("committed patch artifact was attached")
        .metadata = serde_json::json!({
        "change_source": "local_commits",
        "artifact_provenance": "homeboy_generated_committed_patch",
        "base_ref": base,
        "commit_range": range,
        "commits": commits,
        "run_id": running.run_id.as_deref(),
        "task_id": &running.task_id,
        "producer_attempt": running.attempt,
        "source_provenance": running.source_provenance,
        "provider_rotation_index": running.rotation_index,
        "provider_backend": running.request.executor.backend,
        "provider_model": running.request.executor.model(),
        "attempt_workspace": &running.request.workspace.attempt,
        "changed_files": changed_files,
    });
    Ok(())
}

fn attach_gate_feedback_baseline_metadata(metadata: &mut serde_json::Value, running: &RunningTask) {
    if running.request.metadata["cook_loop"]["kind"] == "deterministic-gate-feedback" {
        metadata["gate_feedback_baseline"] = running.request.inputs["cook_loop"].clone();
    }
}

fn committed_change_metadata_for_range(
    cwd: &Path,
    range: &str,
) -> Result<Vec<serde_json::Value>, HarvestError> {
    let output = git_output(
        cwd,
        &["log", "--reverse", "--format=%H%x1f%an%x1f%ae%x1f%s", range],
    )?;
    Ok(committed_change_metadata(&output))
}

fn git_changed_files(cwd: &Path, args: &[&str]) -> Result<Vec<String>, HarvestError> {
    Ok(git_output(cwd, args)?
        .lines()
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn committed_patch_artifact(
    running: &RunningTask,
    path: &Path,
    patch: &str,
    base: &str,
    range: &str,
    commits: Vec<serde_json::Value>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: attempt_patch_id(running, "committed-changes"),
        kind: "patch".to_string(),
        name: Some("committed-changes.patch".to_string()),
        label: Some("executor committed changes".to_string()),
        role: Some("patch".to_string()),
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(patch_sha256(patch)),
        metadata: serde_json::json!({
            "change_source": "local_commits",
            "base_ref": base,
            "commit_range": range,
            "commits": commits,
            "run_id": running.run_id,
            "task_id": running.task_id,
            "producer_attempt": running.attempt,
            "source_provenance": running.source_provenance,
            "provider_rotation_index": running.rotation_index,
            "provider_backend": running.request.executor.backend,
            "provider_model": running.request.executor.model(),
        }),
    }
}

fn patch_sha256(contents: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(contents.as_ref()))
}

fn attempt_patch_path(running: &RunningTask, kind: &str) -> Result<PathBuf, HarvestError> {
    let run_id = running.run_id.as_deref().unwrap_or("unrecorded-run");
    let dir = homeboy_core::artifacts::root()
        .map_err(|error| HarvestError::ArtifactDirectory {
            path: PathBuf::from("<artifact-root>"),
            message: error.message,
        })?
        .join("agent-task")
        .join("attempt-patches")
        .join(homeboy_core::paths::sanitize_path_segment(run_id))
        .join(homeboy_core::paths::sanitize_path_segment(&running.task_id));
    std::fs::create_dir_all(&dir).map_err(|error| HarvestError::ArtifactDirectory {
        path: dir.clone(),
        message: error.to_string(),
    })?;
    Ok(dir.join(format!(
        "attempt-{}-{}-{}.patch",
        running.attempt, kind, running.artifact_nonce
    )))
}

fn attempt_patch_id(running: &RunningTask, kind: &str) -> String {
    format!(
        "{}-attempt-{}-{kind}",
        homeboy_core::paths::sanitize_path_segment(&running.task_id),
        running.attempt
    )
}

fn committed_change_metadata(output: &str) -> Vec<serde_json::Value> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            Some(serde_json::json!({
                "sha": fields.next()?,
                "author_name": fields.next()?,
                "author_email": fields.next()?,
                "subject": fields.next()?,
            }))
        })
        .collect()
}

fn git_is_ancestor(cwd: &Path, base: &str, head: &str) -> Result<bool, HarvestError> {
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", base, head])
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: format!("git merge-base --is-ancestor {base} {head}"),
            cwd: cwd.to_path_buf(),
            message: error.to_string(),
        })?;
    Ok(output.status.success())
}

pub(super) fn git_is_repository(cwd: &Path) -> Result<bool, HarvestError> {
    let output = match Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            // A missing or inaccessible cwd (e.g. reaped scratch workspace)
            // is not a git repository. Return false so callers can fall back
            // instead of propagating ENOENT as a hard harvest failure.
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(false);
            }
            return Err(HarvestError::Git {
                command: "git rev-parse --is-inside-work-tree".to_string(),
                cwd: cwd.to_path_buf(),
                message: error.to_string(),
            });
        }
    };
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
}

/// Report workspace changes while excluding state injected by Homeboy's runner.
pub(super) fn git_status_ignoring_runner_metadata(cwd: &Path) -> Result<String, HarvestError> {
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--",
        ".",
    ];
    args.extend_from_slice(RUNNER_METADATA_EXCLUDE_PATHSPECS);
    git_output(cwd, &args)
}

pub(crate) fn git_output(cwd: &Path, args: &[&str]) -> Result<String, HarvestError> {
    Ok(git_output_raw(cwd, args)?.trim().to_string())
}

/// Preserve byte-sensitive Git output such as patches. Metadata callers use
/// `git_output` so commit IDs and status values stay normalized.
pub(crate) fn git_output_raw(cwd: &Path, args: &[&str]) -> Result<String, HarvestError> {
    git_output_raw_with_env(cwd, args, &[])
}

/// Trimmed Git output with extra environment (e.g. `GIT_INDEX_FILE` for a
/// scratch index). This is the single scheduler-side Git runner; no-env and
/// byte-preserving callers delegate here so the command/error shape stays
/// identical across the harvest and attempt-workspace paths.
pub(crate) fn git_output_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String, HarvestError> {
    Ok(git_output_raw_with_env(cwd, args, env)?.trim().to_string())
}

fn git_output_raw_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String, HarvestError> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: format!("git {}", args.join(" ")),
            cwd: cwd.to_path_buf(),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(HarvestError::Git {
            command: format!("git {}", args.join(" ")),
            cwd: cwd.to_path_buf(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug)]
pub(crate) enum HarvestError {
    DirtyWorkspace {
        status: String,
    },
    UnrelatedHead {
        base: String,
        head: String,
    },
    Git {
        command: String,
        cwd: PathBuf,
        message: String,
    },
    ArtifactDirectory {
        path: PathBuf,
        message: String,
    },
    ArtifactWrite {
        path: PathBuf,
        message: String,
    },
    Adoption {
        message: String,
    },
    CandidateBaselineMismatch {
        message: String,
    },
}

pub(super) fn committed_harvest_preflight_outcome(task_id: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: AgentTaskOutcomeStatus::Failed,
        summary: None,
        failure_classification: None,
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![crate::agent_task::AgentTaskEvidenceRef {
            kind: "scheduler".to_string(),
            uri: "homeboy://agent-task/committed-harvest-preflight".to_string(),
            label: Some("committed-change harvest preflight".to_string()),
        }],
        diagnostics: Vec::new(),
        outputs: serde_json::Value::Null,
        workflow: None,
        follow_up: None,
        metadata: serde_json::Value::Null,
    }
}

pub(super) fn committed_harvest_failure(
    mut outcome: AgentTaskOutcome,
    error: HarvestError,
) -> AgentTaskOutcome {
    let (class, message, data) = match error {
        HarvestError::DirtyWorkspace { status } => (
            "agent_task.committed_harvest_dirty_workspace",
            "refusing committed-change harvest from a workspace with pre-existing uncommitted changes"
                .to_string(),
            serde_json::json!({ "status": status }),
        ),
        HarvestError::UnrelatedHead { base, head } => (
            "agent_task.committed_harvest_unrelated_head",
            "workspace HEAD is no longer descended from the task base; refusing to harvest unrelated commits"
                .to_string(),
            serde_json::json!({ "base": base, "head": head }),
        ),
        HarvestError::Git {
            command,
            cwd,
            message,
        } => (
            "agent_task.committed_harvest_git_failed",
            format!("committed-change harvest failed while running {command}: {message}"),
            serde_json::json!({ "command": command, "cwd": cwd.display().to_string(), "stderr": message }),
        ),
        HarvestError::ArtifactDirectory { path, message } => (
            "agent_task.committed_harvest_artifact_failed",
            format!("committed-change harvest could not create artifact directory {}: {message}", path.display()),
            serde_json::json!({ "path": path, "operation": "create_dir_all", "error": message }),
        ),
        HarvestError::ArtifactWrite { path, message } => (
            "agent_task.committed_harvest_artifact_failed",
            format!("committed-change harvest could not write patch artifact {}: {message}", path.display()),
            serde_json::json!({ "path": path, "operation": "write", "error": message }),
        ),
        HarvestError::Adoption { message } => (
            "agent_task.attempt_workspace_adoption_failed",
            format!("could not materialize explicitly adopted candidate: {message}"),
            serde_json::json!({ "error": message }),
        ),
        HarvestError::CandidateBaselineMismatch { message } => (
            "agent_task.gate_feedback_candidate_baseline_mismatch",
            format!("refusing gate-feedback retry candidate baseline: {message}"),
            serde_json::json!({ "error": message }),
        ),
    };
    outcome.status = AgentTaskOutcomeStatus::Failed;
    outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
    outcome.summary = Some(message.clone());
    outcome.diagnostics.push(AgentTaskDiagnostic {
        class: class.to_string(),
        message,
        data,
    });
    outcome
}
#[cfg(test)]
mod committed_harvest_tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy_core::source_snapshot::SourceSnapshot;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    static LAB_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn process_context_rejects_incomplete_lab_transport_before_scheduler_execution() {
        let _guard = LAB_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("Lab environment lock");
        std::env::set_var(
            homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
            r#"{"runner_id":"lab"}"#,
        );
        std::env::remove_var(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV);

        let error = HarvestExecutionContext::from_current_process()
            .expect_err("a lone source snapshot must be rejected");

        assert!(error
            .to_string()
            .contains("incomplete Lab snapshot transport"));
        std::env::remove_var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command runs");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn gate_feedback_request(
        workspace: &Path,
        current_diff: String,
        patch_artifact: &Path,
        patch_sha256: &str,
    ) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "source-gate-fix-2".to_string(),
            group_key: None,
            parent_plan_id: Some("source-run".to_string()),
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions:
                "Continue the Homeboy cook loop from the current candidate worktree state."
                    .to_string(),
            inputs: serde_json::json!({ "cook_loop": {
                "source_run_id": "source-run",
                "source_task_id": "source",
                "source_patch_task_id": "source",
                "patch_artifact": {
                    "id": "candidate",
                    "path": patch_artifact,
                    "sha256": patch_sha256,
                },
                "failed_gates": [{ "gate_id": "visible" }],
                "next_attempt": 2,
                "to_worktree": workspace.display().to_string(),
                "current_diff": current_diff,
            }}),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                root: Some(workspace.display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::json!({ "cook_loop": { "kind": "deterministic-gate-feedback" }}),
        }
    }

    fn candidate_patch(workspace: &Path) -> (PathBuf, String) {
        git(workspace, &["add", "--all"]);
        let patch = git_output_raw(
            workspace,
            &["diff", "--cached", "--binary", "--full-index", "HEAD"],
        )
        .expect("candidate patch");
        git(workspace, &["reset", "--quiet"]);
        let path = workspace
            .parent()
            .expect("workspace has test parent")
            .join("candidate.patch");
        std::fs::write(&path, &patch).expect("write candidate patch");
        let sha256 = format!("{:x}", Sha256::digest(patch.as_bytes()));
        (path, sha256)
    }

    #[test]
    fn gate_feedback_baseline_allows_only_the_recorded_candidate_and_harvests_remediation_delta() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "--quiet", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join("candidate.txt"), "base\n").expect("base");
        git(&workspace, &["add", "candidate.txt"]);
        git(&workspace, &["commit", "--quiet", "-m", "base"]);
        std::fs::write(workspace.join("candidate.txt"), "candidate\n").expect("candidate");
        std::fs::write(workspace.join("new-candidate.txt"), "new candidate\n")
            .expect("new candidate");
        let (patch_artifact, patch_sha256) = candidate_patch(&workspace);
        let current_diff = std::fs::read_to_string(&patch_artifact).expect("complete current diff");
        let mut request =
            gate_feedback_request(&workspace, current_diff, &patch_artifact, &patch_sha256);

        let preflight =
            prepare_committed_harvest(&request, None, &HarvestExecutionContext::default())
                .expect("recorded promoted candidate is accepted");
        let source_base = preflight.base_sha.expect("source base");
        let scratch = tempfile::tempdir().expect("attempt scratch");
        let attempt = prepare_attempt_workspace(
            &mut request,
            Some(&source_base),
            preflight.candidate_baseline.as_ref(),
            scratch.path(),
        )
        .expect("attempt workspace")
        .expect("isolated attempt");
        let attempt_root = PathBuf::from(request.workspace.root.as_deref().expect("attempt root"));
        assert_eq!(
            std::fs::read_to_string(attempt_root.join("candidate.txt"))
                .expect("candidate baseline"),
            "candidate\n"
        );
        assert_eq!(
            std::fs::read_to_string(attempt_root.join("new-candidate.txt"))
                .expect("new candidate baseline"),
            "new candidate\n"
        );
        std::fs::write(attempt_root.join("remediation.txt"), "green\n").expect("remediation");
        git(&attempt_root, &["add", "remediation.txt"]);
        git(&attempt_root, &["commit", "--quiet", "-m", "remediation"]);
        let remediation_base = attempt.base_sha().to_string();
        let patch = git_output_raw(
            &attempt_root,
            &["diff", "--binary", &remediation_base, "HEAD"],
        )
        .expect("remediation delta");
        assert!(patch.contains("remediation.txt"));
        assert!(!patch.contains("new-candidate.txt"));
        assert!(
            !patch.contains("-base"),
            "candidate is not replayed in remediation"
        );
    }

    #[test]
    fn gate_feedback_baseline_rejects_mismatched_or_extra_dirty_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "--quiet", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join("file.txt"), "base\n").expect("base");
        git(&workspace, &["add", "file.txt"]);
        git(&workspace, &["commit", "--quiet", "-m", "base"]);
        std::fs::write(workspace.join("file.txt"), "candidate\n").expect("candidate");
        let (patch_artifact, patch_sha256) = candidate_patch(&workspace);
        let recorded = std::fs::read_to_string(&patch_artifact).expect("complete recorded diff");
        std::fs::write(workspace.join("extra.txt"), "unrelated\n").expect("extra");
        let error = prepare_committed_harvest(
            &gate_feedback_request(&workspace, recorded.clone(), &patch_artifact, &patch_sha256),
            None,
            &HarvestExecutionContext::default(),
        )
        .expect_err("untracked unrelated dirt must fail closed");
        assert!(matches!(
            error,
            HarvestError::CandidateBaselineMismatch { .. }
        ));

        std::fs::remove_file(workspace.join("extra.txt")).expect("remove extra");
        let error = prepare_committed_harvest(
            &gate_feedback_request(
                &workspace,
                "not a patch".to_string(),
                &patch_artifact,
                "bad",
            ),
            None,
            &HarvestExecutionContext::default(),
        )
        .expect_err("mismatched recorded diff must fail closed");
        assert!(matches!(
            error,
            HarvestError::CandidateBaselineMismatch { .. }
        ));

        let mut ordinary =
            gate_feedback_request(&workspace, String::new(), &patch_artifact, &patch_sha256);
        ordinary.metadata = serde_json::Value::Null;
        let error = prepare_committed_harvest(&ordinary, None, &HarvestExecutionContext::default())
            .expect_err("ordinary dirty worktree remains refused");
        assert!(matches!(error, HarvestError::DirtyWorkspace { .. }));
    }

    #[test]
    fn git_metadata_failure_after_patch_creation_preserves_the_patch_artifact() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join("file.txt"), "base\n").expect("base file");
        git(&workspace, &["add", "file.txt"]);
        git(&workspace, &["commit", "-m", "base"]);
        let base = git(&workspace, &["rev-parse", "HEAD"]);
        std::fs::write(workspace.join("file.txt"), "committed change\n").expect("change");
        git(&workspace, &["commit", "-am", "agent change"]);
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                root: Some(workspace.display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        };
        let running = RunningTask {
            task_id: "task-1".to_string(),
            request,
            workspace_key: None,
            executor_key: "test".to_string(),
            model_key: None,
            resource_units: 1,
            exclusive_resource_keys: Vec::new(),
            attempt: 1,
            started_at: Instant::now(),
            timeout_ms: None,
            execution_deadline_unix_ms: None,
            timeout_cancel_requested: false,
            rotation_index: 0,
            rotation_attempts: Vec::new(),
            candidate_artifacts: Vec::new(),
            retry_attempts: Vec::new(),
            source_workspace_root: None,
            _attempt_workspace: None,
            run_id: Some("committed-harvest-test".to_string()),
            artifact_nonce: "test-artifact".to_string(),
            task_base_sha: Some(base),
            source_provenance: None,
            scratch: crate::controller_scratch::ControllerScratchAllocation {
                path: PathBuf::from("/test/controller-scratch/1"),
                lease_id: "test-lease-1".to_string(),
                index_path: PathBuf::from("/test/controller-scratch/resources.json"),
            },
            adoption: None,
            join_handle: None,
        };
        let mut outcome = committed_harvest_preflight_outcome("task-1".to_string());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        let error = harvest_committed_patch_with_metadata(&mut outcome, &running, |_, _| {
            Err(HarvestError::Git {
                command: "git log injected metadata failure".to_string(),
                cwd: PathBuf::from("/test/workspace"),
                message: "injected failure".to_string(),
            })
        })
        .expect_err("metadata command fails after the real patch is attached");
        let patch_path = outcome.artifacts[0].path.clone().expect("patch path");
        assert!(Path::new(&patch_path).is_file());
        let patch = std::fs::read_to_string(&patch_path).expect("read attached patch");
        assert!(!patch.trim().is_empty());
        assert!(patch.contains("diff --git a/file.txt b/file.txt"));
        assert!(patch.contains("-base\n"));
        assert!(patch.contains("+committed change"));
        assert_eq!(
            outcome.artifacts[0].size_bytes,
            Some(patch.len() as u64),
            "artifact size must match the written patch"
        );
        let failed = committed_harvest_failure(outcome, error);

        assert_eq!(failed.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(failed.artifacts[0].id, "task-1-attempt-1-committed-changes");
        assert_eq!(
            failed.artifacts[0].path.as_deref(),
            Some(patch_path.as_str())
        );
        assert!(failed.evidence_refs.iter().any(|evidence| {
            evidence.uri == "homeboy://agent-task/committed-harvest-preflight"
        }));
        assert!(failed.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.committed_harvest_git_failed"
                && diagnostic.data["command"] == "git log injected metadata failure"
        }));
    }

    #[test]
    fn uncommitted_harvest_excludes_homeboy_runner_metadata_drift() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join("file.txt"), "base\n").expect("base file");
        // The runner metadata exists at the baseline; the runner rewrites it after
        // materialization, which is checkout drift, not provider output. (#8534)
        std::fs::create_dir_all(workspace.join(".homeboy")).expect(".homeboy dir");
        std::fs::write(
            workspace.join(".homeboy/runner-workspace.json"),
            "{\"stage\":\"baseline\"}\n",
        )
        .expect("runner metadata baseline");
        git(&workspace, &["add", "-A"]);
        git(&workspace, &["commit", "-m", "base"]);
        let base = git(&workspace, &["rev-parse", "HEAD"]);

        // The provider edits one file; the runner rewrites its metadata (drift).
        std::fs::write(workspace.join("file.txt"), "provider change\n").expect("provider edit");
        std::fs::write(
            workspace.join(".homeboy/runner-workspace.json"),
            "{\"stage\":\"final\"}\n",
        )
        .expect("runner metadata drift");

        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                root: Some(workspace.display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        };
        let running = RunningTask {
            task_id: "task-1".to_string(),
            request,
            workspace_key: None,
            executor_key: "test".to_string(),
            model_key: None,
            resource_units: 1,
            exclusive_resource_keys: Vec::new(),
            attempt: 1,
            started_at: Instant::now(),
            timeout_ms: None,
            execution_deadline_unix_ms: None,
            timeout_cancel_requested: false,
            rotation_index: 0,
            rotation_attempts: Vec::new(),
            candidate_artifacts: Vec::new(),
            retry_attempts: Vec::new(),
            source_workspace_root: None,
            _attempt_workspace: None,
            run_id: Some("metadata-exclude-test".to_string()),
            artifact_nonce: "test-artifact".to_string(),
            task_base_sha: Some(base),
            source_provenance: None,
            scratch: crate::controller_scratch::ControllerScratchAllocation {
                path: temp.path().join("controller-scratch"),
                lease_id: "test-lease-1".to_string(),
                index_path: temp.path().join("resources.json"),
            },
            adoption: None,
            join_handle: None,
        };
        std::fs::create_dir_all(&running.scratch.path).expect("scratch dir");

        let mut outcome = committed_harvest_preflight_outcome("task-1".to_string());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        harvest_uncommitted_patch(&mut outcome, &running).expect("harvest uncommitted patch");

        let patch_path = outcome.artifacts[0]
            .path
            .clone()
            .expect("uncommitted patch path");
        let patch = std::fs::read_to_string(&patch_path).expect("read patch");
        assert!(
            patch.contains("file.txt") && patch.contains("+provider change"),
            "the provider edit must be captured: {patch}"
        );
        assert!(
            !patch.contains("runner-workspace.json"),
            "Homeboy runner metadata drift must be excluded from the candidate patch: {patch}"
        );
    }

    #[test]
    fn controller_context_ignores_ambient_lab_signal_without_leaking_it_to_local_cells() {
        let _guard = LAB_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("Lab environment lock");
        std::env::remove_var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
        std::env::set_var(
            homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
            r#"{"status":"run_local"}"#,
        );
        let workspace = tempfile::tempdir().expect("workspace");
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "local-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                root: Some(workspace.path().display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        };

        let preflight =
            prepare_committed_harvest(&request, None, &HarvestExecutionContext::default())
                .expect("local controller cell ignores ambient Lab metadata");

        assert!(preflight.base_sha.is_none());
        assert!(preflight.source_provenance.is_none());
        assert!(!workspace.path().join(".git").exists());
        assert!(std::env::var(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV).is_ok());
        std::env::remove_var(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV);
    }

    #[test]
    fn local_git_workspace_ignores_unrelated_lab_snapshot_metadata() {
        let _guard = LAB_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("Lab environment lock");
        let workspace = tempfile::tempdir().expect("workspace");
        git(workspace.path(), &["init", "--quiet", "-b", "main"]);
        git(
            workspace.path(),
            &["config", "user.email", "test@homeboy.invalid"],
        );
        git(workspace.path(), &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source");
        git(workspace.path(), &["add", "file.txt"]);
        git(workspace.path(), &["commit", "--quiet", "-m", "baseline"]);
        std::env::remove_var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
        std::env::set_var(
            homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
            serde_json::json!({
                "runner_id": "other-lab",
                "remote_workspace": "/runner/workspace",
                "sync_mode": "snapshot"
            })
            .to_string(),
        );
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "local-git-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                root: Some(workspace.path().display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        };

        let preflight =
            prepare_committed_harvest(&request, None, &HarvestExecutionContext::default())
                .expect("local Git harvest preflight ignores unrelated Lab metadata");

        assert_eq!(
            preflight.base_sha.as_deref(),
            Some(git(workspace.path(), &["rev-parse", "HEAD"]).as_str())
        );
        assert!(preflight.source_provenance.is_none());
        std::env::remove_var(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV);
    }

    #[test]
    fn committed_harvest_falls_back_to_source_when_attempt_scratch_is_reaped() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");

        // Create the durable source workspace with a committed change past base.
        let source = temp.path().join("source-workspace");
        std::fs::create_dir(&source).expect("source workspace dir");
        git(&source, &["init", "--quiet", "-b", "main"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(source.join("file.txt"), "base\n").expect("base file");
        git(&source, &["add", "file.txt"]);
        git(&source, &["commit", "--quiet", "-m", "base"]);
        let base = git(&source, &["rev-parse", "HEAD"]);
        std::fs::write(source.join("file.txt"), "agent change\n").expect("agent edit");
        git(&source, &["commit", "--quiet", "-am", "agent change"]);

        // Simulate a reaped attempt scratch workspace (path does not exist).
        let reaped_scratch = temp.path().join("nonexistent-scratch-workspace");
        assert!(!reaped_scratch.exists());

        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "reaped-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                // The attempt scratch workspace root points at a reaped path.
                root: Some(reaped_scratch.display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        };
        let running = RunningTask {
            task_id: "reaped-task".to_string(),
            request,
            workspace_key: None,
            executor_key: "test".to_string(),
            model_key: None,
            resource_units: 1,
            exclusive_resource_keys: Vec::new(),
            attempt: 3,
            started_at: Instant::now(),
            timeout_ms: None,
            execution_deadline_unix_ms: None,
            timeout_cancel_requested: false,
            rotation_index: 0,
            rotation_attempts: Vec::new(),
            candidate_artifacts: Vec::new(),
            retry_attempts: Vec::new(),
            // The durable source workspace is the real, persistent checkout.
            source_workspace_root: Some(source.display().to_string()),
            _attempt_workspace: None,
            run_id: Some("reaped-harvest-test".to_string()),
            artifact_nonce: "test-artifact".to_string(),
            task_base_sha: Some(base),
            source_provenance: None,
            scratch: crate::controller_scratch::ControllerScratchAllocation {
                path: temp.path().join("controller-scratch"),
                lease_id: "test-lease-1".to_string(),
                index_path: temp.path().join("resources.json"),
            },
            adoption: None,
            join_handle: None,
        };
        std::fs::create_dir_all(&running.scratch.path).expect("scratch dir");

        let mut outcome = committed_harvest_preflight_outcome("reaped-task".to_string());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;

        // The harvest must fall back to the durable source and produce the patch.
        harvest_committed_patch_with_metadata(&mut outcome, &running, |cwd, range| {
            committed_change_metadata_for_range(cwd, range)
        })
        .expect("harvest falls back to source workspace");

        assert!(
            !outcome.artifacts.is_empty(),
            "a committed patch artifact must be produced"
        );
        let patch_artifact = &outcome.artifacts[0];
        assert_eq!(patch_artifact.kind, "patch");
        let patch_path = patch_artifact.path.as_ref().expect("patch path");
        let patch = std::fs::read_to_string(patch_path).expect("read patch");
        assert!(
            patch.contains("agent change"),
            "patch must contain the agent's change: {patch}"
        );
        assert!(
            !outcome
                .diagnostics
                .iter()
                .any(|d| d.class == "agent_task.committed_harvest_git_failed"),
            "no harvest git failure diagnostic expected"
        );
    }
}
