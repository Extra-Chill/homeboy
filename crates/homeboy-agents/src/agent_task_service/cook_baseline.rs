//! Agent-task cook follow-up baseline materialization.
//!
//! Extracted from `cook.rs`: the process-local machinery that materializes the
//! git baseline a cook retry runs against. `CookFollowUpBaseline` owns a
//! detached `git worktree` (cleaned up in its `Drop`), `DerivedCookBaselineCapability`
//! is the non-serializable controller-validated authority derived from it, and
//! `materialize_initial_candidate_baseline`/`materialize_follow_up_baseline`
//! build them from a source root or a prior promotion. The private `git_output`
//! helpers live here because this cluster is their only user.

use serde_json::Value;
use sha2::Digest;
use std::path::PathBuf;
use std::process::Command;

use crate::agent_task_promotion::{normalize_promotion_patch, AgentTaskPromotionReport};
use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{Error, Result};

/// A cook-owned detached checkout turns the already-promoted dirty candidate
/// into a clean commit before the scheduler creates its normal attempt checkout.
pub(crate) struct CookFollowUpBaseline {
    source_root: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) capability: DerivedCookBaselineCapability,
}

pub(crate) fn cook_attempt_harvest_context(
    harvest_context: &crate::agent_task_scheduler::HarvestExecutionContext,
) -> crate::agent_task_scheduler::HarvestExecutionContext {
    harvest_context.clone()
}

/// Process-local authority for one materialized cook retry baseline. It is not
/// serializable and never enters a request, environment, or durable record.
pub struct DerivedCookBaselineCapability {
    canonical_path: PathBuf,
    commit: String,
    tree: String,
    artifact_sha256: String,
    source_run_id: String,
    source_task_id: String,
    bound_task_id: String,
    parent_snapshot: Option<Value>,
    preexisting_candidate: bool,
}

impl DerivedCookBaselineCapability {
    pub fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }

    pub(crate) fn commit(&self) -> &str {
        &self.commit
    }

    pub(crate) fn tree(&self) -> &str {
        &self.tree
    }

    pub(crate) fn bound_task_id(&self) -> &str {
        &self.bound_task_id
    }

    pub(crate) fn parent_snapshot(&self) -> Option<&Value> {
        self.parent_snapshot.as_ref()
    }

    pub(crate) fn artifact_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "source_patch_artifact_sha256": self.artifact_sha256,
        })
    }

    /// Evidence derived from the controller-validated capability. It is not
    /// authorization for remote workspace or snapshot verification.
    pub fn verified_baseline_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "promoted_patch_artifact_sha256": self.artifact_sha256,
            "baseline_commit": self.commit,
            "baseline_tree": self.tree,
            "parent_snapshot_identity": self.parent_snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .get("workspace_snapshot_identity")
                    .cloned()
                    .or_else(|| snapshot.get("identity").cloned())
            }),
            "preexisting_candidate": self.preexisting_candidate,
        })
    }
}

impl CookFollowUpBaseline {
    pub(crate) fn capability(&self) -> &DerivedCookBaselineCapability {
        &self.capability
    }

    pub(crate) fn artifact_provenance(&self) -> Value {
        self.capability.artifact_provenance()
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_derived_cook_baseline_capability(
    path: PathBuf,
    commit: String,
    tree: String,
    task_id: &str,
    parent_snapshot: Option<Value>,
) -> DerivedCookBaselineCapability {
    DerivedCookBaselineCapability {
        canonical_path: path
            .canonicalize()
            .expect("test baseline path canonicalizes"),
        commit,
        tree,
        artifact_sha256: "test-artifact-sha256".to_string(),
        source_run_id: "test-source-run".to_string(),
        source_task_id: task_id.to_string(),
        bound_task_id: task_id.to_string(),
        parent_snapshot,
        preexisting_candidate: false,
    }
}

/// Materialize a Cook-declared dirty candidate in a detached checkout before
/// provider dispatch. The caller workspace is never staged, reset, or edited.
pub(crate) fn materialize_initial_candidate_baseline(
    plan: &AgentTaskPlan,
    source_root: Option<&std::path::Path>,
    source_run_id: &str,
) -> Result<Option<CookFollowUpBaseline>> {
    let Some(source_root) = source_root else {
        return Ok(None);
    };
    let status = git_output(
        source_root,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    if status.is_empty() {
        return Ok(None);
    }
    let task_id = plan
        .tasks
        .first()
        .map(|task| task.task_id.as_str())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "plan.tasks",
                "Cook cannot adopt a dirty candidate without a provider task",
                None,
                None,
            )
        })?;
    if plan.tasks.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "plan.tasks",
            "Cook can adopt a pre-existing candidate only for a single provider task",
            None,
            Some(vec![
                "Run one Cook task per dirty candidate workspace.".to_string()
            ]),
        ));
    }
    let base = git_output(source_root, &["rev-parse", "HEAD"])?;
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook candidate Git index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    git_output_with_env(
        source_root,
        &["read-tree", &base],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    git_output_with_env(
        source_root,
        &["add", "--all"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let tree = git_output_with_env(
        source_root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let commit = git_output_with_env(
        source_root,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "homeboy: Cook pre-existing candidate baseline",
        ],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let parent = std::env::temp_dir().join("homeboy-cook-initial-baselines");
    std::fs::create_dir_all(&parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook candidate baseline directory".to_string()),
        )
    })?;
    let path = parent.join(format!("baseline-{}", uuid::Uuid::new_v4()));
    let path_string = path.display().to_string();
    git_output(
        source_root,
        &["worktree", "add", "--detach", &path_string, &commit],
    )?;
    let canonical_path = path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook candidate baseline".to_string()),
        )
    })?;
    Ok(Some(CookFollowUpBaseline {
        source_root: source_root.to_path_buf(),
        path,
        capability: DerivedCookBaselineCapability {
            canonical_path,
            commit,
            tree: tree.clone(),
            artifact_sha256: format!("{:x}", sha2::Sha256::digest(tree.as_bytes())),
            source_run_id: source_run_id.to_string(),
            source_task_id: task_id.to_string(),
            bound_task_id: task_id.to_string(),
            parent_snapshot: None,
            preexisting_candidate: true,
        },
    }))
}

