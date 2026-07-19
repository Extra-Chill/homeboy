use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::harvest::git_is_repository;
use super::*;

/// Immutable provenance for one scheduler execution. Controller-local callers
/// use an empty context; only a Lab subprocess captures its paired transport.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct HarvestExecutionContext {
    source_snapshot: Option<homeboy_core::source_snapshot::SourceSnapshot>,
    lab_offload: Option<serde_json::Value>,
}

impl HarvestExecutionContext {
    pub fn from_current_process() -> homeboy_core::Result<Self> {
        let source = std::env::var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV).ok();
        let lab = std::env::var(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV).ok();
        match (source, lab) {
            (None, None) => Ok(Self::default()),
            (Some(source), Some(lab)) if !source.trim().is_empty() && !lab.trim().is_empty() => {
                let lab_offload: serde_json::Value = serde_json::from_str(&lab).map_err(|error| {
                    incomplete_transport_error(format!("invalid Lab offload metadata: {error}"))
                })?;
                if !lab_offload.is_object() {
                    return Err(incomplete_transport_error(
                        "Lab offload metadata must be a JSON object".to_string(),
                    ));
                }
                Ok(Self {
                    source_snapshot: serde_json::from_str(&source).map_err(|error| {
                        incomplete_transport_error(format!("invalid source snapshot metadata: {error}"))
                    })?,
                    lab_offload: Some(lab_offload),
                })
            }
            (source, lab) => Err(incomplete_transport_error(format!(
                "expected paired non-empty source snapshot and Lab offload metadata (source_snapshot={}, lab_offload={})",
                source.is_some_and(|value| !value.trim().is_empty()),
                lab.is_some_and(|value| !value.trim().is_empty()),
            ))),
        }
    }

    fn snapshot_signaled(&self) -> bool {
        self.source_snapshot.is_some() || self.lab_offload.is_some()
    }

    fn source_snapshot(
        &self,
    ) -> Result<homeboy_core::source_snapshot::SourceSnapshot, HarvestError> {
        self.source_snapshot.clone().ok_or_else(|| {
            snapshot_harvest_error("is missing source snapshot transport metadata".to_string())
        })
    }

    fn lab_offload(&self) -> Result<serde_json::Value, HarvestError> {
        self.lab_offload.clone().ok_or_else(|| {
            snapshot_harvest_error("is missing Lab dispatch transport metadata".to_string())
        })
    }
}

fn incomplete_transport_error(message: String) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument(
        "lab_transport",
        format!("incomplete Lab snapshot transport: {message}"),
        None,
        Some(vec!["Run the command on the controller without Lab transport metadata, or use a Lab child process with the exact paired snapshot metadata.".to_string()]),
    )
}

#[derive(Debug)]
pub(super) struct AttemptWorkspace {
    source_root: PathBuf,
    root: PathBuf,
    base_sha: String,
}

impl AttemptWorkspace {
    pub(super) fn base_sha(&self) -> &str {
        &self.base_sha
    }

