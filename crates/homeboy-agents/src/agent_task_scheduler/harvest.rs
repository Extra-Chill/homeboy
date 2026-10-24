use homeboy_engine_primitives::content_hash;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Ref namespace that pins harvested candidates against garbage collection.
pub(crate) const CANDIDATE_REF_PREFIX: &str = "refs/homeboy/candidates/";

/// Turn the attempt's final working tree (tracked + untracked, honoring
/// `.gitignore`) into one immutable commit on the attempt HEAD (#16303).
///
/// The tree is built in a temporary index so the provider's index is never
/// written (#13448). Identity and dates are fixed (the date is the parent's
/// committer date), so the same tree on the same parent is the same sha. A
/// tree equal to the base tree is `NoChanges`: no candidate is recorded.
pub(super) fn harvest_candidate_commit(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
) -> Result<(), HarvestError> {
    let on_runner = crate::agent_task_lifecycle::execution_runner_id().is_some();
    let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::new();
    let result = timer.time("promote.harvest", || {
        harvest_candidate_commit_with_bundle(outcome, running, on_runner)
    });
    if let Some(run_id) = running.run_id.as_deref() {
        let report = timer.into_report();
        let persist = (|| -> homeboy_core::Result<()> {
            let store = homeboy_core::observation::ObservationStore::open_initialized()?;
            store.record_phase_spans(
                run_id,
                &format!(
                    "harvest:{}:{}:{}",
                    running.task_id, running.attempt, report.spans[0].started_at
                ),
                &report,
            )
        })();
        if let Err(error) = persist {
            homeboy_core::log_status!("observe", "harvest timing persistence failed: {}", error);
        }
    }
    result
}

/// `bundle`: a Lab runner hands the candidate back as the commit itself.
fn harvest_candidate_commit_with_bundle(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
    bundle: bool,
) -> Result<(), HarvestError> {
    // Only Homeboy's harvest may assert a candidate.
    outcome.candidate = None;
    let Some(base) = running.task_base_sha.as_deref() else {
        return Ok(());
    };
    let Some(attempt_root) = running.request.workspace.root.as_deref().map(Path::new) else {
        return Ok(());
    };
    let run_id = running.run_id.as_deref().unwrap_or("unrecorded-run");
    let mut harvested_from_source = false;
    let root = if git_is_repository(attempt_root)? {
        outcome.candidate = candidate_from_workspace(attempt_root, base, run_id)?;
        attempt_root
    } else if let Some(source_root) = reaped_attempt_source_root(running, attempt_root)? {
        // The attempt checkout was reaped (#15980). Its committed change set
        // survives in the durable source workspace, so harvest it from there.
        outcome.candidate = candidate_from_committed_head(source_root, base, run_id)?;
        harvested_from_source = outcome.candidate.is_some();
        source_root
    } else {
        return Ok(());
    };
    let Some(candidate) = outcome.candidate.as_ref() else {
        if outcome.status == AgentTaskOutcomeStatus::Succeeded {
            crate::agent_task_provider::record_verified_unchanged_empty_attempt(outcome);
        }
        return Ok(());
    };
    let candidate_sha = candidate.candidate_sha.clone();
    if !outcome.metadata.is_object() {
        outcome.metadata = serde_json::json!({});
    }
    outcome.metadata["source_provenance"] = serde_json::json!(running.source_provenance);
    if harvested_from_source {
        outcome.metadata["candidate_harvest_root"] = serde_json::json!({
            "kind": "source_workspace",
            "reason": "attempt_workspace_unavailable",
            "attempt_root": attempt_root.display().to_string(),
            "source_root": root.display().to_string(),
        });
    }
    let candidate_ref = format!(
        "{CANDIDATE_REF_PREFIX}{}",
        homeboy_core::paths::sanitize_path_segment(run_id)
    );
    if bundle {
        // The bundle is the Lab handback of the candidate (#16303).
        let path = candidate_bundle_path(running)?;
        let path_arg = path.display().to_string();
        let exclude_base = format!("^{base}");
        git_output(
            root,
            &["bundle", "create", &path_arg, &candidate_ref, &exclude_base],
        )?;
        let bytes = std::fs::read(&path).map_err(|error| HarvestError::ArtifactWrite {
            path: path.clone(),
            message: error.to_string(),
        })?;
        outcome.artifacts.push(AgentTaskArtifact {
            id: format!(
                "{}-attempt-{}-candidate-bundle",
                running.task_id, running.attempt
            ),
            kind: crate::agent_task_promotion::CANDIDATE_BUNDLE_ARTIFACT_KIND.to_string(),
            path: Some(path_arg),
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(patch_sha256(&bytes)),
            metadata: serde_json::json!({ "candidate_sha": candidate_sha, "base_ref": base }),
            ..AgentTaskArtifact::default()
        });
    }
    Ok(())
}