impl Drop for CookFollowUpBaseline {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.source_root)
            .status();
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_root)
            .status();
    }
}

pub(crate) fn materialize_follow_up_baseline(
    promotion: &AgentTaskPromotionReport,
    source_run_id: &str,
    bound_task_id: &str,
) -> Result<CookFollowUpBaseline> {
    let source_root = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "gate-failed promotion did not report its managed target workspace",
                None,
                None,
            )
        })?;
    let expected_head = promotion.target.head.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "promotion.target.head",
            "gate-failed promotion did not record the immutable target HEAD",
            None,
            None,
        )
    })?;
    if git_output(&source_root, &["rev-parse", "HEAD"])? != expected_head {
        return Err(Error::validation_invalid_argument(
            "promotion.target.head",
            "promotion target HEAD changed after the gate-failed promotion; refusing cook retry baseline",
            None,
            None,
        ));
    }
    let parent_snapshot = std::env::var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV)
        .ok()
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument("source_snapshot", error.to_string(), None, None)
        })?;
    let artifact_bytes = std::fs::read(&promotion.patch_artifact.path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read promoted patch artifact".to_string()),
        )
    })?;
    let artifact_sha256 = format!("{:x}", sha2::Sha256::digest(&artifact_bytes));
    if let Some(expected) = promotion.patch_artifact.sha256.as_deref() {
        if expected != artifact_sha256 {
            return Err(Error::validation_invalid_argument(
                "promotion.patch_artifact.sha256",
                "promoted artifact bytes no longer match durable sha256",
                None,
                None,
            ));
        }
    }
    let artifact = std::str::from_utf8(&artifact_bytes).map_err(|error| {
        Error::validation_invalid_argument(
            "promotion.patch_artifact",
            format!("patch bytes are not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    // A provider patch is relative to an adopted dirty candidate, while the
    // retry checkout starts at the clean target HEAD. Reconstruct from the
    // controller-recorded complete candidate diff when available.
    let complete_candidate = promotion
        .provenance
        .pointer("/gate_feedback_baseline/current_diff")
        .and_then(Value::as_str)
        .filter(|diff| !diff.trim().is_empty())
        .unwrap_or(artifact);
    let normalized = normalize_promotion_patch(complete_candidate, &promotion.to_worktree)?;
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create cook baseline Git index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    git_output_with_env(
        &source_root,
        &["read-tree", expected_head],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    git_output_with_env(
        &source_root,
        &["add", "--all"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let target_tree = git_output_with_env(
        &source_root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let parent = std::env::temp_dir().join("homeboy-cook-follow-up-baselines");
    std::fs::create_dir_all(&parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create cook baseline directory".to_string()),
        )
    })?;
    let path = parent.join(format!("baseline-{}", uuid::Uuid::new_v4()));
    let path_string = path.display().to_string();
    git_output(
        &source_root,
        &["worktree", "add", "--detach", &path_string, expected_head],
    )?;
    let baseline = CookFollowUpBaseline {
        source_root,
        path: path.clone(),
        // The capability is completed only after the committed baseline's
        // identity has been verified below.
        capability: DerivedCookBaselineCapability {
            canonical_path: path,
            commit: String::new(),
            tree: String::new(),
            artifact_sha256,
            source_run_id: source_run_id.to_string(),
            source_task_id: promotion.source.task_id.clone(),
            bound_task_id: bound_task_id.to_string(),
            parent_snapshot,
            preexisting_candidate: false,
        },
    };
    let head_tree = git_output(&baseline.path, &["rev-parse", "HEAD^{tree}"])?;
    let (commit, tree) = if head_tree == target_tree {
        (expected_head.to_string(), head_tree)
    } else {
        let patch_path = baseline.path.join(".homeboy-cook-baseline.patch");
        std::fs::write(&patch_path, normalized.content.as_bytes()).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write cook baseline patch".to_string()),
            )
        })?;
        git_output(
            &baseline.path,
            &[
                "apply",
                "--whitespace=nowarn",
                &patch_path.display().to_string(),
            ],
        )?;
        std::fs::remove_file(&patch_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("remove cook baseline patch".to_string()),
            )
        })?;
        git_output(&baseline.path, &["add", "--all"])?;
        git_output(
            &baseline.path,
            &[
                "-c",
                "user.name=Homeboy",
                "-c",
                "user.email=homeboy@localhost",
                "commit",
                "--no-verify",
                "-m",
                "homeboy: cook promoted baseline",
            ],
        )?;
        (
            git_output(&baseline.path, &["rev-parse", "HEAD"])?,
            git_output(&baseline.path, &["rev-parse", "HEAD^{tree}"])?,
        )
    };
    if tree != target_tree {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion target contains extra, missing, or unrelated changes; refusing cook retry baseline",
            None,
            None,
        ));
    }
    let mut baseline = baseline;
    baseline.capability.canonical_path = baseline.path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize cook retry baseline".to_string()),
        )
    })?;
    baseline.capability.commit = commit;
    baseline.capability.tree = tree;
    Ok(baseline)
}

pub(crate) fn git_output(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    git_output_with_env(cwd, args, &[])
}

pub(crate) fn git_output_with_env(
    cwd: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("git {}", args.join(" "))))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