    pub(super) fn cleanup(&self) -> Result<(), String> {
        // The provider has returned and this exact detached checkout is no
        // longer leased. Remove only declared rebuildable output before the
        // source-state guard decides whether the worktree itself is removable.
        homeboy_core::cleanup::cleanup_worktree_artifacts(&self.root)
            .map_err(|error| format!("cannot cleanup rebuildable artifacts: {}", error.message))?;
        let status = git_output(&self.root, &["status", "--porcelain=v1"])
            .map_err(|error| format!("cannot inspect attempt workspace state: {error:?}"))?;
        if !status.trim().is_empty() {
            return Err("attempt workspace has uncommitted changes".to_string());
        }
        let unpushed = git_output(
            &self.root,
            &["rev-list", "--count", &format!("{}..HEAD", self.base_sha)],
        )
        .map_err(|error| format!("cannot inspect attempt workspace commits: {error:?}"))?;
        if unpushed.trim() != "0" {
            return Err(format!(
                "attempt workspace has {} commit(s) beyond its base",
                unpushed.trim()
            ));
        }
        let remove = Command::new("git")
            .args(["worktree", "remove"])
            .arg(&self.root)
            .current_dir(&self.source_root)
            .status()
            .map_err(|error| error.to_string())?;
        if !remove.success() {
            return Err(format!("git worktree remove exited {remove}"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct HarvestPreflight {
    pub(super) base_sha: Option<String>,
    pub(super) source_provenance: Option<serde_json::Value>,
    pub(super) candidate_baseline: Option<CandidateBaseline>,
}

/// A gate-feedback retry may start from the promoted, uncommitted candidate.
/// This patch is derived only after its recorded provenance reproduces the
/// exact worktree tree, so it cannot authorize arbitrary existing dirt.
#[derive(Debug)]
pub(super) struct CandidateBaseline {
    patch: String,
}

pub(super) fn prepare_committed_harvest(
    request: &AgentTaskRequest,
    derived_cook_baseline: Option<&crate::agent_task_service::DerivedCookBaselineCapability>,
    context: &HarvestExecutionContext,
) -> Result<HarvestPreflight, HarvestError> {
    let Some(root) = request.workspace.root.as_deref().map(Path::new) else {
        return Ok(HarvestPreflight {
            base_sha: None,
            source_provenance: None,
            candidate_baseline: None,
        });
    };
    let snapshot_signaled = context.snapshot_signaled();
    let is_repository = git_is_repository(root)?;
    if !snapshot_signaled && !is_repository {
        return Ok(HarvestPreflight {
            base_sha: None,
            source_provenance: None,
            candidate_baseline: None,
        });
    }
    let source_provenance = if let Some(capability) = derived_cook_baseline {
        validate_derived_cook_baseline(root, request, capability, context)?;
        Some(serde_json::json!({
            "verified_cook_baseline": capability.verified_baseline_provenance(),
            "parent_snapshot": capability.parent_snapshot(),
        }))
    } else if snapshot_signaled {
        let source_snapshot = context.source_snapshot()?;
        let lab_offload = context.lab_offload()?;
        let provenance =
            homeboy_core::lab_workspace_provenance::with_lab_workspace_provenance(|p| {
                p.verify_lab_workspace(
                    &root.display().to_string(),
                    root,
                    source_snapshot,
                    lab_offload,
                    is_repository,
                )
            })
            .map_err(snapshot_harvest_error)?;
        if is_repository {
            // git-root verification is folded into verify_lab_workspace above via
            // require_git_root = is_repository.
        } else {
            if provenance.materialization_mode == "git" {
                return Err(snapshot_harvest_error(
                    "verified Git materialization is missing root Git metadata".to_string(),
                ));
            }
            if root.join(".git").exists() {
                return Err(snapshot_harvest_error(
                    "snapshot workspace unexpectedly contains root .git metadata".to_string(),
                ));
            }
            if provenance.materialization_mode != "snapshot"
                && provenance.materialization_mode != "snapshot-git"
            {
                return Err(snapshot_harvest_error(format!(
                    "unsupported snapshot materialization mode `{}`",
                    provenance.materialization_mode
                )));
            }
        }
        let mut source_provenance = serde_json::json!({
            "source_revision": provenance.source_revision,
            "workspace_snapshot_identity": provenance.workspace_identity,
            "snapshot_hash": provenance.snapshot_hash,
            "runner_id": provenance.runner_id,
            "workspace_path": root.display().to_string(),
            "materialization_mode": provenance.materialization_mode,
        });
        // This is descriptive controller evidence only. Verification above
        // remains the authority for the remote Lab snapshot.
        if let Some(verified_baseline) = context.lab_offload.as_ref().and_then(|metadata| {
            metadata
                .get("source_provenance")
                .and_then(|provenance| provenance.get("verified_cook_baseline"))
                .cloned()
        }) {
            source_provenance["verified_cook_baseline"] = verified_baseline;
        }
        Some(source_provenance)
    } else {
        None
    };
    if !is_repository {
        let source_snapshot = context.source_snapshot()?;
        let lab_offload = context.lab_offload()?;
        homeboy_core::lab_workspace_provenance::with_lab_workspace_provenance(|p| {
            p.materialize_verified_lab_snapshot_git_baseline(
                &root.display().to_string(),
                root,
                source_snapshot,
                lab_offload,
            )
        })
        .map_err(|message| HarvestError::Git {
            command: "materialize verified Lab snapshot Git baseline".to_string(),
            message,
        })?;
    }
    let git_root = git_output(root, &["rev-parse", "--show-toplevel"])?;
    let canonical_root = root.canonicalize().map_err(|error| HarvestError::Git {
        command: "canonicalize workspace root".to_string(),
        message: error.to_string(),
    })?;
    let canonical_git_root =
        PathBuf::from(git_root)
            .canonicalize()
            .map_err(|error| HarvestError::Git {
                command: "canonicalize Git top-level".to_string(),
                message: error.to_string(),
            })?;
    if canonical_root != canonical_git_root {
        return Err(HarvestError::Git {
            command: "verify Git top-level ownership".to_string(),
            message: "Git top-level does not exactly match the managed workspace root".to_string(),
        });
    }
    let status = git_output(root, &["status", "--porcelain", "--untracked-files=all"])?;
    let candidate_baseline = if status.trim().is_empty() {
        None
    } else {
        Some(verified_gate_feedback_baseline(root, request, &status)?)
    };
    Ok(HarvestPreflight {
        base_sha: Some(git_output(root, &["rev-parse", "HEAD"])?),
        source_provenance,
        candidate_baseline,
    })
}

fn verified_gate_feedback_baseline(
    root: &Path,
    request: &AgentTaskRequest,
    status: &str,
) -> Result<CandidateBaseline, HarvestError> {
    let cook_loop = request
        .inputs
        .get("cook_loop")
        .and_then(serde_json::Value::as_object);
    let is_gate_feedback = request
        .metadata
        .get("cook_loop")
        .and_then(|value| value.get("kind"))
        .and_then(serde_json::Value::as_str)
        == Some("deterministic-gate-feedback");
    let Some(cook_loop) = cook_loop else {
        return Err(HarvestError::DirtyWorkspace {
            status: status.to_string(),
        });
    };
    let required = [
        "source_run_id",
        "source_task_id",
        "source_patch_task_id",
        "patch_artifact",
        "failed_gates",
        "next_attempt",
        "to_worktree",
    ];
    if !is_gate_feedback || required.iter().any(|key| !cook_loop.contains_key(*key)) {
        return Err(HarvestError::DirtyWorkspace {
            status: status.to_string(),
        });
    }
    let patch = crate::agent_task_candidate_baseline::validate_gate_feedback_candidate_baseline(
        root,
        &serde_json::Value::Object(cook_loop.clone()),
    )
    .map_err(|error| HarvestError::CandidateBaselineMismatch {
        message: error.message,
    })?;
    Ok(CandidateBaseline { patch })
}

fn snapshot_harvest_error(message: String) -> HarvestError {
    HarvestError::Git {
        command: "verify Lab snapshot harvest provenance".to_string(),
        message,
    }
}

fn validate_derived_cook_baseline(
    root: &Path,
    request: &AgentTaskRequest,
    capability: &crate::agent_task_service::DerivedCookBaselineCapability,
    context: &HarvestExecutionContext,
) -> Result<(), HarvestError> {
    let canonical_root = root.canonicalize().map_err(|error| {
        snapshot_harvest_error(format!("canonicalize derived baseline root: {error}"))
    })?;
    if canonical_root != capability.canonical_path()
        || request.task_id != capability.source_task_id()
    {
        return Err(snapshot_harvest_error(
            "derived baseline capability does not bind this workspace and task".to_string(),
        ));
    }
    let status = git_output(root, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err(HarvestError::DirtyWorkspace { status });
    }
    let head = git_output(root, &["rev-parse", "HEAD"])?;
    let tree = git_output(root, &["rev-parse", "HEAD^{tree}"])?;
    if head != capability.commit() || tree != capability.tree() {
        return Err(snapshot_harvest_error(
            "derived baseline HEAD/tree does not match its internal contract".to_string(),
        ));
    }
    if let Some(parent_snapshot) = capability.parent_snapshot() {
        let ambient = serde_json::to_value(context.source_snapshot()?)
            .map_err(|error| snapshot_harvest_error(error.to_string()))?;
        if parent_snapshot != &ambient {
            return Err(snapshot_harvest_error(
                "derived baseline parent snapshot does not match ambient snapshot".to_string(),
            ));
        }
    } else if context.source_snapshot.is_some() {
        return Err(snapshot_harvest_error(
            "derived baseline is missing its Lab parent snapshot".to_string(),
        ));
    }
    Ok(())
}

/// Give each provider dispatch a detached checkout at the caller's clean base.
/// The managed task worktree remains a preflight-only source of truth, so a
/// timed-out provider cannot contaminate a later provider rotation.
pub(super) fn prepare_attempt_workspace(
    request: &mut AgentTaskRequest,
    base: Option<&str>,
    candidate_baseline: Option<&CandidateBaseline>,
    scratch_root: &Path,
) -> Result<Option<Arc<AttemptWorkspace>>, HarvestError> {
    let Some(root) = request.workspace.root.as_deref().map(PathBuf::from) else {
        return Ok(None);
    };
    let Some(base) = base else {
        return Ok(None);
    };
    let identity = format!("attempt-{}", uuid::Uuid::new_v4());
    // The attempt checkout is a child of the scheduler's durably registered
    // scratch lease, never an untracked system-temporary worktree.
    let attempt_root = scratch_root.join("workspace");
    let attempt_root_string = attempt_root.display().to_string();
    git_output(
        &root,
        &["worktree", "add", "--detach", &attempt_root_string, base],
    )?;
    // Establish cleanup ownership before anything that can fail below.
    let mut workspace = Arc::new(AttemptWorkspace {
        source_root: root.clone(),
        root: attempt_root.clone(),
        base_sha: base.to_string(),
    });
    let adoption = request
        .workspace
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.adoption.clone());
    if let Some(adoption) = adoption.as_ref() {
        apply_adopted_candidate(&attempt_root, adoption)?;
    }
    if let Some(candidate_baseline) = candidate_baseline {
        apply_gate_feedback_baseline(&attempt_root, candidate_baseline)?;
    }
    let attempt_base_sha = git_output(&attempt_root, &["rev-parse", "HEAD"])?;
    Arc::get_mut(&mut workspace)
        .expect("attempt workspace has no other owners during setup")
        .base_sha = attempt_base_sha;
    let attempt_base = workspace.base_sha().to_string();
    let base_fingerprint = fingerprint(attempt_base.as_bytes());
    remap_workspace_config(&mut request.executor.config, &root, &attempt_root);
    request.workspace.root = Some(attempt_root.display().to_string());
    request.workspace.attempt = Some(crate::agent_task::AgentTaskAttemptWorkspace {
        identity,
        base_ref: attempt_base,
        base_fingerprint,
        adoption,
    });
    Ok(Some(workspace))
}

fn apply_gate_feedback_baseline(
    root: &Path,
    baseline: &CandidateBaseline,
) -> Result<(), HarvestError> {
    let patch = tempfile::NamedTempFile::new().map_err(|error| HarvestError::Git {
        command: "create gate-feedback baseline patch".to_string(),
        message: error.to_string(),
    })?;
    std::fs::write(patch.path(), &baseline.patch).map_err(|error| HarvestError::Git {
        command: "write gate-feedback baseline patch".to_string(),
        message: error.to_string(),
    })?;
    let patch_path = patch.path().display().to_string();
    git_output(root, &["apply", "--index", "--binary", &patch_path])?;
    git_output(
        root,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit",
            "--no-verify",
            "-m",
            "homeboy: gate-feedback candidate baseline",
        ],
    )?;
    Ok(())
}

pub(super) fn remap_workspace_config(
    config: &mut serde_json::Value,
    source: &Path,
    attempt: &Path,
) {
    let source = source.display().to_string();
    let attempt = attempt.display().to_string();
    fn remap(value: &mut serde_json::Value, source: &str, attempt: &str) {
        match value {
            serde_json::Value::String(path) if path == source => *path = attempt.to_string(),
            serde_json::Value::String(path) => {
                if let Some(suffix) = path.strip_prefix(&format!("{source}/")) {
                    *path = format!("{attempt}/{suffix}");
                }
            }
            serde_json::Value::Array(values) => values
                .iter_mut()
                .for_each(|value| remap(value, source, attempt)),
            serde_json::Value::Object(values) => values
                .values_mut()
                .for_each(|value| remap(value, source, attempt)),
            _ => {}
        }
    }
    remap(config, &source, &attempt);
}

fn apply_adopted_candidate(
    root: &Path,
    adoption: &crate::agent_task::AgentTaskCandidateAdoption,
) -> Result<(), HarvestError> {
    let patch = std::fs::read(&adoption.patch_path).map_err(|error| HarvestError::Adoption {
        message: format!("could not read candidate patch: {error}"),
    })?;
    let actual_fingerprint = fingerprint(&patch);
    if actual_fingerprint != adoption.patch_fingerprint {
        return Err(HarvestError::Adoption {
            message: "candidate patch fingerprint did not match the explicit adoption request"
                .to_string(),
        });
    }
    git_output(root, &["apply", "--index", &adoption.patch_path]).map_err(|error| {
        HarvestError::Adoption {
            message: format!("could not apply adopted candidate: {error:?}"),
        }
    })?;
    Ok(())
}

pub(crate) fn fingerprint(contents: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(contents))
}

fn sha256_hex(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn terminal_cleanup_reclaims_build_output_before_preserving_changed_source() {
        let source = tempfile::tempdir().expect("source repository");
        git(source.path(), &["init", "-b", "main"]);
        fs::create_dir_all(source.path().join("src")).expect("source directory");
        fs::write(source.path().join("src/lib.rs"), "base").expect("source file");
        git(source.path(), &["add", "src/lib.rs"]);
        git(
            source.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        let attempt_root = source.path().join("attempt");
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "--detach",
                attempt_root.to_str().expect("attempt path"),
                "HEAD",
            ],
        );
        fs::create_dir_all(attempt_root.join("target/debug")).expect("target directory");
        fs::write(attempt_root.join("target/debug/app"), "artifact").expect("target artifact");
        fs::write(attempt_root.join("src/lib.rs"), "changed source").expect("changed source");

        let workspace = AttemptWorkspace {
            source_root: source.path().to_path_buf(),
            root: attempt_root.clone(),
            base_sha: git_output(&attempt_root, &["rev-parse", "HEAD"]).expect("attempt head"),
        };
        let error = workspace
            .cleanup()
            .expect_err("changed source retains worktree");

        assert!(error.contains("uncommitted changes"));
        assert!(!attempt_root.join("target").exists());
        assert_eq!(
            fs::read_to_string(attempt_root.join("src/lib.rs")).expect("source remains"),
            "changed source"
        );
    }

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed with {status}", args);
    }
}
