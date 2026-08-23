//! Agent-task cook follow-up baseline materialization.
//!
//! Extracted from `cook.rs`: the process-local machinery that materializes the
//! git baseline a cook retry runs against. `CookFollowUpBaseline` owns a
//! detached `git worktree` (cleaned up in its `Drop`), `DerivedCookBaselineCapability`
//! is the non-serializable controller-validated authority derived from it, and
//! `materialize_initial_candidate_baseline`/`materialize_follow_up_baseline`
//! build them from a source root or a prior promotion. The private `git_output`
//! helpers live here because this cluster is their only user.

use homeboy_engine_primitives::content_hash;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

use crate::agent_task_gate::{
    failure_fingerprint, run_gate_command_with_timeout, AgentTaskGateBaselineComparison,
    AgentTaskGateDifferentialResult, AgentTaskGateStatus,
};
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

    /// A lifecycle retry persists this controller-materialized checkout for a
    /// later provider dispatch. Unlike the in-process Cook path, no owner is
    /// available to remove it when this function returns.
    pub(crate) fn preserve_for_retry(self) {
        std::mem::forget(self);
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
    // A Cook baseline is a full `git worktree` checkout of the candidate tree.
    // Placing it in `std::env::temp_dir()` put a whole working tree outside
    // every cleanup surface -- no owner record, no pin, no run binding, and
    // invisible to the retained-storage report -- and broke outright on a
    // `noexec` `/tmp` (#11128). The artifact root is the volume the operator
    // already sized for run bytes.
    let parent = homeboy_core::artifacts::root()?.join("cook-baseline");
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
            artifact_sha256: content_hash::sha256_hex(tree.as_bytes()),
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
    materialize_follow_up_baseline_at(promotion, None, None, source_run_id, bound_task_id)
}

pub(crate) fn materialize_follow_up_baseline_in_root(
    promotion: &AgentTaskPromotionReport,
    artifact_root: &std::path::Path,
    source_run_id: &str,
    bound_task_id: &str,
) -> Result<CookFollowUpBaseline> {
    materialize_follow_up_baseline_at(
        promotion,
        Some(artifact_root),
        None,
        source_run_id,
        bound_task_id,
    )
}