/// The durable source workspace that stands in for a reaped attempt
/// checkout: only when it is a different path and still a Git repository.
fn reaped_attempt_source_root<'a>(
    running: &'a RunningTask,
    attempt_root: &Path,
) -> Result<Option<&'a Path>, HarvestError> {
    let Some(source_root) = running
        .source_workspace_root
        .as_deref()
        .map(Path::new)
        .filter(|source_root| *source_root != attempt_root)
    else {
        return Ok(None);
    };
    Ok(git_is_repository(source_root)?.then_some(source_root))
}

/// Refuse a HEAD that no longer descends from the task base: its tree is not
/// a change set against the base, and treating it as "no changes" would hide
/// the rewrite.
fn ensure_head_descends_from_base(root: &Path, base: &str) -> Result<String, HarvestError> {
    let head = git_output(root, &["rev-parse", "HEAD^{commit}"])?;
    if !git_is_ancestor(root, base, &head)? {
        return Err(HarvestError::UnrelatedHead {
            base: base.to_string(),
            head,
        });
    }
    Ok(head)
}

fn pin_candidate_ref(root: &Path, run_id: &str, candidate_sha: &str) -> Result<(), HarvestError> {
    let candidate_ref = format!(
        "{CANDIDATE_REF_PREFIX}{}",
        homeboy_core::paths::sanitize_path_segment(run_id)
    );
    git_output(root, &["update-ref", &candidate_ref, candidate_sha])?;
    Ok(())
}

