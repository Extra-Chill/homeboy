//! Per-worktree Cook authority and lossless reset admission.
use std::path::{Path, PathBuf};

use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    cook: String,
    branch: String,
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "destination",
            String::from_utf8_lossy(&output.stderr).trim(),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn marker(repo: &Path) -> Result<PathBuf> {
    Ok(
        PathBuf::from(git(repo, &["rev-parse", "--absolute-git-dir"])?)
            .join("homeboy-cook-owner.json"),
    )
}

#[derive(Debug)]
pub struct DestinationOwnerMismatch(pub String);
impl std::fmt::Display for DestinationOwnerMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DestinationOwnerMismatch {}

pub(crate) fn owner_mismatch(message: impl Into<String>) -> Error {
    let source = DestinationOwnerMismatch(message.into());
    Error::validation_invalid_argument("destination_owner", source.to_string(), None, None)
        .with_source(source)
        .with_retryable(false)
}

/// Called only at durable destination creation, never at mutation admission.
pub(crate) fn record(repo: &Path, cook: &str) -> Result<()> {
    let owner = Owner {
        cook: cook.to_string(),
        branch: git(repo, &["symbolic-ref", "--short", "HEAD"])?,
    };
    let path = marker(repo)?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(
                &serde_json::to_vec(&owner).map_err(|e| Error::internal_io(e.to_string(), None))?,
            )
            .map_err(|e| Error::internal_io(e.to_string(), None))?;
            file.sync_all()
                .map_err(|e| Error::internal_io(e.to_string(), None))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => check(repo, Some(cook)),
        Err(e) => Err(Error::internal_io(e.to_string(), None)),
    }
}

/// Unmanaged Git checkouts remain caller-owned. A recorded destination admits
/// its owner, a retry of the owner's lineage, and a Cook succeeding a terminal
/// owner (which first preserves the owner's staged candidate).
pub(crate) fn check(repo: &Path, cook: Option<&str>) -> Result<()> {
    admit(repo, cook, true)
}

/// A run claims through its Cook: an attempt by its id prefix, a retry by its
/// `retry_of` lineage. A run never takes over another Cook's destination.
pub(crate) fn check_run(repo: &Path, run: Option<&str>) -> Result<()> {
    admit(repo, run, false)
}

/// Attempt ids are `<cook>-attempt-<n>-<suffix>` (`cook_attempt_run_id`), so
/// an attempt maps to its Cook without reading lifecycle state.
fn acts_for(owner: &str, run: &str) -> bool {
    run == owner
        || run
            .strip_prefix(owner)
            .is_some_and(|rest| rest.starts_with("-attempt-"))
}

/// What the durable lifecycle says about `claimant` against `owner`. An
/// unreadable record proves nothing, so it admits nothing.
fn lifecycle_admits(owner: &str, claimant: &str, succeed: bool) -> Option<bool> {
    let store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().ok()?;
    if let Ok(record) = store.read_record(claimant) {
        let root = crate::agent_task_lifecycle::retry_root_run_id_in_store(&store, &record).ok()?;
        if acts_for(owner, &root) {
            return Some(false);
        }
    }
    (succeed && store.read_record(owner).ok()?.state().is_terminal()).then_some(true)
}

fn admit(repo: &Path, claimant: Option<&str>, succeed: bool) -> Result<()> {
    let path = marker(repo)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(owner_mismatch(e.to_string())),
    };
    let owner: Owner = serde_json::from_slice(&bytes).map_err(|e| owner_mismatch(e.to_string()))?;
    let branch = git(repo, &["symbolic-ref", "--short", "HEAD"])?;
    let admitted = match claimant {
        _ if branch != owner.branch => None,
        Some(run) if acts_for(&owner.cook, run) => Some(false),
        Some(run) => lifecycle_admits(&owner.cook, run, succeed),
        None => None,
    };
    match (admitted, claimant) {
        (Some(true), Some(cook)) => {
            preserve_candidate(repo, &owner.cook)?;
            write_owner(
                &path,
                &Owner {
                    cook: cook.to_string(),
                    branch,
                },
            )
        }
        (Some(_), _) => Ok(()),
        (None, _) => Err(owner_mismatch(format!(
            "destination belongs to Cook {} on {}, not {:?} on {}",
            owner.cook, owner.branch, claimant, branch
        ))),
    }
}

fn write_owner(path: &Path, owner: &Owner) -> Result<()> {
    let staged = path.with_extension("json.next");
    std::fs::write(
        &staged,
        serde_json::to_vec(owner).map_err(|e| Error::internal_io(e.to_string(), None))?,
    )
    .and_then(|()| std::fs::rename(&staged, path))
    .map_err(|e| Error::internal_io(e.to_string(), None))
}

/// Must succeed before any destructive recovery operation. The index is the
/// candidate, not HEAD; commit-tree leaves both index and working tree intact.
pub(crate) fn before_reset(repo: &Path, cook: &str) -> Result<()> {
    check(repo, Some(cook))?;
    preserve_candidate(repo, cook)
}

/// Keep a staged candidate reachable at `refs/homeboy/candidates/<cook>`.
fn preserve_candidate(repo: &Path, cook: &str) -> Result<()> {
    let tree = git(repo, &["write-tree"])?;
    let head_tree = git(repo, &["rev-parse", "HEAD^{tree}"])?;
    if tree != head_tree {
        let reference = format!("refs/homeboy/candidates/{cook}");
        git(repo, &["check-ref-format", &reference])?;
        let head = git(repo, &["rev-parse", "HEAD"])?;
        let commit = git(
            repo,
            &[
                "-c",
                "user.name=Homeboy",
                "-c",
                "user.email=homeboy@localhost",
                "commit-tree",
                &tree,
                "-p",
                &head,
                "-m",
                "Preserve staged Cook candidate before recovery",
            ],
        )?;
        git(repo, &["update-ref", &reference, &commit])?;
    }
    Ok(())
}