/// Replay unresolved failed gates against the immutable verified base. This is
/// controller work, not provider remediation: callers persist the resulting
/// promotion before evaluating Cook feedback.
pub(crate) fn compare_gate_failures_to_verified_base(
    promotion: &mut AgentTaskPromotionReport,
    repository_root: &std::path::Path,
    gate_workspace: &std::path::Path,
    base_sha: &str,
    timeout: std::time::Duration,
    mut checkpoint: impl FnMut(usize, usize) -> Result<()>,
) -> Result<()> {
    if !promotion.status.gate_failed() {
        return Ok(());
    }
    let unresolved = promotion
        .deterministic_gates
        .iter()
        .filter(|gate| {
            gate.status == AgentTaskGateStatus::Failed && gate.baseline_comparison.is_none()
        })
        .count();
    if unresolved == 0 {
        return Ok(());
    }
    let repository_root = repository_root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook gate repository root".to_string()),
        )
    })?;
    let gate_workspace = gate_workspace.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook gate workspace".to_string()),
        )
    })?;
    let baseline_root = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook gate baseline".to_string()),
        )
    })?;
    let baseline_path = baseline_root.path().join("base");
    let gate_workspace_relative = gate_workspace.strip_prefix(&repository_root).map_err(|_| {
        Error::validation_invalid_argument(
            "gate_workspace",
            "Cook gate workspace is outside its repository root",
            Some(gate_workspace.display().to_string()),
            None,
        )
    })?;
    let output = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&baseline_path)
        .arg(base_sha)
        .current_dir(&repository_root)
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("materialize Cook gate baseline".to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_io(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
            Some("materialize immutable Cook gate baseline".to_string()),
        ));
    }
    let comparison = (|| -> Result<()> {
        let canonical_baseline_path = baseline_path.canonicalize().map_err(|error| {
            Error::validation_invalid_argument(
                "gate_workspace",
                format!("canonicalize Cook gate baseline root: {error}"),
                Some(baseline_path.display().to_string()),
                None,
            )
        })?;
        let baseline_gate_workspace = homeboy_core::resolve_contained_local_path(
            &canonical_baseline_path,
            gate_workspace_relative,
            "gate_workspace",
        )?;
        let baseline_gate_workspace = baseline_gate_workspace.canonicalize().map_err(|error| {
            Error::validation_invalid_argument(
                "gate_workspace",
                format!("canonicalize Cook baseline component workspace: {error}"),
                Some(baseline_gate_workspace.display().to_string()),
                None,
            )
        })?;
        if !baseline_gate_workspace.starts_with(&canonical_baseline_path) {
            return Err(Error::validation_invalid_argument(
                "gate_workspace",
                "canonical Cook baseline component workspace escapes its baseline root",
                Some(baseline_gate_workspace.display().to_string()),
                None,
            ));
        }
        homeboy_core::hygiene::materialize_worktree_dependencies(&baseline_gate_workspace)?;
        let mut compared = 0;
        for (index, gate) in promotion.deterministic_gates.iter_mut().enumerate() {
            if gate.status != AgentTaskGateStatus::Failed || gate.baseline_comparison.is_some() {
                continue;
            }
            compared += 1;
            checkpoint(compared, unresolved)?;
            let Some(package_artifacts) = gate.environment.package_artifact_replay_requirements()
            else {
                gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                    base_ref: base_sha.to_string(),
                    exit_code: 124,
                    failure_fingerprint: String::new(),
                    matches_candidate_failure: false,
                    result: AgentTaskGateDifferentialResult::Inconclusive,
                });
                continue;
            };
            let command = gate.command.last().cloned().unwrap_or_default();
            let baseline_run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
            let baseline = (|| {
                let runtime = homeboy_core::engine::invocation::InvocationGuard::acquire(
                    &baseline_run_dir,
                    &homeboy_core::engine::invocation::InvocationRequirements::default(),
                )?;
                run_gate_command_with_timeout(
                    &baseline_gate_workspace,
                    index + 1,
                    &command,
                    gate.visibility,
                    gate.reveal_policy,
                    &runtime.context().tmp_dir,
                    timeout,
                    &gate.environment.replay_policy(),
                    &package_artifacts,
                )
            })();
            if let Ok(baseline) = &baseline {
                if baseline.environment.package_artifacts != gate.environment.package_artifacts {
                    gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: String::new(),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::Inconclusive,
                    });
                    baseline_run_dir.finish(true);
                    continue;
                }
            }
            let result: Result<()> = match baseline {
                Ok(baseline) if baseline.exit_code == 124 => {
                    gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: failure_fingerprint(
                            baseline.exit_code,
                            &baseline.stdout,
                            &baseline.stderr,
                            baseline
                                .failure_evidence
                                .as_ref()
                                .map(|evidence| evidence.diagnostics.as_slice())
                                .unwrap_or_default(),
                        ),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::Inconclusive,
                    });
                    Ok(())
                }
                Ok(baseline) if baseline.status == AgentTaskGateStatus::Failed => {
                    let matches = failure_fingerprint(
                        gate.exit_code,
                        &gate.stdout,
                        &gate.stderr,
                        gate.failure_evidence
                            .as_ref()
                            .map(|evidence| evidence.diagnostics.as_slice())
                            .unwrap_or_default(),
                    ) == failure_fingerprint(
                        baseline.exit_code,
                        &baseline.stdout,
                        &baseline.stderr,
                        baseline
                            .failure_evidence
                            .as_ref()
                            .map(|evidence| evidence.diagnostics.as_slice())
                            .unwrap_or_default(),
                    );
                    gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: failure_fingerprint(
                            baseline.exit_code,
                            &baseline.stdout,
                            &baseline.stderr,
                            baseline
                                .failure_evidence
                                .as_ref()
                                .map(|evidence| evidence.diagnostics.as_slice())
                                .unwrap_or_default(),
                        ),
                        matches_candidate_failure: matches,
                        result: if matches {
                            AgentTaskGateDifferentialResult::BaselineRed
                        } else {
                            AgentTaskGateDifferentialResult::CandidateRegression
                        },
                    });
                    if matches {
                        gate.accept_inherited_failure();
                    }
                    Ok(())
                }
                Ok(baseline) => {
                    gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: failure_fingerprint(
                            baseline.exit_code,
                            &baseline.stdout,
                            &baseline.stderr,
                            baseline
                                .failure_evidence
                                .as_ref()
                                .map(|evidence| evidence.diagnostics.as_slice())
                                .unwrap_or_default(),
                        ),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::CandidateRegression,
                    });
                    Ok(())
                }
                Err(error) => {
                    gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: 124,
                        failure_fingerprint: String::new(),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::Inconclusive,
                    });
                    // Preserve the inconclusive evidence and leave the candidate
                    // gate red. A baseline execution failure must never convert a
                    // candidate failure into an infrastructure error that loses
                    // its durable comparison result.
                    let _ = error;
                    Ok(())
                }
            };
            baseline_run_dir.finish(result.is_ok());
            result?;
        }
        promotion.normalize_gate_outcome();
        Ok(())
    })();
    let cleanup = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&baseline_path)
        .current_dir(&repository_root)
        .status()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("remove Cook gate baseline".to_string()),
            )
        })?;
    if !cleanup.success() {
        return Err(Error::internal_io(
            "git worktree remove failed".to_string(),
            Some("remove Cook gate baseline".to_string()),
        ));
    }
    comparison
}

