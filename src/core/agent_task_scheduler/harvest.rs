use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::*;

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
    git_output(root, &["add", "--all"])?;
    let patch = git_output_raw(
        root,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--find-renames",
            "HEAD",
        ],
    )?;
    if patch.trim().is_empty() {
        return Ok(());
    }
    let changed_files = git_changed_files(root, &["diff", "--cached", "--name-only", "HEAD"])?;
    let path = attempt_patch_path(running, "uncommitted")?;
    std::fs::write(&path, patch.as_bytes()).map_err(|error| HarvestError::ArtifactWrite {
        path: path.clone(),
        message: error.to_string(),
    })?;
    outcome.artifacts.push(AgentTaskArtifact {
        schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
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
    let mut root = attempt_root;
    let mut head = git_output(root, &["rev-parse", "HEAD"])?;
    if head == base {
        if let Some(source_root) = running
            .source_workspace_root
            .as_deref()
            .map(Path::new)
            .filter(|source_root| source_root != &attempt_root)
        {
            let source_head = git_output(source_root, &["rev-parse", "HEAD"])?;
            if source_head != base {
                // The scheduler owns this bounded Git-derived patch. It is not
                // a provider-declared runtime file, even though its source
                // checkout differs from the isolated execution checkout.
                root = source_root;
                head = source_head;
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
    let patch = git_output_raw(
        root,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--find-renames",
            base,
            "HEAD",
        ],
    )?;
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
    let changed_files = git_changed_files(root, &["diff", "--name-only", base, "HEAD"])?;
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
        schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
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
    let dir = crate::core::artifacts::root()
        .map_err(|error| HarvestError::ArtifactDirectory {
            path: PathBuf::from("<artifact-root>"),
            message: error.message,
        })?
        .join("agent-task")
        .join("attempt-patches")
        .join(crate::core::paths::sanitize_path_segment(run_id))
        .join(crate::core::paths::sanitize_path_segment(&running.task_id));
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
        crate::core::paths::sanitize_path_segment(&running.task_id),
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
            message: error.to_string(),
        })?;
    Ok(output.status.success())
}

pub(super) fn git_is_repository(cwd: &Path) -> Result<bool, HarvestError> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: "git rev-parse --is-inside-work-tree".to_string(),
            message: error.to_string(),
        })?;
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
}

pub(crate) fn git_output(cwd: &Path, args: &[&str]) -> Result<String, HarvestError> {
    Ok(git_output_raw(cwd, args)?.trim().to_string())
}

/// Preserve byte-sensitive Git output such as patches. Metadata callers use
/// `git_output` so commit IDs and status values stay normalized.
pub(crate) fn git_output_raw(cwd: &Path, args: &[&str]) -> Result<String, HarvestError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: format!("git {}", args.join(" ")),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(HarvestError::Git {
            command: format!("git {}", args.join(" ")),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug)]
pub(crate) enum HarvestError {
    DirtyWorkspace { status: String },
    UnrelatedHead { base: String, head: String },
    Git { command: String, message: String },
    ArtifactDirectory { path: PathBuf, message: String },
    ArtifactWrite { path: PathBuf, message: String },
    Adoption { message: String },
}

