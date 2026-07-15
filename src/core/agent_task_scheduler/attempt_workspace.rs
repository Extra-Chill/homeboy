use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use sha2::{Digest, Sha256};

use super::harvest::git_is_repository;
use super::*;

#[derive(Debug)]
pub(super) struct AttemptWorkspace {
    source_root: PathBuf,
    root: PathBuf,
    retain: AtomicBool,
}

impl AttemptWorkspace {
    pub(super) fn retain_for_diagnostics(&self) {
        self.retain.store(true, Ordering::Release);
    }

    pub(super) fn cleanup(&self) -> Result<(), String> {
        let remove = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.root)
            .current_dir(&self.source_root)
            .status()
            .map_err(|error| error.to_string())?;
        if !remove.success() {
            return Err(format!("git worktree remove exited {remove}"));
        }
        self.retain.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for AttemptWorkspace {
    fn drop(&mut self) {
        if self.retain.load(Ordering::Acquire) {
            return;
        }
        // This is a scheduler-created detached checkout, never the caller's
        // managed task workspace. Drop runs after the executor thread exits.
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.root)
            .current_dir(&self.source_root)
            .status();
    }
}

pub(super) struct HarvestPreflight {
    pub(super) base_sha: Option<String>,
    pub(super) source_provenance: Option<serde_json::Value>,
}

pub(super) fn prepare_committed_harvest(
    request: &AgentTaskRequest,
    derived_cook_baseline: Option<&crate::core::agent_task_service::DerivedCookBaselineCapability>,
) -> Result<HarvestPreflight, HarvestError> {
    let Some(root) = request.workspace.root.as_deref().map(Path::new) else {
        return Ok(HarvestPreflight {
            base_sha: None,
            source_provenance: None,
        });
    };
    let snapshot_signaled = std::env::var_os(crate::core::observation::LAB_OFFLOAD_METADATA_ENV)
        .is_some()
        || std::env::var_os(crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV).is_some();
    let is_repository = git_is_repository(root)?;
    if !snapshot_signaled && !is_repository {
        return Ok(HarvestPreflight {
            base_sha: None,
            source_provenance: None,
        });
    }
    let source_provenance = if let Some(capability) = derived_cook_baseline {
        validate_derived_cook_baseline(root, request, capability)?;
        Some(serde_json::json!({
            "verified_cook_baseline": capability.verified_baseline_provenance(),
            "parent_snapshot": capability.parent_snapshot(),
        }))
    } else if snapshot_signaled {
        let provenance =
            crate::core::runner::verify_lab_workspace_from_env(&root.display().to_string(), root)
                .map_err(snapshot_harvest_error)?;
        if is_repository {
            crate::core::runner::verify_lab_workspace_git_root(root, &provenance)
                .map_err(snapshot_harvest_error)?;
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
        if let Some(verified_baseline) =
            std::env::var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|metadata| {
                    metadata
                        .get("source_provenance")
                        .and_then(|provenance| provenance.get("verified_cook_baseline"))
                        .cloned()
                })
        {
            source_provenance["verified_cook_baseline"] = verified_baseline;
        }
        Some(source_provenance)
    } else {
        None
    };
    if !is_repository {
        crate::core::runner::materialize_verified_lab_snapshot_git_baseline_from_env(
            &root.display().to_string(),
            root,
        )
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
    if !status.trim().is_empty() {
        return Err(HarvestError::DirtyWorkspace { status });
    }
    Ok(HarvestPreflight {
        base_sha: Some(git_output(root, &["rev-parse", "HEAD"])?),
        source_provenance,
    })
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
    capability: &crate::core::agent_task_service::DerivedCookBaselineCapability,
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
        let ambient: serde_json::Value =
            std::env::var(crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .ok_or_else(|| {
                    snapshot_harvest_error("missing ambient parent snapshot".to_string())
                })?;
        if parent_snapshot != &ambient {
            return Err(snapshot_harvest_error(
                "derived baseline parent snapshot does not match ambient snapshot".to_string(),
            ));
        }
    } else if std::env::var_os(crate::core::observation::SOURCE_SNAPSHOT_METADATA_ENV).is_some() {
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
) -> Result<Option<Arc<AttemptWorkspace>>, HarvestError> {
    let Some(root) = request.workspace.root.as_deref().map(PathBuf::from) else {
        return Ok(None);
    };
    let Some(base) = base else {
        return Ok(None);
    };
    let parent = std::env::temp_dir().join("homeboy-agent-task-attempts");
    std::fs::create_dir_all(&parent).map_err(|error| HarvestError::ArtifactDirectory {
        path: parent.clone(),
        message: error.to_string(),
    })?;
    let identity = format!("attempt-{}", uuid::Uuid::new_v4());
    let attempt_root = parent.join(&identity);
    let attempt_root_string = attempt_root.display().to_string();
    git_output(
        &root,
        &["worktree", "add", "--detach", &attempt_root_string, base],
    )?;
    // Establish cleanup ownership before anything that can fail below.
    let workspace = Arc::new(AttemptWorkspace {
        source_root: root.clone(),
        root: attempt_root.clone(),
        retain: AtomicBool::new(false),
    });
    let base_fingerprint = fingerprint(base.as_bytes());
    let adoption = request
        .workspace
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.adoption.clone());
    if let Some(adoption) = adoption.as_ref() {
        apply_adopted_candidate(&attempt_root, adoption)?;
    }
    remap_workspace_config(&mut request.executor.config, &root, &attempt_root);
    request.workspace.root = Some(attempt_root.display().to_string());
    request.workspace.attempt = Some(crate::core::agent_task::AgentTaskAttemptWorkspace {
        identity,
        base_ref: base.to_string(),
        base_fingerprint,
        adoption,
    });
    Ok(Some(workspace))
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
    adoption: &crate::core::agent_task::AgentTaskCandidateAdoption,
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