/// Re-materialize a follow-up baseline worktree at a specific path that was
/// previously created by [`materialize_follow_up_baseline`] but has since been
/// reaped (e.g. by tmp cleanup, disk-pressure cleanup, or `git worktree prune`).
///
/// This reuses the same durable promotion data and worktree/patch logic as
/// [`materialize_follow_up_baseline`] to deterministically recreate an identical
/// worktree at the original path, preserving the baseline identity contract for
/// the provider preflight and subsequent gate verification.
pub(crate) fn re_materialize_follow_up_baseline(
    promotion: &AgentTaskPromotionReport,
    target_path: &std::path::Path,
    source_run_id: &str,
    bound_task_id: &str,
) -> Result<CookFollowUpBaseline> {
    materialize_follow_up_baseline_at(
        promotion,
        None,
        Some(target_path),
        source_run_id,
        bound_task_id,
    )
}

/// Shared worktree/patch materialization logic. When `target_path` is `None` a
/// fresh UUID path is generated under the follow-up baselines temp directory;
/// when `Some(path)` the worktree is created at the exact given path (used by
/// recovery to re-materialize a reaped baseline at its original location).
fn materialize_follow_up_baseline_at(
    promotion: &AgentTaskPromotionReport,
    artifact_root: Option<&std::path::Path>,
    target_path: Option<&std::path::Path>,
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
    let parent_snapshot = parent_snapshot_from_current_process()?;
    let artifact_bytes = std::fs::read(&promotion.patch_artifact.path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read promoted patch artifact".to_string()),
        )
    })?;
    let artifact_sha256 = content_hash::sha256_hex(&artifact_bytes);
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
    let path = match target_path {
        Some(p) => p.to_path_buf(),
        None => {
            // Same reasoning as the initial-candidate baseline above: a whole
            // working-tree checkout belongs under the artifact root, not in
            // the process temp dir where nothing can see or reap it (#11128).
            let root = match artifact_root {
                Some(root) => root.to_path_buf(),
                None => homeboy_core::artifacts::root()?,
            };
            let parent = root.join("cook-baseline");
            std::fs::create_dir_all(&parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("create cook baseline directory".to_string()),
                )
            })?;
            parent.join(format!("baseline-{}", uuid::Uuid::new_v4()))
        }
    };
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
        git_output_with_env(
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
            &[
                ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z"),
                ("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z"),
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

fn parent_snapshot_from_current_process() -> Result<Option<Value>> {
    let Some(raw) = std::env::var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV).ok()
    else {
        return Ok(None);
    };
    parent_snapshot_from_transport(&raw).map(Some)
}

fn parent_snapshot_from_transport(raw: &str) -> Result<Value> {
    homeboy_core::observation::resolve_json_value(raw).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source_snapshot",
            "invalid inline or referenced source snapshot JSON",
            None,
            None,
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_gate::{
        AgentTaskGateEnvironment, AgentTaskGateFailureClassification, AgentTaskGateFailureEvidence,
        AgentTaskGateRevealPolicy, AgentTaskGateStatus,
    };
    use homeboy_core::gate::HomeboyGateVisibility;
    use sha2::{Digest, Sha256};

    #[test]
    fn cook_baseline_resolves_referenced_parent_snapshot() {
        let directory = tempfile::tempdir().expect("transport directory");
        let path = directory.path().join("source.json");
        let payload = br#"{"schema":"fixture/source/v1","snapshot_hash":"abc"}"#;
        std::fs::write(&path, payload).expect("write source snapshot");
        let reference = serde_json::json!({
            "schema": homeboy_core::observation::PROVENANCE_REFERENCE_SCHEMA,
            "path": path,
            "sha256": format!("{:x}", Sha256::digest(payload)),
        })
        .to_string();

        assert_eq!(
            parent_snapshot_from_transport(&reference).expect("referenced parent snapshot")
                ["snapshot_hash"],
            "abc"
        );
    }

    #[test]
    fn differential_gate_comparison_classifies_inherited_regression_and_inconclusive() {
        for (name, command, candidate_output, timeout, expected, accepted) in [
            (
                "inherited",
                "printf 'same\\n' >&2; exit 1",
                "same\n",
                std::time::Duration::from_secs(1),
                AgentTaskGateDifferentialResult::BaselineRed,
                true,
            ),
            (
                "regression",
                "cat state >&2; exit 1",
                "candidate\n",
                std::time::Duration::from_secs(1),
                AgentTaskGateDifferentialResult::CandidateRegression,
                false,
            ),
            (
                "inconclusive",
                "sleep 1; exit 1",
                "candidate\n",
                std::time::Duration::from_millis(10),
                AgentTaskGateDifferentialResult::Inconclusive,
                false,
            ),
        ] {
            let temp = tempfile::tempdir().expect("repository");
            git_output(temp.path(), &["init", "-b", "main"]).expect("init");
            git_output(temp.path(), &["config", "user.name", "Homeboy Test"]).expect("name");
            git_output(temp.path(), &["config", "user.email", "test@example.test"]).expect("email");
            std::fs::write(temp.path().join("state"), "base\n").expect("base state");
            git_output(temp.path(), &["add", "state"]).expect("add");
            git_output(temp.path(), &["commit", "-m", "base"]).expect("commit");
            let base = git_output(temp.path(), &["rev-parse", "HEAD"]).expect("base sha");
            std::fs::write(temp.path().join("state"), "candidate\n").expect("candidate state");
            let mut promotion: AgentTaskPromotionReport =
                serde_json::from_value(serde_json::json!({
                    "schema": "homeboy/agent-task-promotion-report/v1",
                    "status": "gate_failed",
                    "source": {"kind": "aggregate", "task_id": "task"},
                    "to_worktree": "fixture",
                    "target": {"worktree": "fixture", "path": temp.path()},
                    "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
                    "deterministic_gates": [],
                    "operator_notification": {"status": "blocked", "message": "red"}
                }))
                .expect("promotion");
            let mut gate = crate::agent_task_gate::AgentTaskGateReport::new(
                name,
                vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
                1,
                "",
                candidate_output,
                None,
                HomeboyGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                AgentTaskGateEnvironment::default(),
            );
            gate.status = AgentTaskGateStatus::Failed;
            promotion.deterministic_gates.push(gate);

            let result = compare_gate_failures_to_verified_base(
                &mut promotion,
                temp.path(),
                temp.path(),
                &base,
                timeout,
                |_compared, _total| Ok(()),
            );

            result.expect("baseline comparison");
            assert_eq!(
                promotion.deterministic_gates[0]
                    .baseline_comparison
                    .as_ref()
                    .expect("comparison")
                    .result,
                expected
            );
            assert_eq!(
                promotion.deterministic_gates[0].status
                    == AgentTaskGateStatus::AcceptedInheritedFailure,
                accepted
            );
            assert_eq!(
                promotion.status,
                crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed
            );
        }
    }

    #[test]
    fn differential_gate_comparison_accepts_an_identical_missing_monorepo_path() {
        let temp = tempfile::tempdir().expect("repository");
        git_output(temp.path(), &["init", "-b", "main"]).expect("init");
        git_output(temp.path(), &["config", "user.name", "Homeboy Test"]).expect("name");
        git_output(temp.path(), &["config", "user.email", "test@example.test"]).expect("email");
        std::fs::write(temp.path().join("README"), "base\n").expect("base file");
        git_output(temp.path(), &["add", "README"]).expect("add");
        git_output(temp.path(), &["commit", "-m", "base"]).expect("commit");
        let base = git_output(temp.path(), &["rev-parse", "HEAD"]).expect("base sha");

        let command = "cd packages/missing-component && cargo test";
        let candidate = std::process::Command::new("sh")
            .args(["-lc", command])
            .current_dir(temp.path())
            .output()
            .expect("run candidate gate");
        assert!(!candidate.status.success());
        let mut promotion: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "gate_failed",
            "source": {"kind": "aggregate", "task_id": "task"},
            "to_worktree": "fixture",
            "target": {"worktree": "fixture", "path": temp.path()},
            "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
            "deterministic_gates": [],
            "operator_notification": {"status": "blocked", "message": "red"}
        }))
        .expect("promotion");
        let stdout = String::from_utf8_lossy(&candidate.stdout).to_string();
        let stderr = String::from_utf8_lossy(&candidate.stderr).to_string();
        let exit_code = candidate.status.code().unwrap_or(1);
        let mut gate = crate::agent_task_gate::AgentTaskGateReport::new(
            "monorepo-gate",
            vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
            exit_code,
            &stdout,
            &stderr,
            None,
            HomeboyGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            AgentTaskGateEnvironment::default(),
        );
        gate.failure_evidence = Some(AgentTaskGateFailureEvidence {
            classification: AgentTaskGateFailureClassification::CandidateCode,
            summary: "monorepo gate failed from its requested path".to_string(),
            command: command.to_string(),
            exit_code,
            stdout_tail: stdout,
            stderr_tail: stderr,
            agent_feedback: "Repair the candidate gate failure.".to_string(),
            diagnostics: Vec::new(),
        });
        promotion.deterministic_gates.push(gate);

        compare_gate_failures_to_verified_base(
            &mut promotion,
            temp.path(),
            temp.path(),
            &base,
            std::time::Duration::from_secs(1),
            |_compared, _total| Ok(()),
        )
        .expect("baseline comparison");

        let gate = &promotion.deterministic_gates[0];
        assert_eq!(gate.status, AgentTaskGateStatus::AcceptedInheritedFailure);
        assert_eq!(
            gate.baseline_comparison
                .as_ref()
                .expect("comparison")
                .result,
            AgentTaskGateDifferentialResult::BaselineRed
        );
    }
}