pub(super) fn committed_harvest_preflight_outcome(task_id: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: AgentTaskOutcomeStatus::Failed,
        summary: None,
        failure_classification: None,
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![crate::core::agent_task::AgentTaskEvidenceRef {
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
        HarvestError::Git { command, message } => (
            "agent_task.committed_harvest_git_failed",
            format!("committed-change harvest failed while running {command}: {message}"),
            serde_json::json!({ "command": command, "stderr": message }),
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
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::source_snapshot::SourceSnapshot;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    static LAB_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command runs");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn git_metadata_failure_after_patch_creation_preserves_the_patch_artifact() {
        let _home = crate::test_support::HomeGuard::new();
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
            scratch: crate::core::controller_scratch::ControllerScratchAllocation {
                path: PathBuf::from("/test/controller-scratch/1"),
                lease_id: "test-lease-1".to_string(),
            },
        };
        let mut outcome = committed_harvest_preflight_outcome("task-1".to_string());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        let error = harvest_committed_patch_with_metadata(&mut outcome, &running, |_, _| {
            Err(HarvestError::Git {
                command: "git log injected metadata failure".to_string(),
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
    fn lab_snapshot_preflight_materializes_a_provider_ready_attempt_workspace() {
        let _guard = LAB_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("Lab environment lock");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source");
        let path = workspace.path().display().to_string();
        let snapshot = SourceSnapshot {
            runner_id: "lab".to_string(),
            local_path: Some("/controller/source".to_string()),
            remote_path: Some(path.clone()),
            workspace_root: None,
            git_branch: Some("main".to_string()),
            git_sha: Some("a".repeat(40)),
            dirty: false,
            sync_mode: "lab_offload".to_string(),
            workspace_snapshot_identity: Some("snapshot:provider-ready".to_string()),
            snapshot_hash: "sha256:provider-ready".to_string(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            sync_excludes: vec![".git".to_string(), ".git/**".to_string()],
        };
        let content_hash =
            crate::core::runner::workspace_content_hash(workspace.path(), &snapshot.sync_excludes)
                .expect("content hash");
        let lab = serde_json::json!({
            "runner_id": "lab", "remote_workspace": path, "sync_mode": "snapshot", "status": "offloaded",
            "source_snapshot": snapshot,
            "workspace_verification": {
                "schema": "homeboy/lab-workspace-verification/v2", "identity": "snapshot:provider-ready",
                "content_hash": content_hash, "sync_excludes": snapshot.sync_excludes,
                "permission_policy": "unix-executable", "content_hash_algorithm": "homeboy-workspace-content-v2+unix-executable",
                "source_snapshot": snapshot,
                "primary_workspace": { "identity": "snapshot:provider-ready", "remote_path": workspace.path().display().to_string() }
            }
        });
        std::env::set_var(
            crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
            serde_json::to_string(&snapshot).expect("snapshot JSON"),
        );
        std::env::set_var(
            crate::core::observation::LAB_OFFLOAD_METADATA_ENV,
            serde_json::to_string(&lab).expect("Lab JSON"),
        );
        let mut request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "snapshot-task".to_string(),
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
        let preflight = prepare_committed_harvest(&request).expect("snapshot preflight");
        let baseline = preflight.base_sha.expect("synthetic baseline");
        let source_provenance = preflight.source_provenance.expect("source provenance");
        assert!(workspace.path().join(".git").is_dir());
        assert_eq!(source_provenance["source_revision"], "a".repeat(40));
        let attempt = prepare_attempt_workspace(&mut request, Some(&baseline))
            .expect("provider-ready attempt")
            .expect("attempt workspace");
        assert_ne!(
            request.workspace.root.as_deref(),
            Some(workspace.path().to_str().unwrap())
        );
        assert!(git_is_repository(Path::new(request.workspace.root.as_deref().unwrap())).unwrap());
        let external_patch = workspace.path().join("external.patch");
        std::fs::write(&external_patch, "external patch").expect("external patch");
        let mut outcome = committed_harvest_preflight_outcome("snapshot-task".to_string());
        outcome.artifacts = vec![
            AgentTaskArtifact {
                schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "external".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some(external_patch.display().to_string()),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: serde_json::json!({"kept": true}),
            },
            AgentTaskArtifact {
                schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "url-only".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: None,
                url: Some("https://example.test/candidate.patch".to_string()),
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: serde_json::Value::Null,
            },
        ];
        let running = RunningTask {
            task_id: "snapshot-task".to_string(),
            request: request.clone(),
            workspace_key: None,
            executor_key: "test".to_string(),
            model_key: None,
            resource_units: 1,
            exclusive_resource_keys: Vec::new(),
            attempt: 1,
            started_at: Instant::now(),
            timeout_ms: None,
            timeout_cancel_requested: false,
            rotation_index: 0,
            rotation_attempts: Vec::new(),
            candidate_artifacts: Vec::new(),
            retry_attempts: Vec::new(),
            source_workspace_root: None,
            _attempt_workspace: None,
            run_id: None,
            artifact_nonce: "test".to_string(),
            task_base_sha: Some(baseline),
            source_provenance: Some(source_provenance.clone()),
            scratch: crate::core::controller_scratch::ControllerScratchAllocation {
                path: PathBuf::from("/test/controller-scratch/1"),
                lease_id: "test-lease-1".to_string(),
            },
        };
        persist_attempt_patch_artifacts(
            &mut outcome,
            &running,
            Path::new(request.workspace.root.as_deref().unwrap()),
        )
        .expect("external patch provenance");
        for artifact in &outcome.artifacts {
            assert_eq!(artifact.metadata["source_provenance"], source_provenance);
        }
        assert_eq!(outcome.artifacts[0].metadata["kept"], true);
        drop(attempt);
        std::env::remove_var(crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
        std::env::remove_var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
    }

    #[test]
    fn local_non_git_workspace_skips_harvest_preflight_without_lab_transport() {
        let _guard = LAB_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("Lab environment lock");
        std::env::remove_var(crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
        std::env::remove_var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
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

        let preflight = prepare_committed_harvest(&request).expect("local non-Git no-op");

        assert!(preflight.base_sha.is_none());
        assert!(preflight.source_provenance.is_none());
        assert!(!workspace.path().join(".git").exists());
    }
}