/// Harvest only the committed change set of a workspace whose uncommitted
/// state Homeboy does not own (the source workspace standing in for a reaped
/// attempt checkout). The candidate is the HEAD commit itself.
fn candidate_from_committed_head(
    root: &Path,
    base: &str,
    run_id: &str,
) -> Result<Option<crate::agent_task::AgentTaskCandidate>, HarvestError> {
    let head = ensure_head_descends_from_base(root, base)?;
    let tree = git_output(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    if tree == git_output(root, &["rev-parse", &format!("{base}^{{tree}}")])? {
        return Ok(None);
    }
    pin_candidate_ref(root, run_id, &head)?;
    Ok(Some(crate::agent_task::AgentTaskCandidate {
        base_sha: base.to_string(),
        candidate_sha: head,
        tree_sha: tree,
    }))
}

/// Capture one owned worktree without touching its live index.
pub(crate) fn candidate_from_workspace(
    root: &Path,
    base: &str,
    run_id: &str,
) -> Result<Option<crate::agent_task::AgentTaskCandidate>, HarvestError> {
    if !git_is_repository(root)? {
        return Ok(None);
    }
    let head = ensure_head_descends_from_base(root, base)?;
    let index = tempfile::NamedTempFile::new().map_err(|error| HarvestError::Git {
        command: "create candidate harvest Git index".to_string(),
        cwd: root.to_path_buf(),
        message: error.to_string(),
    })?;
    let index_path = index.path().display().to_string();
    let index_env: &[(&str, &str)] = &[("GIT_INDEX_FILE", index_path.as_str())];
    git_output_with_env(root, &["read-tree", "HEAD"], index_env)?;
    git_output_with_env(root, &["add", "--all", "--", "."], index_env)?;
    let tree = git_output_with_env(root, &["write-tree"], index_env)?;
    if tree == git_output(root, &["rev-parse", &format!("{base}^{{tree}}")])? {
        return Ok(None);
    }
    let date = format!(
        "@{} +0000",
        git_output(root, &["log", "-1", "--format=%ct", &head])?
    );
    let identity_env: &[(&str, &str)] = &[
        ("GIT_AUTHOR_NAME", "Homeboy"),
        ("GIT_AUTHOR_EMAIL", "homeboy@localhost"),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", "Homeboy"),
        ("GIT_COMMITTER_EMAIL", "homeboy@localhost"),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let commit_args = [
        "-c",
        "commit.gpgSign=false",
        "commit-tree",
        tree.as_str(),
        "-p",
        head.as_str(),
        "-m",
        "homeboy: cook candidate",
    ];
    let candidate_sha = git_output_with_env(root, &commit_args, identity_env)?;
    pin_candidate_ref(root, run_id, &candidate_sha)?;
    Ok(Some(crate::agent_task::AgentTaskCandidate {
        base_sha: base.to_string(),
        candidate_sha: candidate_sha.clone(),
        tree_sha: tree,
    }))
}

/// A deadline can interrupt an otherwise valid workspace diff before the
/// provider has completed its result contract. Keep that distinction on the
/// durable artifact: it is recoverable evidence, never an implicitly complete
/// provider result.
pub(super) fn mark_timeout_workspace_candidates_incomplete(outcome: &mut AgentTaskOutcome) {
    if outcome.candidate.is_none() {
        return;
    }
    if !outcome.metadata.is_object() {
        outcome.metadata = serde_json::json!({});
    }
    outcome.metadata["timeout_recovery"] = serde_json::json!({
        "incomplete": true,
        "recoverable_candidate_count": 1,
    });
}

fn patch_sha256(contents: impl AsRef<[u8]>) -> String {
    content_hash::sha256_hex(contents.as_ref())
}

fn candidate_bundle_path(running: &RunningTask) -> Result<PathBuf, HarvestError> {
    let run_id = running.run_id.as_deref().unwrap_or("unrecorded-run");
    let root = super::artifact_root_for_running(running)?;
    let dir = root
        .join("agent-task")
        .join("candidate-bundles")
        .join(homeboy_core::paths::sanitize_path_segment(run_id))
        .join(homeboy_core::paths::sanitize_path_segment(&running.task_id));
    std::fs::create_dir_all(&dir).map_err(|error| HarvestError::ArtifactDirectory {
        path: dir.clone(),
        message: error.to_string(),
    })?;
    Ok(dir.join(format!(
        "attempt-{}-{}.bundle",
        running.attempt, running.artifact_nonce
    )))
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
pub(super) fn git_status_porcelain(cwd: &Path) -> Result<String, HarvestError> {
    git_status_ignoring_snapshot_excludes(cwd, &[])
}

/// Same cleanliness surface as [`git_status_porcelain`], plus
/// declared snapshot excludes so omitted tracked context is not source dirt.
pub(super) fn git_status_ignoring_snapshot_excludes(
    cwd: &Path,
    sync_excludes: &[String],
) -> Result<String, HarvestError> {
    let extra = homeboy_core::source_snapshot::git_exclude_pathspecs(sync_excludes);
    let mut args = vec![
        "status".to_string(),
        "--porcelain=v1".to_string(),
        "--untracked-files=all".to_string(),
        "--".to_string(),
        ".".to_string(),
    ];
    args.extend(extra);
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
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
    CandidateTreeMismatch {
        message: String,
    },
    SnapshotInvalidated {
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
        candidate: None,
    }
}

pub(super) fn committed_harvest_failure(
    mut outcome: AgentTaskOutcome,
    error: HarvestError,
) -> AgentTaskOutcome {
    // Harvest is secondary evidence collection. If execution already failed and
    // there is no candidate to harvest, a Git/provenance inspection failure
    // must not replace the provider's actionable failure reason. Workspace
    // cleanliness and candidate-baseline violations remain fail-closed below.
    let preserve_execution_failure = outcome.status != AgentTaskOutcomeStatus::Succeeded
        && outcome.candidate.is_none()
        && matches!(error, HarvestError::Git { .. });
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
        HarvestError::CandidateTreeMismatch { message } => (
            "agent_task.gate_feedback_candidate_baseline_mismatch",
            format!("refusing gate-feedback retry candidate baseline: {message}"),
            serde_json::json!({ "error": message }),
        ),
        HarvestError::SnapshotInvalidated { message } => (
            "agent_task.workspace_snapshot_invalidated",
            format!("Cook workspace snapshot was invalidated before provider execution: {message}"),
            serde_json::json!({ "error": message, "pre_provider": true }),
        ),
    };
    if preserve_execution_failure {
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: class.to_string(),
            message,
            data,
        });
        return outcome;
    }
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

    use sha2::{Digest, Sha256};
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    static LAB_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn unavailable_provider_remains_primary_when_hash_check_fails_without_candidate() {
        let mut outcome = committed_harvest_preflight_outcome("task-1".to_string());
        outcome.status = AgentTaskOutcomeStatus::Failed;
        outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
        outcome.summary = Some("provider unavailable: account_unavailable".to_string());
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.provider_unavailable".to_string(),
            message: "selected provider account is unavailable".to_string(),
            data: serde_json::json!({"reason": "account_unavailable"}),
        });

        let failed = committed_harvest_failure(
            outcome,
            HarvestError::Git {
                command: "verify workspace content hash".to_string(),
                cwd: PathBuf::from("/lab/workspace"),
                message: "workspace content hash mismatch (expected abc, got def)".to_string(),
            },
        );

        assert_eq!(failed.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            failed.summary.as_deref(),
            Some("provider unavailable: account_unavailable")
        );
        assert!(failed.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.provider_unavailable"
                && diagnostic.data["reason"] == "account_unavailable"
        }));
        assert!(failed.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.committed_harvest_git_failed"
                && diagnostic.message.contains("content hash mismatch")
        }));
        assert!(!failed.artifacts.iter().any(is_actionable_patch_artifact));
    }

    #[test]
    fn changed_workspace_remains_fail_closed_during_harvest() {
        let mut outcome = committed_harvest_preflight_outcome("task-1".to_string());
        outcome.status = AgentTaskOutcomeStatus::Failed;
        outcome.summary = Some("provider unavailable: account_unavailable".to_string());

        let failed = committed_harvest_failure(
            outcome,
            HarvestError::DirtyWorkspace {
                status: " M src/lib.rs".to_string(),
            },
        );

        assert_eq!(failed.status, AgentTaskOutcomeStatus::Failed);
        assert!(failed
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("refusing committed-change harvest")));
        assert!(failed.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.committed_harvest_dirty_workspace"
                && diagnostic.data["status"] == " M src/lib.rs"
        }));
    }

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

    use homeboy_core::test_support::git_command_output as git;

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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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

    /// #15355: the provider's detached checkout must have its submodules
    /// before the provider starts, or a submodule/workspace repository can
    /// neither install nor build there.
    #[test]
    fn attempt_workspace_initializes_submodules_before_the_provider_starts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("library");
        std::fs::create_dir(&library).expect("library");
        git(&library, &["init", "--quiet", "-b", "main"]);
        git(&library, &["config", "user.email", "test@example.com"]);
        git(&library, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(library.join("lib.txt"), "library\n").expect("library file");
        git(&library, &["add", "lib.txt"]);
        git(&library, &["commit", "--quiet", "-m", "library"]);

        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "--quiet", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        git(
            &workspace,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                &library.display().to_string(),
                "vendor/library",
            ],
        );
        // Only the source checkout can satisfy the submodule.
        git(
            &workspace,
            &[
                "config",
                "--file",
                ".gitmodules",
                "submodule.vendor/library.url",
                "https://example.invalid/library.git",
            ],
        );
        git(&workspace, &["submodule", "sync", "--quiet"]);
        git(&workspace, &["add", "--all"]);
        git(&workspace, &["commit", "--quiet", "-m", "superproject"]);
        let head = git(&workspace, &["rev-parse", "HEAD"]);

        let mut request = gate_feedback_request(
            &workspace,
            String::new(),
            &temp.path().join("unused.patch"),
            "unused",
        );
        let scratch = tempfile::tempdir().expect("attempt scratch");
        prepare_attempt_workspace(&mut request, Some(&head), None, scratch.path())
            .expect("attempt workspace")
            .expect("isolated attempt");

        let attempt_root = PathBuf::from(request.workspace.root.as_deref().expect("attempt root"));
        assert_ne!(attempt_root, workspace);
        assert_eq!(
            std::fs::read_to_string(attempt_root.join("vendor/library/lib.txt"))
                .expect("submodule initialized in the attempt checkout"),
            "library\n"
        );
        let setup = &request.metadata["cook_attempt_workspace_setup"];
        assert_eq!(setup["schema"], "homeboy/cook-attempt-workspace-setup/v1");
        let submodules = setup["outcomes"]
            .as_array()
            .expect("setup outcomes")
            .iter()
            .find(|outcome| outcome["provider_id"] == "git-submodules")
            .expect("submodule setup evidence");
        assert_eq!(submodules["status"], "succeeded", "{setup}");
        assert_eq!(submodules["workspace"], "cook_attempt_workspace");
        // An initialized submodule at its pinned commit is not a change.
        assert_eq!(
            git(&attempt_root, &["status", "--porcelain"]),
            "",
            "setup must not dirty the attempt checkout"
        );
    }

    #[test]
    fn gate_feedback_baseline_allows_only_the_recorded_candidate_and_harvests_remediation_delta() {
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
        let base = git(&workspace, &["rev-parse", "HEAD"]);
        git(&workspace, &["add", "-A"]);
        let tree = git(&workspace, &["write-tree"]);
        let candidate = git(
            &workspace,
            &["commit-tree", &tree, "-p", &base, "-m", "candidate"],
        );
        git(&workspace, &["reset", "--mixed", &base]);
        request.inputs["cook_loop"]["candidate_commit"] =
            serde_json::json!({"base_sha":base,"candidate_sha":candidate,"tree_sha":tree});

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
        assert!(matches!(error, HarvestError::CandidateTreeMismatch { .. }));

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
        assert!(matches!(error, HarvestError::CandidateTreeMismatch { .. }));

        let mut ordinary =
            gate_feedback_request(&workspace, String::new(), &patch_artifact, &patch_sha256);
        ordinary.metadata = serde_json::Value::Null;
        let error = prepare_committed_harvest(&ordinary, None, &HarvestExecutionContext::default())
            .expect_err("ordinary dirty worktree remains refused");
        assert!(matches!(error, HarvestError::DirtyWorkspace { .. }));
    }

    #[test]
    fn uncommitted_harvest_ignores_the_linked_index_lock_and_preserves_commits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&source).expect("source");
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(source.join("file.txt"), "base\n").expect("base file");
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-m", "base"]);
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                workspace.to_str().expect("UTF-8 worktree path"),
                "HEAD",
            ],
        );
        let base = git(&workspace, &["rev-parse", "HEAD"]);

        // The provider commits one edit, then makes another uncommitted edit.
        std::fs::write(workspace.join("file.txt"), "provider change\n").expect("provider edit");
        git(&workspace, &["add", "file.txt"]);
        git(&workspace, &["commit", "-m", "provider commit"]);
        std::fs::write(workspace.join("late.txt"), "late provider change\n")
            .expect("late provider edit");
        let index_lock =
            PathBuf::from(git(&workspace, &["rev-parse", "--absolute-git-dir"])).join("index.lock");
        std::fs::write(&index_lock, b"held by provider").expect("hold linked-worktree index lock");

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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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
            rotation_transitions_used: 0,
            rotation_attempts: Vec::new(),
            readiness_skips: Vec::new(),
            provider_capacity_key: "test-capacity".to_string(),
            candidate_artifacts: Vec::new(),
            retry_attempts: Vec::new(),
            source_workspace_root: None,
            _attempt_workspace: None,
            run_id: Some("metadata-exclude-test".to_string()),
            artifact_root: None,
            artifact_nonce: "test-artifact".to_string(),
            task_base_sha: Some(base.clone()),
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
        harvest_candidate_commit(&mut outcome, &running).expect("harvest candidate");
        assert!(index_lock.exists());
        assert_eq!(git(&workspace, &["diff", "--cached", "--name-only"]), "");
        assert!(outcome.artifacts.is_empty());
        let candidate = outcome.candidate.unwrap();
        let diff = git(
            &workspace,
            &["diff", &candidate.base_sha, &candidate.candidate_sha],
        );
        assert!(diff.contains("+provider change"));
        assert!(diff.contains("+late provider change"));
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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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

    fn committed_harvest_running_task(
        temp: &Path,
        attempt_root: &Path,
        source_root: Option<&Path>,
        base: String,
    ) -> RunningTask {
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
                root: Some(attempt_root.display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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
            rotation_transitions_used: 0,
            rotation_attempts: Vec::new(),
            readiness_skips: Vec::new(),
            provider_capacity_key: "test-capacity".to_string(),
            candidate_artifacts: Vec::new(),
            retry_attempts: Vec::new(),
            source_workspace_root: source_root.map(|root| root.display().to_string()),
            _attempt_workspace: None,
            run_id: Some("reaped-harvest-test".to_string()),
            artifact_root: None,
            artifact_nonce: "test-artifact".to_string(),
            task_base_sha: Some(base),
            source_provenance: None,
            scratch: crate::controller_scratch::ControllerScratchAllocation {
                path: temp.join("controller-scratch"),
                lease_id: "test-lease-1".to_string(),
                index_path: temp.join("resources.json"),
            },
            adoption: None,
            join_handle: None,
        };
        std::fs::create_dir_all(&running.scratch.path).expect("scratch dir");
        running
    }

    #[test]
    fn committed_candidate_is_harvested_when_declared_output_is_invalid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("attempt-workspace");
        std::fs::create_dir(&workspace).expect("attempt workspace dir");
        git(&workspace, &["init", "--quiet", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join("file.txt"), "base\n").expect("base file");
        git(&workspace, &["add", "file.txt"]);
        git(&workspace, &["commit", "--quiet", "-m", "base"]);
        let base = git(&workspace, &["rev-parse", "HEAD"]);
        std::fs::write(workspace.join("file.txt"), "agent change\n").expect("agent edit");
        git(&workspace, &["commit", "--quiet", "-am", "agent change"]);
        let running = committed_harvest_running_task(temp.path(), &workspace, None, base);

        // The provider committed a real change, but its declared review form
        // missed the structural schema, so the executor reported a recoverable
        // candidate rather than success.
        let mut outcome = committed_harvest_preflight_outcome("reaped-task".to_string());
        outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
        outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
        outcome.summary =
            Some("output 'review_form' did not satisfy its declared schema".to_string());

        harvest_candidate_commit(&mut outcome, &running).expect("harvest candidate");
        assert!(outcome.artifacts.is_empty());
        let candidate = outcome.candidate.as_ref().unwrap();
        assert!(git(
            &workspace,
            &["diff", &candidate.base_sha, &candidate.candidate_sha]
        )
        .contains("agent change"));
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::CandidateRecoverable);
    }

    fn candidate_attempt_repo(temp: &Path) -> (PathBuf, String) {
        let workspace = temp.join("attempt-workspace");
        std::fs::create_dir(&workspace).expect("attempt workspace dir");
        git(&workspace, &["init", "--quiet", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join(".gitignore"), "ignored.txt\n").expect("gitignore");
        std::fs::write(workspace.join("file.txt"), "base\n").expect("base file");
        git(&workspace, &["add", "."]);
        git(&workspace, &["commit", "--quiet", "-m", "base"]);
        let base = git(&workspace, &["rev-parse", "HEAD"]);
        (workspace, base)
    }

    fn harvested_candidate(running: &RunningTask) -> Option<crate::agent_task::AgentTaskCandidate> {
        let mut outcome = committed_harvest_preflight_outcome(running.task_id.clone());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        harvest_candidate_commit(&mut outcome, running).expect("harvest candidate commit");
        outcome.candidate
    }

    /// `git write-tree` of the final worktree state, staged in the real index
    /// only after harvest so the oracle cannot share harvest's mechanism.
    fn final_worktree_tree(workspace: &Path) -> String {
        git(workspace, &["add", "--all"]);
        git(workspace, &["write-tree"])
    }

    #[test]
    fn dirty_attempt_harvests_one_candidate_commit_without_touching_the_provider_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace, base) = candidate_attempt_repo(temp.path());
        std::fs::write(workspace.join("file.txt"), "staged\n").expect("staged edit");
        git(&workspace, &["add", "file.txt"]);
        std::fs::write(workspace.join("file.txt"), "agent change\n").expect("modified");
        std::fs::write(workspace.join("new.txt"), "untracked\n").expect("untracked");
        std::fs::write(workspace.join("ignored.txt"), "ignored\n").expect("ignored");
        let index_before = std::fs::read(workspace.join(".git/index")).expect("index");
        let running = committed_harvest_running_task(temp.path(), &workspace, None, base.clone());

        let candidate = harvested_candidate(&running).expect("dirty attempt has a candidate");

        assert_eq!(
            std::fs::read(workspace.join(".git/index")).expect("index"),
            index_before,
            "harvest must never write the provider's index (#13448)"
        );
        assert_eq!(candidate.base_sha, base);
        let head = git(&workspace, &["rev-parse", "HEAD"]);
        assert_eq!(
            git(
                &workspace,
                &["rev-parse", &format!("{}^", candidate.candidate_sha)]
            ),
            head
        );
        assert_eq!(
            git(
                &workspace,
                &["rev-parse", "refs/homeboy/candidates/reaped-harvest-test"]
            ),
            candidate.candidate_sha
        );
        assert_eq!(
            git(
                &workspace,
                &[
                    "rev-parse",
                    &format!("{}^{{tree}}", candidate.candidate_sha)
                ]
            ),
            candidate.tree_sha
        );
        assert_eq!(candidate.tree_sha, final_worktree_tree(&workspace));
        assert_eq!(
            git(
                &workspace,
                &["show", &format!("{}:new.txt", candidate.candidate_sha)]
            ),
            "untracked"
        );
    }

    #[test]
    fn timed_out_runner_harvest_bundles_dirty_candidate_for_a_base_only_controller() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace, base) = candidate_attempt_repo(temp.path());
        let controller = temp.path().join("controller");
        git(
            temp.path(),
            &["clone", "-q", workspace.to_str().unwrap(), "controller"],
        );
        std::fs::write(workspace.join("file.txt"), "agent change\n").expect("dirty");
        std::fs::write(workspace.join("new.txt"), "untracked\n").expect("untracked");
        let running = committed_harvest_running_task(temp.path(), &workspace, None, base);
        let mut outcome = committed_harvest_preflight_outcome(running.task_id.clone());

        outcome.status = AgentTaskOutcomeStatus::Timeout;
        outcome.failure_classification =
            Some(crate::agent_task::AgentTaskFailureClassification::Timeout);
        harvest_candidate_commit_with_bundle(&mut outcome, &running, true).expect("harvest");
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);

        let candidate = outcome.candidate.clone().expect("dirty attempt candidate");
        let bundle = outcome
            .artifacts
            .iter()
            .find(|artifact| {
                artifact.kind == crate::agent_task_promotion::CANDIDATE_BUNDLE_ARTIFACT_KIND
            })
            .expect("runner harvest returns the candidate bundle artifact");
        let path = Path::new(bundle.path.as_deref().expect("bundle path"));
        let bytes = std::fs::read(path).expect("bundle bytes");
        assert_eq!(bundle.size_bytes, Some(bytes.len() as u64));
        assert_eq!(
            bundle.sha256.as_deref(),
            Some(patch_sha256(&bytes).as_str())
        );
        let local_ref = crate::agent_task_promotion::fetch_candidate_bundle(
            &controller,
            path,
            "reaped-harvest-test",
            &candidate,
        )
        .expect("base-only controller fetches the candidate");
        assert_eq!(
            git(&controller, &["rev-parse", &local_ref]),
            candidate.candidate_sha
        );
        let tree = git(
            &controller,
            &["rev-parse", &format!("{local_ref}^{{tree}}")],
        );
        assert_eq!(tree, candidate.tree_sha);
        crate::agent_task_promotion::materialize_candidate_tree(&controller, &candidate)
            .expect("materialize");
        git(&controller, &["add", "--all"]);
        assert_eq!(git(&controller, &["write-tree"]), candidate.tree_sha);
        assert_eq!(git(&controller, &["rev-parse", "HEAD"]), candidate.base_sha);
    }

    #[test]
    fn committed_attempt_candidate_keeps_provider_commits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace, base) = candidate_attempt_repo(temp.path());
        std::fs::write(workspace.join("file.txt"), "agent change\n").expect("agent edit");
        git(&workspace, &["commit", "--quiet", "-am", "agent change"]);
        let head = git(&workspace, &["rev-parse", "HEAD"]);
        let index_before = std::fs::read(workspace.join(".git/index")).expect("index");
        let running = committed_harvest_running_task(temp.path(), &workspace, None, base.clone());

        let candidate = harvested_candidate(&running).expect("committed attempt has a candidate");

        assert_eq!(
            std::fs::read(workspace.join(".git/index")).expect("index"),
            index_before
        );
        assert_eq!(candidate.base_sha, base);
        assert_eq!(
            git(
                &workspace,
                &["rev-parse", &format!("{}^", candidate.candidate_sha)]
            ),
            head
        );
        assert_eq!(candidate.tree_sha, final_worktree_tree(&workspace));
    }

    #[test]
    fn harvesting_the_same_state_twice_yields_the_same_candidate_sha() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace, base) = candidate_attempt_repo(temp.path());
        std::fs::write(workspace.join("new.txt"), "untracked\n").expect("untracked");
        let running = committed_harvest_running_task(temp.path(), &workspace, None, base);

        let first = harvested_candidate(&running).expect("first harvest");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = harvested_candidate(&running).expect("second harvest");

        assert_eq!(first, second);
    }

    #[test]
    fn unchanged_attempt_has_no_candidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace, base) = candidate_attempt_repo(temp.path());
        std::fs::write(workspace.join("ignored.txt"), "ignored\n").expect("ignored");
        let running = committed_harvest_running_task(temp.path(), &workspace, None, base);
        let mut outcome = committed_harvest_preflight_outcome(running.task_id.clone());
        outcome.candidate = Some(crate::agent_task::AgentTaskCandidate {
            base_sha: "provider".to_string(),
            candidate_sha: "asserted".to_string(),
            tree_sha: "candidate".to_string(),
        });

        harvest_candidate_commit(&mut outcome, &running).expect("harvest");

        assert_eq!(outcome.candidate, None);
        assert!(git(&workspace, &["for-each-ref", "refs/homeboy/candidates"]).is_empty());
    }

    #[test]
    fn committed_harvest_falls_back_to_source_when_attempt_scratch_is_reaped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source, base) = candidate_attempt_repo(temp.path());
        std::fs::write(source.join("file.txt"), "agent change\n").expect("agent edit");
        git(&source, &["commit", "--quiet", "-am", "agent change"]);
        let source_head = git(&source, &["rev-parse", "HEAD"]);
        // Uncommitted source state is the operator's, never the candidate's.
        std::fs::write(source.join("operator.txt"), "operator\n").expect("operator dirt");
        let reaped_scratch = temp.path().join("reaped-attempt-workspace");
        assert!(!reaped_scratch.exists());
        let running = committed_harvest_running_task(
            temp.path(),
            &reaped_scratch,
            Some(&source),
            base.clone(),
        );

        let mut outcome = committed_harvest_preflight_outcome(running.task_id.clone());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        harvest_candidate_commit(&mut outcome, &running).expect("harvest falls back to source");

        let candidate = outcome
            .candidate
            .clone()
            .expect("source commit is the candidate");
        assert_eq!(candidate.base_sha, base);
        assert_eq!(candidate.candidate_sha, source_head);
        assert_eq!(
            candidate.tree_sha,
            git(&source, &["rev-parse", &format!("{source_head}^{{tree}}")])
        );
        assert_eq!(
            git(
                &source,
                &["rev-parse", "refs/homeboy/candidates/reaped-harvest-test"]
            ),
            source_head
        );
        assert!(git(
            &source,
            &["ls-tree", "--name-only", &candidate.candidate_sha]
        )
        .lines()
        .all(|name| name != "operator.txt"));
        assert_eq!(
            outcome.metadata["candidate_harvest_root"]["kind"],
            "source_workspace"
        );
        assert!(outcome.artifacts.is_empty(), "no patch artifact");
    }

    #[test]
    fn reaped_attempt_with_no_source_commits_has_no_candidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source, base) = candidate_attempt_repo(temp.path());
        let not_a_repo = temp.path().join("plain-dir");
        std::fs::create_dir(&not_a_repo).expect("plain dir");
        let running = committed_harvest_running_task(temp.path(), &not_a_repo, Some(&source), base);

        let mut outcome = committed_harvest_preflight_outcome(running.task_id.clone());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        harvest_candidate_commit(&mut outcome, &running).expect("harvest");

        assert_eq!(outcome.candidate, None);
        assert!(git(&source, &["for-each-ref", "refs/homeboy/candidates"]).is_empty());
    }
}
