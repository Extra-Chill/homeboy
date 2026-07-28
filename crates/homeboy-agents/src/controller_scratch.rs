use homeboy_engine_primitives::content_hash;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent_task::AgentTaskOutcome;
use homeboy_core::observation::ObservationStore;
use homeboy_core::output::{OutputBudget, OutputPresentation, OutputTruncation};
use homeboy_core::{git, paths, Error, Result};

pub const CONTROLLER_SCRATCH_SCHEMA: &str = "homeboy/controller-scratch/v1";
const INTERRUPTED_RETENTION: &str = "P1D";
const RETENTION_OWNER_SUMMARY_LIMIT: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControllerScratchResource {
    pub path: String,
    pub run_id: String,
    #[serde(default)]
    pub plan_id: String,
    pub task_id: String,
    #[serde(default)]
    pub attempt: u32,
    pub root_bound: String,
    pub owner_pid: u32,
    #[serde(default)]
    pub lifecycle_state: String,
    #[serde(default)]
    pub lease_id: String,
    pub reconstructable: bool,
    /// Controller-created temporary state that can be removed even when it is a
    /// detached Git checkout with no upstream.
    #[serde(default)]
    pub ephemeral: bool,
    pub retention: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_evidence: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
    /// Git registration identity of a linked worktree materialized inside this
    /// lease, recorded when the worktree is created.
    ///
    /// The live path does not need it — a linked worktree's own `.git` pointer
    /// file names its registration, and reading that is stronger evidence than
    /// anything we could remember. This field exists for the case the pointer
    /// file is already gone: a registration whose directory was deleted behind
    /// Git is exactly the leak #10568 describes, and pruning it requires
    /// knowing which repository holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_worktree: Option<ControllerScratchGitWorktree>,
}

/// Where a linked Git worktree is registered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControllerScratchGitWorktree {
    /// Absolute path of the linked worktree itself.
    pub path: String,
    /// Absolute path of the repository the worktree is registered against.
    pub source_root: String,
    /// Absolute path of the registration directory Git keeps for this worktree
    /// (`<repo>/.git/worktrees/<id>`). Recorded so a stranded registration can
    /// be removed by identity instead of by a repository-wide
    /// `git worktree prune`, which would also reap registrations Homeboy does
    /// not own.
    pub registration: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerScratchAllocation {
    pub path: PathBuf,
    pub lease_id: String,
    pub(crate) index_path: PathBuf,
}

/// Allocate and durably register one scheduler-owned temporary root for a
/// provider dispatch attempt. Allocation remains separate from terminal lease
/// handling and retention policy.
pub fn allocate_attempt(
    run_id: &str,
    plan_id: &str,
    task_id: &str,
    attempt: u32,
) -> Result<ControllerScratchAllocation> {
    allocate_attempt_at_index(run_id, plan_id, task_id, attempt, index_path()?)
}

#[cfg(test)]
pub fn allocate_test_attempt(
    run_id: &str,
    plan_id: &str,
    task_id: &str,
    attempt: u32,
) -> Result<ControllerScratchAllocation> {
    let index_path = paths::homeboy_data()?.join(format!(
        "controller-scratch/test-indexes/{}/resources.json",
        paths::sanitize_path_segment(run_id)
    ));
    allocate_attempt_at_index(run_id, plan_id, task_id, attempt, index_path)
}

fn allocate_attempt_at_index(
    run_id: &str,
    plan_id: &str,
    task_id: &str,
    attempt: u32,
    index_path: PathBuf,
) -> Result<ControllerScratchAllocation> {
    let root = paths::homeboy_data()?.join("controller-scratch/attempts");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", root.display())),
        )
    })?;
    let root = root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("canonicalize {}", root.display())),
        )
    })?;
    let lease_id = uuid::Uuid::new_v4().to_string();
    let path = root
        .join(paths::sanitize_path_segment(run_id))
        .join(paths::sanitize_path_segment(plan_id))
        .join(paths::sanitize_path_segment(task_id))
        .join(format!("attempt-{attempt}"))
        .join(&lease_id);
    fs::create_dir_all(&path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", path.display())),
        )
    })?;
    let path = path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("canonicalize {}", path.display())),
        )
    })?;
    if path == root || !path.starts_with(&root) {
        return Err(Error::validation_invalid_argument(
            "controller_scratch.path",
            "allocated scratch path must be contained by its root",
            Some(path.display().to_string()),
            None,
        ));
    }

    with_index_lock(&index_path, || {
        let mut index = read_or_recover_index_at_unlocked(&index_path)?;
        index.resources.push(ControllerScratchResource {
            path: path.display().to_string(),
            run_id: run_id.to_string(),
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            attempt,
            root_bound: root.display().to_string(),
            owner_pid: std::process::id(),
            lifecycle_state: "active".to_string(),
            lease_id: lease_id.clone(),
            reconstructable: true,
            ephemeral: false,
            retention: "P7D".to_string(),
            source_ref: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            finalized_at: None,
            terminal_reason: None,
            terminal_evidence: None,
            interrupted_at: None,
            git_worktree: None,
        });
        write_index_at_unlocked(&index_path, &index)
    })?;

    Ok(ControllerScratchAllocation {
        path,
        lease_id,
        index_path,
    })
}

/// Releases one scheduler-owned attempt after its candidate evidence has been
/// harvested. The first terminal record is authoritative, making retries and
/// duplicate provider completions safe to replay.
pub fn release_attempt(
    allocation: &ControllerScratchAllocation,
    reason: &str,
    evidence: serde_json::Value,
) -> Result<()> {
    with_index_lock(&allocation.index_path, || {
        let mut index = read_index_at_unlocked(&allocation.index_path)?;
        let Some(resource) = index.resources.iter_mut().find(|resource| {
            resource.lease_id == allocation.lease_id
                && Path::new(&resource.path) == allocation.path.as_path()
        }) else {
            return Err(Error::validation_invalid_argument(
                "controller_scratch.lease_id",
                "allocated scratch lease is not registered",
                Some(allocation.lease_id.clone()),
                None,
            ));
        };
        if resource.finalized_at.is_none() {
            resource.lifecycle_state = "released".to_string();
            resource.finalized_at = Some(chrono::Utc::now().to_rfc3339());
            resource.terminal_reason = Some(reason.to_string());
            resource.terminal_evidence = Some(evidence);
            write_index_at_unlocked(&allocation.index_path, &index)?;
        }
        Ok(())
    })
}

/// Mark a controller-owned allocation as disposable temporary state. This is
/// intentionally separate from ordinary attempt scratch: an interrupted
/// detached checkout has no upstream by design, so the normal unpushed-Git
/// retention guard would otherwise retain it forever.
pub fn mark_attempt_ephemeral(allocation: &ControllerScratchAllocation) -> Result<()> {
    with_index_lock(&allocation.index_path, || {
        let mut index = read_index_at_unlocked(&allocation.index_path)?;
        let resource = index
            .resources
            .iter_mut()
            .find(|resource| {
                resource.lease_id == allocation.lease_id
                    && Path::new(&resource.path) == allocation.path.as_path()
            })
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "controller_scratch.lease_id",
                    "allocated scratch lease is not registered",
                    Some(allocation.lease_id.clone()),
                    None,
                )
            })?;
        resource.ephemeral = true;
        write_index_at_unlocked(&allocation.index_path, &index)
    })
}

/// Record that a linked Git worktree has been materialized inside an allocated
/// scratch lease, binding the worktree's Git registration to the durable run
/// that owns the lease (#10568).
///
/// The registration is read back out of Git rather than taken from the caller:
/// the worktree's own `.git` pointer file is the only authority on which
/// repository holds its registration, and a caller-supplied source root that
/// disagrees with it would let cleanup prune the wrong repository. A path that
/// is not a linked worktree records nothing and is not an error — providers may
/// materialize plain directories.
pub fn record_attempt_git_worktree(
    allocation: &ControllerScratchAllocation,
    worktree: &Path,
) -> Result<()> {
    let Some(registration) = linked_worktree_registration(worktree) else {
        return Ok(());
    };
    with_index_lock(&allocation.index_path, || {
        let mut index = read_index_at_unlocked(&allocation.index_path)?;
        let resource = index
            .resources
            .iter_mut()
            .find(|resource| {
                resource.lease_id == allocation.lease_id
                    && Path::new(&resource.path) == allocation.path.as_path()
            })
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "controller_scratch.lease_id",
                    "allocated scratch lease is not registered",
                    Some(allocation.lease_id.clone()),
                    None,
                )
            })?;
        resource.git_worktree = Some(registration);
        write_index_at_unlocked(&allocation.index_path, &index)
    })
}

/// Resolve the Git registration of a *linked* worktree from its own on-disk
/// pointer file.
///
/// A linked worktree's `.git` is a regular file containing
/// `gitdir: <repo>/.git/worktrees/<id>`, and that registration directory holds
/// a `gitdir` file pointing back at `<worktree>/.git`. Both files are written
/// by Git, so following them is ownership by recorded metadata, not by a
/// database join — and requiring the two to agree makes a half-written or
/// recycled registration unusable rather than merely suspicious.
///
/// The worktree path is taken from Git's own back-pointer rather than from the
/// argument, so the recorded string is byte-identical to what a later prune
/// will read back. Canonicalizing our argument instead would disagree with Git
/// on any platform where the temporary or home directory is reached through a
/// symlink.
///
/// A normal checkout (`.git` is a directory) has no registration to prune and
/// answers `None`, as does any pointer that cannot be parsed, resolved, or
/// cross-checked. Fail closed: the consequence of guessing here is
/// `git worktree remove` against the wrong repository.
fn linked_worktree_registration(worktree: &Path) -> Option<ControllerScratchGitWorktree> {
    let pointer = worktree.join(".git");
    if !fs::symlink_metadata(&pointer).ok()?.is_file() {
        return None;
    }
    let contents = fs::read_to_string(&pointer).ok()?;
    let registration = contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix("gitdir:")
            .map(|value| PathBuf::from(value.trim()))
    })?;
    // `<repo>/.git/worktrees/<id>` -> `<repo>`. Anything shallower is not a
    // linked-worktree registration and is refused rather than reinterpreted.
    let registrations = registration.parent()?;
    if registrations.file_name()? != "worktrees" {
        return None;
    }
    let source_root = registrations.parent()?.parent()?;
    if !source_root.is_dir() {
        return None;
    }
    // Git's back-pointer: `<registration>/gitdir` names `<worktree>/.git`.
    let back_pointer = fs::read_to_string(registration.join("gitdir")).ok()?;
    let recorded_worktree = PathBuf::from(back_pointer.trim()).parent()?.to_path_buf();
    if recorded_worktree.canonicalize().ok()? != worktree.canonicalize().ok()? {
        return None;
    }
    Some(ControllerScratchGitWorktree {
        path: recorded_worktree.display().to_string(),
        source_root: source_root.canonicalize().ok()?.display().to_string(),
        registration: registration.display().to_string(),
    })
}

/// Remove one stranded Git worktree registration whose directory is already
/// gone, by identity.
///
/// Repository-wide `git worktree prune` would also reap registrations Homeboy
/// never created, so this removes exactly the recorded registration and only
/// when the registration itself still agrees that it belongs to this path:
///
/// * the worktree path must really be absent — an existing worktree is live;
/// * `<registration>/gitdir` must still name exactly `<worktree>/.git`, so a
///   reused or recycled registration id can never be mistaken for ours. The
///   recorded path came out of this same file, so this is an exact match, not
///   a normalization guess;
/// * a `locked` marker retains, exactly as Git's own prune does.
///
/// Every failure retains. Nothing here consults the run database.
fn prune_stranded_worktree_registration(registration: &ControllerScratchGitWorktree) -> bool {
    let worktree = Path::new(&registration.path);
    if worktree.exists() {
        return false;
    }
    let directory = Path::new(&registration.registration);
    if directory.join("locked").exists() {
        return false;
    }
    let Ok(recorded) = fs::read_to_string(directory.join("gitdir")) else {
        return false;
    };
    if Path::new(recorded.trim()) != worktree.join(".git") {
        return false;
    }
    fs::remove_dir_all(directory).is_ok()
}

/// Unregister every linked Git worktree inside a scratch lease using Git's own
/// lifecycle operation, before the lease's bytes are removed.
///
/// This is the #10568 defect in one line: cleanup used to `remove_dir_all` the
/// lease, which deletes a *registered* worktree behind Git's back and leaves
/// `<repo>/.git/worktrees/<id>` alive in a shared repository, inflating every
/// worktree inventory for months (Git only expires registrations during `gc`,
/// after `gc.worktreePruneExpire`).
///
/// `Some(reason)` means nothing was deleted and the lease is retained for
/// reporting. There is deliberately no fall-back to deleting the directory: a
/// retained lease is recoverable, a stranded registration in someone else's
/// repository is not.
///
/// `--force` is required rather than optional here: the recovery path
/// intentionally admits a workspace whose contents were bundled and verified
/// while still dirty. Every guard `--force` would skip has already been proved
/// by `cleanup_block_reason` with stronger evidence.
fn unregister_scratch_git_worktrees(path: &Path) -> Option<String> {
    for worktree in [path.to_path_buf(), path.join("workspace")] {
        let Some(registration) = linked_worktree_registration(&worktree) else {
            continue;
        };
        if git::run_git(
            Path::new(&registration.source_root),
            &["worktree", "remove", "--force", registration.path.as_str()],
            "git worktree remove",
        )
        .is_err()
        {
            return Some(format!(
                "linked Git worktree `{}` could not be unregistered from `{}`; retained so its registration is not stranded",
                registration.path, registration.source_root
            ));
        }
    }
    None
}

/// Proof that unregistering a linked worktree cannot destroy committed work.
///
/// `git worktree remove` deletes the worktree and its registration; it never
/// deletes objects. What it does remove is the worktree's detached HEAD as a
/// reachability root, so a commit only that HEAD referenced becomes unreachable
/// and eventually collectable. The work is therefore preserved exactly when
/// every commit reachable from the worktree's HEAD is already reachable from a
/// branch, a tag, or a remote-tracking ref in the owning repository — refs that
/// outlive the worktree.
///
/// This is strictly stronger than the `@{upstream}` test it replaces for this
/// shape. `@{upstream}..HEAD == 0` means the commits are reachable from a
/// remote-tracking ref, which `--remotes` already covers; it is never weaker.
/// Another attempt worktree's detached HEAD deliberately does not count as an
/// anchor — it is exactly as ephemeral as this one.
///
/// Every failure answers "not preserved".
fn attempt_worktree_commits_are_preserved(worktree: &Path, source_root: &Path) -> bool {
    let Ok(head) = git::run_git(worktree, &["rev-parse", "HEAD"], "git rev-parse HEAD") else {
        return false;
    };
    let head = head.trim().to_string();
    if head.is_empty() {
        return false;
    }
    git::run_git(
        source_root,
        &[
            "rev-list",
            "--count",
            head.as_str(),
            "--not",
            "--branches",
            "--tags",
            "--remotes",
        ],
        "git rev-list unanchored attempt commits",
    )
    .map(|count| count.trim() == "0")
    .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn abandon_attempt_for_test(allocation: &ControllerScratchAllocation) -> Result<()> {
    with_index_lock(&allocation.index_path, || {
        let mut index = read_index_at_unlocked(&allocation.index_path)?;
        let resource = index
            .resources
            .iter_mut()
            .find(|resource| resource.lease_id == allocation.lease_id)
            .expect("test allocation is registered");
        resource.owner_pid = u32::MAX;
        write_index_at_unlocked(&allocation.index_path, &index)
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ControllerScratchIndex {
    #[serde(default = "schema")]
    schema: String,
    #[serde(default)]
    resources: Vec<ControllerScratchResource>,
}

#[derive(Debug, Serialize)]
pub struct ControllerScratchCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    /// Registered attempt Git worktrees still present on disk. Aggregate
    /// visibility for #10568: these inflate every repository's worktree
    /// inventory until their lease converges.
    pub registered_worktree_count: usize,
    /// Leases whose directory is already gone. Each may hold a Git registration
    /// stranded in a shared repository, and an index row nothing else reaps.
    pub stale_registration_count: usize,
    /// Stranded registrations removed by identity during this apply.
    pub stale_registrations_pruned: usize,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    /// Bytes that would be eligible under the DEFAULT per-resource retention
    /// policy (i.e. with no `--older-than-days` override). Equal to
    /// `estimated_bytes` when no override is active; smaller when an override
    /// unlocks additional released, clean scratch that is still inside its
    /// default retention window. Lets operators see exactly how much a
    /// pressure-reclaim override adds versus normal cleanup.
    pub default_policy_eligible_bytes: u64,
    /// Bytes eligible ONLY because of an explicit retention override
    /// (`estimated_bytes - default_policy_eligible_bytes`). Zero when no
    /// override is active.
    pub override_unlocked_bytes: u64,
    pub reclaimed_bytes: u64,
    pub candidates: Vec<ControllerScratchCandidate>,
    pub candidate_detail: OutputTruncation,
    pub skipped: Vec<ControllerScratchSkipped>,
    pub skipped_detail: OutputTruncation,
    pub retention_reasons: Vec<ControllerScratchRetentionReason>,
    pub remaining_candidate_count: usize,
    pub remaining_candidate_bytes: u64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
    pub drain_command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControllerScratchCandidate {
    pub path: String,
    pub run_id: String,
    pub task_id: String,
    pub size_bytes: u64,
    pub owner_pid: u32,
    pub lease_id: String,
    pub reason: String,
    pub lifecycle_state: String,
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControllerScratchSkipped {
    pub path: String,
    pub run_id: Option<String>,
    pub owner_pid: Option<u32>,
    pub lifecycle_state: Option<String>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_command: Option<String>,
}

/// Bounded retention inventory for operators investigating cleanup convergence.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ControllerScratchRetentionReason {
    pub reason: String,
    pub resource_count: usize,
    pub owners: Vec<ControllerScratchRetentionOwner>,
    pub additional_owner_count: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ControllerScratchRetentionOwner {
    pub run_id: String,
    pub resource_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerScratchCleanupOptions {
    pub apply: bool,
    pub limit: usize,
    /// Return all detail rows rather than the normal agent-facing response budget.
    pub full: bool,
    /// Optional override for the terminal-run retention window, in seconds.
    ///
    /// `None` (default) uses each resource's own configured retention window
    /// (e.g. `P7D`), preserving historical behavior. `Some(0)` lets released,
    /// clean, terminal scratch converge immediately once finalized — this is the
    /// disk-pressure path exposed by `--older-than-days`. The override ONLY
    /// affects the retention time-window comparison; it never bypasses the
    /// still-active-run, running-owner, not-finalized, orphaned-transition, or
    /// dirty/unpushed guards, which continue to protect in-use resources.
    pub retention_override_seconds: Option<i64>,
}

/// Read-only lifecycle inventory used by retained-storage reporting. Unlike
/// cleanup, this neither reconciles leases nor writes the scratch index.
#[derive(Debug, Serialize)]
pub struct ControllerScratchRetainedResource {
    pub path: String,
    pub run_id: String,
    pub task_id: String,
    pub owner_pid: u32,
    pub lifecycle_state: String,
    pub reason: String,
    pub liveness: String,
    pub age_seconds: Option<u64>,
    pub size_bytes: u64,
}

pub fn retained_storage_inventory() -> Result<Vec<ControllerScratchRetainedResource>> {
    let index_path = index_path()?;
    with_index_lock(&index_path, || {
        let index = read_index_unlocked()?;
        let now = chrono::Utc::now();
        let mut resources = Vec::new();
        for resource in index.resources {
            let path = PathBuf::from(&resource.path);
            if !path.exists() {
                continue;
            }
            let (reason, liveness) = if !resource.reconstructable {
                (
                    "resource is not explicitly reconstructable",
                    "unknown/unmanaged",
                )
            } else if homeboy_core::process::pid_is_running(resource.owner_pid) {
                ("owner process is still running", "active")
            } else if resource.finalized_at.is_none() {
                ("resource has not been finalized by its owning run", "stale")
            } else {
                (
                    "retention policy has not been advanced by cleanup",
                    "terminal",
                )
            };
            let age_seconds = chrono::DateTime::parse_from_rfc3339(&resource.created_at)
                .ok()
                .map(|created| {
                    now.signed_duration_since(created.with_timezone(&chrono::Utc))
                        .num_seconds()
                        .max(0) as u64
                });
            resources.push(ControllerScratchRetainedResource {
                path: resource.path,
                run_id: resource.run_id,
                task_id: resource.task_id,
                owner_pid: resource.owner_pid,
                lifecycle_state: resource.lifecycle_state,
                reason: reason.to_string(),
                liveness: liveness.to_string(),
                age_seconds,
                size_bytes: path_size(&path)?,
            });
        }
        Ok(resources)
    })
}

/// Registers provider-created controller scratch returned in an outcome's
/// `metadata.controller_scratch` object or array. Providers own materializing
/// the path; Homeboy owns its durable lifecycle and cleanup policy.
pub fn register_outcome_resources(run_id: &str, outcomes: &[AgentTaskOutcome]) -> Result<()> {
    let index_path = index_path()?;
    with_index_lock(&index_path, || {
        register_outcome_resources_unlocked(run_id, outcomes)
    })
}

fn register_outcome_resources_unlocked(run_id: &str, outcomes: &[AgentTaskOutcome]) -> Result<()> {
    let mut index = read_index_unlocked()?;
    let mut changed = false;
    for outcome in outcomes {
        let values = outcome
            .metadata
            .get("controller_scratch")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| vec![value.clone()])
            })
            .unwrap_or_default();
        for value in values {
            let Some(path) = value.get("path").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let path = PathBuf::from(path).canonicalize().map_err(|error| {
                Error::validation_invalid_argument(
                    "controller_scratch.path",
                    error.to_string(),
                    Some(path.to_string()),
                    None,
                )
            })?;
            let root = value
                .get("root_bound")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| path.parent().unwrap_or(&path).to_path_buf())
                .canonicalize()
                .map_err(|error| {
                    Error::validation_invalid_argument(
                        "controller_scratch.root_bound",
                        error.to_string(),
                        None,
                        None,
                    )
                })?;
            if path == root || !path.starts_with(&root) {
                return Err(Error::validation_invalid_argument(
                    "controller_scratch.path",
                    "scratch path must be contained by its declared root",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            let resource = ControllerScratchResource {
                path: path.display().to_string(),
                run_id: run_id.to_string(),
                plan_id: String::new(),
                task_id: outcome.task_id.clone(),
                attempt: 0,
                root_bound: root.display().to_string(),
                owner_pid: std::process::id(),
                lifecycle_state: "provider_registered".to_string(),
                // Provider resources arrive after their materialization, so
                // assign their lease while adopting them into our index.
                lease_id: uuid::Uuid::new_v4().to_string(),
                reconstructable: value
                    .get("reconstructable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                ephemeral: value
                    .get("ephemeral")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                retention: value
                    .get("retention")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("P7D")
                    .to_string(),
                source_ref: value
                    .get("source_ref")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                created_at: chrono::Utc::now().to_rfc3339(),
                finalized_at: None,
                terminal_reason: None,
                terminal_evidence: None,
                interrupted_at: None,
                git_worktree: None,
            };
            index
                .resources
                .retain(|existing| existing.path != resource.path);
            index.resources.push(resource);
            changed = true;
        }
    }
    if changed {
        write_index_unlocked(&index)?;
    }
    Ok(())
}

/// Marks all resources owned by a terminal run as finalized, including failed
/// and cancelled exits, so retention starts regardless of task outcome.
pub fn finalize_run(run_id: &str) -> Result<()> {
    let index_path = index_path()?;
    with_index_lock(&index_path, || finalize_run_unlocked(run_id))
}

fn finalize_run_unlocked(run_id: &str) -> Result<()> {
    let mut index = read_index_unlocked()?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut changed = false;
    for resource in &mut index.resources {
        if resource.run_id == run_id {
            if resource.finalized_at.is_none() {
                resource.finalized_at = Some(now.clone());
                changed = true;
            }
            let has_recovery_evidence = recovery_evidence_is_current(resource);
            let stale_dirty_workspace = !resource.ephemeral
                && git_safety_path(resource, Path::new(&resource.path))
                    .is_some_and(|workspace| git_dirty_or_unpushed(&workspace));
            let needs_recovery = !has_recovery_evidence
                && (matches!(
                    resource.lifecycle_state.as_str(),
                    "active" | "provider_registered"
                ) || resource.lifecycle_state == "interrupted"
                    || stale_dirty_workspace);
            if needs_recovery {
                let workspace_recovery = recover_authoritative_workspace(resource);
                resource.lifecycle_state = "interrupted".to_string();
                resource.interrupted_at = Some(now.clone());
                resource.terminal_reason = Some("owning_run_terminalized".to_string());
                resource.terminal_evidence = Some(serde_json::json!({
                    "run_id": run_id,
                    "retention": INTERRUPTED_RETENTION,
                    "workspace_recovery": workspace_recovery,
                }));
                changed = true;
            }
        }
    }
    if changed {
        write_index_unlocked(&index)?;
    }
    Ok(())
}

pub fn cleanup(options: ControllerScratchCleanupOptions) -> Result<ControllerScratchCleanupOutput> {
    let index_path = index_path()?;
    with_index_lock(&index_path, || cleanup_unlocked(options))
}

fn cleanup_unlocked(
    options: ControllerScratchCleanupOptions,
) -> Result<ControllerScratchCleanupOutput> {
    let mut index = read_index_unlocked()?;
    // Only durably registered resources are cleanup candidates. In particular,
    // do not infer ownership by scanning a shared system temporary directory.
    let mut skipped = Vec::new();
    let mut skipped_bytes = 0;
    // Complete, lightweight (reason, run_id) record of every skipped resource,
    // used to compute an accurate `skipped_count` and `retention_reasons`
    // summary even when the materialized `skipped` rows are capped.
    let mut all_skips: Vec<(String, Option<String>)> = Vec::new();
    let mut applied_count = 0;
    let mut reclaimed_bytes = 0;
    // Bytes among the eligible set that would also clear the DEFAULT retention
    // window (no override). Used to report override-unlocked bytes separately.
    let mut default_policy_eligible_bytes: u64 = 0;
    let now = chrono::Utc::now();
    let mut eligible = Vec::new();
    let mut reconciled = false;
    // Leases whose bytes are already gone but whose Git registration is not.
    // Nothing else in this function can see them: the loop below skips a
    // missing path entirely, so a registration deleted behind Git stayed
    // stranded in its source repository indefinitely (#10568).
    let mut stranded: Vec<usize> = Vec::new();
    let mut registered_worktree_count = 0;
    for (position, resource) in index.resources.iter_mut().enumerate() {
        let path = PathBuf::from(&resource.path);
        if !path.exists() {
            // Only a finalized lease is a converged one. An unfinalized row
            // whose directory is missing may still belong to a dispatch that
            // has not materialized its scratch yet.
            if resource.finalized_at.is_some() && resource.git_worktree.is_some() {
                stranded.push(position);
            }
            continue;
        }
        // A lease can outlive its worktree — an operator or a partial cleanup
        // can remove `<lease>/workspace` on its own — so registration liveness
        // is asked of the worktree path, not of the lease.
        if let Some(registration) = resource.git_worktree.as_ref() {
            if Path::new(&registration.path).exists() {
                registered_worktree_count += 1;
            } else if resource.finalized_at.is_some() {
                stranded.push(position);
            }
        }
        let lifecycle_state = resource.lifecycle_state.clone();
        let interrupted_at = resource.interrupted_at.clone();
        let reason =
            cleanup_block_reason(resource, &path, now, options.retention_override_seconds)?;
        reconciled |= resource.lifecycle_state != lifecycle_state
            || resource.interrupted_at != interrupted_at;
        if let Some(reason) = reason {
            // Keep every row for an explicit `--full` inspection. The default
            // response admits rows directly to its item/byte budget so a large
            // retained index is not materialized before it is truncated.
            all_skips.push((reason.clone(), Some(resource.run_id.clone())));
            append_skipped_detail(
                &mut skipped,
                &mut skipped_bytes,
                options.full,
                ControllerScratchSkipped {
                    path: resource.path.clone(),
                    run_id: Some(resource.run_id.clone()),
                    owner_pid: Some(resource.owner_pid),
                    lifecycle_state: Some(resource.lifecycle_state.clone()),
                    recovery_command: scratch_recovery_command(resource),
                    reason,
                },
            )?;
            continue;
        }
        let size_bytes = path_size(&path)?;
        // Would this candidate also be eligible under the DEFAULT policy (no
        // override)? The override only relaxes the retention time window, and
        // every other guard has already passed, so default-eligibility reduces
        // to the default retention window still being expired.
        if resource_retention_window_expired(resource, &path, now) {
            default_policy_eligible_bytes += size_bytes;
        }
        eligible.push(ControllerScratchCandidate {
            path: resource.path.clone(),
            run_id: resource.run_id.clone(),
            task_id: resource.task_id.clone(),
            size_bytes,
            owner_pid: resource.owner_pid,
            lease_id: resource.lease_id.clone(),
            reason: resource.terminal_reason.clone().unwrap_or_default(),
            lifecycle_state: resource.lifecycle_state.clone(),
            source_ref: resource.source_ref.clone(),
        });
    }
    let stale_registration_count = stranded.len();
    let mut stale_registrations_pruned = 0;
    if options.apply {
        for position in stranded {
            let resource = &mut index.resources[position];
            let Some(registration) = resource.git_worktree.as_ref() else {
                continue;
            };
            if prune_stranded_worktree_registration(registration) {
                // Converged. Clearing the record keeps the lifecycle row as
                // evidence without re-reporting a registration that is gone.
                resource.git_worktree = None;
                stale_registrations_pruned += 1;
                reconciled = true;
            }
        }
    }
    if reconciled {
        write_index_unlocked(&index)?;
    }

    let candidate_count = eligible.len();
    let estimated_bytes = eligible.iter().map(|candidate| candidate.size_bytes).sum();
    let remaining: Vec<_> = eligible.iter().skip(options.limit).collect();
    let remaining_candidate_count = remaining.len();
    let remaining_candidate_bytes = remaining.iter().map(|candidate| candidate.size_bytes).sum();
    let has_more = remaining_candidate_count > 0;
    let mutation_candidate_count = candidate_count.min(options.limit);
    let mutation_candidates = eligible
        .iter()
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    for candidate in &mutation_candidates {
        if options.apply {
            match remove_candidate(&candidate, now, options.retention_override_seconds)? {
                ScratchRemoval::Removed(bytes) => {
                    applied_count += 1;
                    reclaimed_bytes += bytes;
                }
                ScratchRemoval::Skipped(reason) => {
                    all_skips.push((reason.clone(), Some(candidate.run_id.clone())));
                    append_skipped_detail(
                        &mut skipped,
                        &mut skipped_bytes,
                        options.full,
                        ControllerScratchSkipped {
                            path: candidate.path.clone(),
                            run_id: Some(candidate.run_id.clone()),
                            owner_pid: Some(candidate.owner_pid),
                            lifecycle_state: Some(candidate.lifecycle_state.clone()),
                            recovery_command: None,
                            reason,
                        },
                    )?;
                }
            }
        }
    }
    let (candidates, candidate_detail) = present_detail(
        mutation_candidates,
        options.full,
        mutation_candidate_count,
        format!("{} --full", cleanup_command(options)),
    )?;
    let skipped_count = all_skips.len();
    let skipped_detail = skipped_detail_metadata(
        &skipped,
        skipped_count,
        skipped_bytes,
        options.full,
        format!("{} --full", cleanup_command(options)),
    );
    Ok(ControllerScratchCleanupOutput {
        command: "cleanup.controller-scratch",
        mode: if options.apply { "apply" } else { "dry_run" },
        registered_worktree_count,
        stale_registration_count,
        stale_registrations_pruned,
        candidate_count,
        applied_count,
        skipped_count,
        estimated_bytes,
        default_policy_eligible_bytes,
        override_unlocked_bytes: estimated_bytes.saturating_sub(default_policy_eligible_bytes),
        reclaimed_bytes,
        candidates,
        candidate_detail,
        retention_reasons: summarize_retention(&all_skips),
        skipped,
        skipped_detail,
        remaining_candidate_count,
        remaining_candidate_bytes,
        has_more,
        next_command: has_more.then(|| cleanup_command(options)),
        drain_command: cleanup_command(ControllerScratchCleanupOptions {
            apply: true,
            limit: options.limit.saturating_mul(10).max(1),
            full: false,
            retention_override_seconds: options.retention_override_seconds,
        }),
    })
}

fn append_skipped_detail(
    skipped: &mut Vec<ControllerScratchSkipped>,
    skipped_bytes: &mut usize,
    full: bool,
    detail: ControllerScratchSkipped,
) -> Result<()> {
    let detail_bytes = serde_json::to_vec(&detail)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize cleanup detail".to_string()),
            )
        })?
        .len();
    if full
        || (skipped.len() < OutputBudget::COLLECTION.max_items
            && skipped_bytes.saturating_add(detail_bytes) <= OutputBudget::COLLECTION.max_bytes)
    {
        *skipped_bytes = skipped_bytes.saturating_add(detail_bytes);
        skipped.push(detail);
    }
    Ok(())
}

fn skipped_detail_metadata(
    skipped: &[ControllerScratchSkipped],
    total_items: usize,
    returned_bytes: usize,
    full: bool,
    export_command: String,
) -> OutputTruncation {
    let returned_items = skipped.len();
    let truncated = returned_items < total_items;
    if full {
        return OutputTruncation {
            presentation: OutputPresentation::LosslessExport,
            total_items,
            returned_items,
            omitted_items: 0,
            total_bytes: returned_bytes,
            returned_bytes,
            omitted_bytes: 0,
            total_bytes_known: true,
            truncated: false,
            continue_command: export_command.clone(),
            export_command,
        };
    }
    OutputTruncation {
        presentation: OutputPresentation::BoundedCollection,
        total_items,
        returned_items,
        omitted_items: total_items.saturating_sub(returned_items),
        // The default path does not serialize omitted rows solely to compute a
        // byte total. Exact aggregate counts remain available above.
        total_bytes: returned_bytes,
        returned_bytes,
        omitted_bytes: 0,
        total_bytes_known: !truncated,
        truncated,
        continue_command: export_command.clone(),
        export_command,
    }
}

/// Bound response presentation separately from the ordered set admitted for
/// mutation. `--full` is the explicit lossless inspection mode.
fn present_detail<T>(
    items: Vec<T>,
    full: bool,
    total_items: usize,
    export_command: String,
) -> Result<(Vec<T>, OutputTruncation)>
where
    T: Clone + Serialize,
{
    let total_bytes = items.iter().try_fold(0usize, |total, item| {
        serde_json::to_vec(item)
            .map(|value| total.saturating_add(value.len()))
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize cleanup detail".to_string()),
                )
            })
    })?;
    if full {
        return Ok((
            items,
            OutputTruncation {
                presentation: OutputPresentation::LosslessExport,
                total_items,
                returned_items: total_items,
                omitted_items: 0,
                total_bytes,
                returned_bytes: total_bytes,
                omitted_bytes: 0,
                total_bytes_known: true,
                truncated: false,
                continue_command: export_command.clone(),
                export_command,
            },
        ));
    }

    let mut returned = Vec::new();
    let mut returned_bytes: usize = 0;
    for item in items {
        let item_bytes = serde_json::to_vec(&item)
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize cleanup detail".to_string()),
                )
            })?
            .len();
        if returned.len() >= OutputBudget::COLLECTION.max_items
            || returned_bytes.saturating_add(item_bytes) > OutputBudget::COLLECTION.max_bytes
        {
            break;
        }
        returned_bytes += item_bytes;
        returned.push(item);
    }
    let returned_items = returned.len();
    let omitted_items = total_items.saturating_sub(returned_items);
    Ok((
        returned,
        OutputTruncation {
            presentation: OutputPresentation::BoundedCollection,
            total_items,
            returned_items,
            omitted_items,
            total_bytes,
            returned_bytes,
            omitted_bytes: total_bytes.saturating_sub(returned_bytes),
            total_bytes_known: true,
            truncated: omitted_items > 0,
            continue_command: export_command.clone(),
            export_command,
        },
    ))
}

/// Summarize every skipped resource by reason and owning run. Takes the complete
/// lightweight `(reason, run_id)` record so aggregates stay exact when the
/// default response bounds detailed rows.
fn summarize_retention(
    all_skips: &[(String, Option<String>)],
) -> Vec<ControllerScratchRetentionReason> {
    let mut reasons = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for (reason, run_id) in all_skips {
        let Some(run_id) = run_id.as_deref() else {
            continue;
        };
        *reasons
            .entry(reason.clone())
            .or_default()
            .entry(run_id.to_string())
            .or_default() += 1;
    }
    reasons
        .into_iter()
        .map(|(reason, owners)| {
            let resource_count = owners.values().sum();
            let mut owners = owners
                .into_iter()
                .map(|(run_id, resource_count)| ControllerScratchRetentionOwner {
                    run_id,
                    resource_count,
                })
                .collect::<Vec<_>>();
            owners.sort_by(|left, right| {
                right
                    .resource_count
                    .cmp(&left.resource_count)
                    .then_with(|| left.run_id.cmp(&right.run_id))
            });
            let additional_owner_count = owners.len().saturating_sub(RETENTION_OWNER_SUMMARY_LIMIT);
            owners.truncate(RETENTION_OWNER_SUMMARY_LIMIT);
            ControllerScratchRetentionReason {
                reason,
                resource_count,
                owners,
                additional_owner_count,
            }
        })
        .collect()
}

fn cleanup_block_reason(
    resource: &mut ControllerScratchResource,
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
    retention_override_seconds: Option<i64>,
) -> Result<Option<String>> {
    if !resource.reconstructable {
        return Ok(Some(
            "resource is not explicitly reconstructable".to_string(),
        ));
    }
    let root = PathBuf::from(&resource.root_bound).canonicalize().ok();
    let canonical = path.canonicalize().ok();
    if root.is_none()
        || canonical.is_none()
        || !canonical
            .as_ref()
            .is_some_and(|path| path.starts_with(root.as_ref().unwrap()))
    {
        return Ok(Some(
            "resource escaped its registered root bound".to_string(),
        ));
    }
    let terminal = ObservationStore::open_initialized()?
        .get_run(&resource.run_id)?
        .map(|run| run.status != "running")
        .unwrap_or(true);
    if !terminal {
        return Ok(Some("owning run is still active".to_string()));
    }
    if homeboy_core::process::pid_is_running(resource.owner_pid) {
        return Ok(Some("owner process is still running".to_string()));
    }
    if resource.lifecycle_state == "active" {
        resource.lifecycle_state = "orphaned".to_string();
        resource.finalized_at = Some(now.to_rfc3339());
        resource.interrupted_at = Some(now.to_rfc3339());
        resource.terminal_reason = Some("terminal_run_or_missing_lease_owner".to_string());
        resource.terminal_evidence = Some(serde_json::json!({
            "owner_pid": resource.owner_pid,
            "stale_retention": INTERRUPTED_RETENTION,
        }));
        return Ok(Some(
            "terminal or missing run has a dead active lease owner; orphaned retention has started"
                .to_string(),
        ));
    }
    if resource.finalized_at.is_none() {
        return Ok(Some(
            "resource has not been finalized by its owning run".to_string(),
        ));
    }
    // Only the time-window comparison honors the override. All the guards above
    // (still-active run, running owner, active→orphaned transition, not yet
    // finalized) have already returned, and the dirty/unpushed guard below still
    // applies — so an aggressive override can only converge released, clean,
    // finalized, terminal scratch, never in-use resources.
    let override_retention = retention_override_seconds.map(|seconds| format!("{seconds}s"));
    let default_retention = default_retention_window(resource);
    let retention = override_retention.as_deref().unwrap_or(default_retention);
    if !retention_expired(resource.finalized_at.as_deref(), retention, path, now) {
        return Ok(Some("retention has not expired".to_string()));
    }
    if !resource.ephemeral {
        match git_safety_path(resource, path) {
            Some(path) if recovered_workspace_matches(resource, &path) => {}
            Some(path) if git_dirty_or_unpushed(&path) => {
                return Ok(Some("git checkout has dirty or unpushed state".to_string()));
            }
            Some(_) => {}
            None if resource.plan_id.is_empty() => {
                return Ok(Some(
                    "resource has no explicit authoritative Git checkout".to_string(),
                ));
            }
            None => {}
        }
    }
    Ok(None)
}

/// The retention window a resource is subject to under the DEFAULT policy (no
/// `--older-than-days` override): the shorter orphaned/interrupted window when
/// applicable, otherwise the resource's own configured retention.
fn default_retention_window(resource: &ControllerScratchResource) -> &str {
    if matches!(
        resource.lifecycle_state.as_str(),
        "interrupted" | "orphaned"
    ) {
        INTERRUPTED_RETENTION
    } else {
        &resource.retention
    }
}

/// Whether an already-eligible candidate would ALSO clear its default retention
/// window (i.e. would be a candidate even without an override). Callers must
/// only use this for resources that have already passed the other cleanup
/// guards; it evaluates the time window alone.
fn resource_retention_window_expired(
    resource: &ControllerScratchResource,
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    retention_expired(
        resource.finalized_at.as_deref(),
        default_retention_window(resource),
        path,
        now,
    )
}

fn retention_expired(
    finalized_at: Option<&str>,
    retention: &str,
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let seconds = retention
        .strip_suffix('s')
        .and_then(|value| value.parse::<i64>().ok())
        .or_else(|| {
            retention
                .strip_prefix('P')
                .and_then(|value| value.strip_suffix('D'))
                .and_then(|value| value.parse::<i64>().ok())
                .map(|days| days * 86400)
        })
        .unwrap_or(i64::MAX);
    let reference = finalized_at
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .or_else(|| {
            fs::metadata(path)
                .ok()?
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
        });
    reference.is_some_and(|time| now.signed_duration_since(time).num_seconds() >= seconds)
}

/// Outcome of one destructive-boundary attempt. A skip carries its exact reason
/// so a lease retained for an unregisterable Git worktree is reported as such
/// rather than as an anonymous "changed or disappeared".
enum ScratchRemoval {
    Removed(u64),
    Skipped(String),
}

const REMOVAL_RACE_REASON: &str = "resource changed or disappeared before deletion";

fn remove_candidate(
    candidate: &ControllerScratchCandidate,
    now: chrono::DateTime<chrono::Utc>,
    retention_override_seconds: Option<i64>,
) -> Result<ScratchRemoval> {
    let mut index = read_index_unlocked()?;
    let Some(position) = index.resources.iter().position(|resource| {
        resource.lease_id == candidate.lease_id
            && resource.path == candidate.path
            && resource.owner_pid == candidate.owner_pid
    }) else {
        return Ok(ScratchRemoval::Skipped(REMOVAL_RACE_REASON.to_string()));
    };
    let path = PathBuf::from(&index.resources[position].path);
    // Re-validate with the SAME retention override used for eligibility, so a
    // candidate that qualified under `--older-than-days` is not spuriously
    // re-blocked here (which would leave it un-deleted forever).
    if !path.exists()
        || cleanup_block_reason(
            &mut index.resources[position],
            &path,
            now,
            retention_override_seconds,
        )?
        .is_some()
    {
        write_index_unlocked(&index)?;
        return Ok(ScratchRemoval::Skipped(REMOVAL_RACE_REASON.to_string()));
    }
    let bytes = path_size(&path)?;
    // Git first. A registered worktree deleted behind Git leaves a live
    // registration in a shared repository that nothing reaps (#10568).
    if let Some(reason) = unregister_scratch_git_worktrees(&path) {
        write_index_unlocked(&index)?;
        return Ok(ScratchRemoval::Skipped(reason));
    }
    // `git worktree remove` already deleted the worktree; the enclosing lease
    // directory can still hold provider scratch beside it.
    if path.exists() {
        fs::remove_dir_all(&path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("remove {}", path.display())),
            )
        })?;
    }
    // The row itself is deliberately retained as terminal lifecycle evidence.
    // Its Git registration is not: that registration no longer exists, and
    // leaving the record behind would keep re-reporting a stranded registration
    // that has already converged.
    index.resources[position].git_worktree = None;
    write_index_unlocked(&index)?;
    Ok(ScratchRemoval::Removed(bytes))
}

fn cleanup_command(options: ControllerScratchCleanupOptions) -> String {
    let mut command = format!(
        "homeboy cleanup --include controller-scratch --limit {}",
        options.limit
    );
    if options.apply {
        command.push_str(" --apply");
    }
    if let Some(seconds) = options.retention_override_seconds {
        // CLI input is integral days, and production callers preserve that
        // representation as seconds in cleanup options.
        command.push_str(&format!(" --older-than-days {}", seconds / 86_400));
    }
    command
}

fn git_dirty_or_unpushed(path: &Path) -> bool {
    // Include ignored files. They are still local source state and therefore
    // must be retained unless the terminal recovery path has preserved it.
    let status = git::run_git(
        path,
        &["status", "--porcelain=v1", "--ignored"],
        "git status",
    );
    let Ok(status) = status else {
        return true;
    };
    if !status.trim().is_empty() {
        return true;
    }
    // A linked attempt worktree is created with `git worktree add --detach`, so
    // it has no upstream *by construction*: `@{upstream}..HEAD` can never
    // resolve, the error branch below answered "unpushed", and every attempt
    // worktree was therefore retained forever. That is the non-convergence
    // behind #10568's 174 surviving attempt worktrees. Reachability is the
    // question that actually applies to a detached checkout.
    if let Some(registration) = linked_worktree_registration(path) {
        return !attempt_worktree_commits_are_preserved(path, Path::new(&registration.source_root));
    }
    git::run_git(
        path,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        "git rev-list upstream",
    )
    .map(|count| count.trim() != "0")
    .unwrap_or(true)
}

fn git_safety_path(resource: &ControllerScratchResource, path: &Path) -> Option<PathBuf> {
    if is_git_checkout(path) {
        return Some(path.to_path_buf());
    }

    // Scheduler-owned attempt roots contain one authoritative checkout at this
    // fixed path. Other nested repositories are provider/test temporary state,
    // not source candidates, and must not retain the whole scratch lease.
    if !resource.plan_id.is_empty() {
        let workspace = path.join("workspace");
        if is_git_checkout(&workspace) {
            return Some(workspace);
        }
    }

    None
}

fn recovery_evidence_is_current(resource: &ControllerScratchResource) -> bool {
    let recovery = resource
        .terminal_evidence
        .as_ref()
        .and_then(|evidence| evidence.get("workspace_recovery"));
    match recovery
        .and_then(|recovery| recovery.get("state"))
        .and_then(serde_json::Value::as_str)
    {
        Some("recovered") => git_safety_path(resource, Path::new(&resource.path))
            .is_some_and(|workspace| recovered_workspace_matches(resource, &workspace)),
        Some("explicitly_ephemeral") => resource.ephemeral,
        Some("authoritative_checkout_absent") => {
            !resource.plan_id.is_empty()
                && git_safety_path(resource, Path::new(&resource.path)).is_none()
        }
        _ => false,
    }
}

fn recover_authoritative_workspace(resource: &ControllerScratchResource) -> serde_json::Value {
    recover_authoritative_workspace_inner(resource).unwrap_or_else(|error| {
        serde_json::json!({
            "state": "recovery_failed",
            "message": error.message,
        })
    })
}

fn recover_authoritative_workspace_inner(
    resource: &ControllerScratchResource,
) -> Result<serde_json::Value> {
    if resource.ephemeral {
        return Ok(serde_json::json!({ "state": "explicitly_ephemeral" }));
    }
    let root = Path::new(&resource.path);
    let Some(workspace) = git_safety_path(resource, root) else {
        return Ok(serde_json::json!({
            "state": if resource.plan_id.is_empty() {
                "authoritative_checkout_unknown"
            } else {
                "authoritative_checkout_absent"
            },
        }));
    };
    let status = git::run_git(
        &workspace,
        &["status", "--porcelain=v1", "--ignored"],
        "git status",
    )?;
    if status
        .lines()
        .any(|line| line.starts_with("??") || line.starts_with("!!"))
    {
        return Ok(serde_json::json!({
            "state": "untracked_changes_retained",
            "workspace": workspace,
        }));
    }
    let staged = git::run_git(&workspace, &["ls-files", "--stage"], "git ls-files --stage")?;
    if staged.lines().any(|line| line.starts_with("160000 ")) {
        return Ok(serde_json::json!({
            "state": "submodules_retained",
            "workspace": workspace,
        }));
    }
    let head = git::run_git(&workspace, &["rev-parse", "HEAD"], "git rev-parse HEAD")?;
    let patch = git::run_git(
        &workspace,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--find-renames",
            "HEAD",
            "--",
            ".",
        ],
        "git diff HEAD",
    )?;
    let recovery_root = paths::artifact_root()?
        .join("controller-scratch-recovery")
        .join(paths::sanitize_path_segment(&resource.run_id))
        .join(paths::sanitize_path_segment(&resource.lease_id));
    fs::create_dir_all(&recovery_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", recovery_root.display())),
        )
    })?;
    let bundle_path = recovery_root.join("workspace.bundle");
    git::run_git(
        &workspace,
        &[
            "bundle",
            "create",
            &bundle_path.display().to_string(),
            "HEAD",
        ],
        "git bundle create",
    )?;
    let patch_path = recovery_root.join("tracked-changes.patch");
    fs::write(&patch_path, patch.as_bytes()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", patch_path.display())),
        )
    })?;
    let bundle_sha256 = file_sha256(&bundle_path)?;
    let patch_sha256 = sha256(patch.as_bytes());
    Ok(serde_json::json!({
        "state": "recovered",
        "workspace": workspace,
        "head": head.trim(),
        "status": status,
        "bundle": {
            "path": bundle_path,
            "sha256": bundle_sha256,
        },
        "patch": {
            "path": patch_path,
            "sha256": patch_sha256,
        },
    }))
}

fn recovered_workspace_matches(resource: &ControllerScratchResource, workspace: &Path) -> bool {
    let Some(recovery) = resource
        .terminal_evidence
        .as_ref()
        .and_then(|evidence| evidence.get("workspace_recovery"))
        .filter(|recovery| recovery["state"] == "recovered")
    else {
        return false;
    };
    let Some(bundle_path) = recovery
        .pointer("/bundle/path")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(bundle_sha256) = recovery
        .pointer("/bundle/sha256")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(patch_path) = recovery
        .pointer("/patch/path")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(patch_sha256) = recovery
        .pointer("/patch/sha256")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let expected_head = recovery.get("head").and_then(serde_json::Value::as_str);
    let expected_status = recovery.get("status").and_then(serde_json::Value::as_str);
    let current_head = git::run_git(workspace, &["rev-parse", "HEAD"], "git rev-parse HEAD").ok();
    let current_status = git::run_git(
        workspace,
        &["status", "--porcelain=v1", "--ignored"],
        "git status",
    )
    .ok();
    let current_patch = git::run_git(
        workspace,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--find-renames",
            "HEAD",
            "--",
            ".",
        ],
        "git diff HEAD",
    )
    .ok();

    expected_head == current_head.as_deref().map(str::trim)
        && expected_status == current_status.as_deref()
        && current_patch
            .as_deref()
            .is_some_and(|patch| sha256(patch.as_bytes()) == patch_sha256)
        && file_sha256(Path::new(bundle_path)).ok().as_deref() == Some(bundle_sha256)
        && file_sha256(Path::new(patch_path)).ok().as_deref() == Some(patch_sha256)
}

fn file_sha256(path: &Path) -> Result<String> {
    content_hash::sha256_file(path)
}

fn sha256(bytes: &[u8]) -> String {
    content_hash::sha256_hex(bytes)
}

fn scratch_recovery_command(resource: &ControllerScratchResource) -> Option<String> {
    (resource.lifecycle_state == "interrupted")
        .then(|| format!("homeboy agent-task cancel {}", resource.run_id))
}

fn is_git_checkout(path: &Path) -> bool {
    git::run_git(
        path,
        &["rev-parse", "--show-toplevel"],
        "git rev-parse top-level",
    )
    .ok()
    .and_then(|top_level| PathBuf::from(top_level.trim()).canonicalize().ok())
    .zip(path.canonicalize().ok())
    .is_some_and(|(top_level, path)| top_level == path)
}

fn path_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }
    fs::read_dir(path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?
        .try_fold(metadata.len(), |total, entry| {
            Ok(total
                + path_size(
                    &entry
                        .map_err(|error| Error::internal_io(error.to_string(), None))?
                        .path(),
                )?)
        })
}

fn index_path() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("controller-scratch/resources.json"))
}

fn with_index_lock<T>(index_path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_path = index_path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(lock_path.display().to_string()))
        })?;
    let _guard = ControllerScratchIndexLock::lock(file)?;
    operation()
}

struct ControllerScratchIndexLock {
    file: File,
}

impl ControllerScratchIndexLock {
    #[cfg(unix)]
    fn lock(file: File) -> Result<Self> {
        use std::os::fd::AsRawFd;

        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock controller scratch index".to_string()),
            ));
        }
        Ok(Self { file })
    }

    #[cfg(not(unix))]
    fn lock(file: File) -> Result<Self> {
        Ok(Self { file })
    }
}

impl Drop for ControllerScratchIndexLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

#[cfg(test)]
fn read_index() -> Result<ControllerScratchIndex> {
    let index_path = index_path()?;
    with_index_lock(&index_path, read_index_unlocked)
}

fn read_index_unlocked() -> Result<ControllerScratchIndex> {
    read_index_at_unlocked(&index_path()?)
}

fn read_index_at_unlocked(path: &Path) -> Result<ControllerScratchIndex> {
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|error| {
            Error::internal_json(error.to_string(), Some(path.display().to_string()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ControllerScratchIndex {
            schema: schema(),
            resources: Vec::new(),
        }),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

/// A controller using an older non-atomic writer can expose transient partial
/// bytes even while this process holds the index lock. Retry once before
/// classifying the durable index as malformed.
fn read_or_recover_index_at_unlocked(path: &Path) -> Result<ControllerScratchIndex> {
    let raw = read_index_bytes(path)?;
    let first_error = match serde_json::from_str(&raw) {
        Ok(index) => return Ok(index),
        Err(error) => error,
    };
    let retried_raw = read_index_bytes(path)?;
    match serde_json::from_str(&retried_raw) {
        Ok(index) => Ok(index),
        Err(retried_error) => {
            if let Ok(document) = serde_json::from_str::<serde_json::Value>(&retried_raw) {
                return salvage_compatible_resources(path, document, &retried_error.to_string());
            }
            let quarantine_path = preserve_index_bytes(path, "corrupt")?;
            eprintln!(
                "Homeboy quarantined syntactically invalid controller scratch index {} to {} after two reads; first parse error: {}; retry parse error: {}; allocating a fresh scratch root",
                path.display(),
                quarantine_path.display(),
                first_error,
                retried_error
            );
            Ok(ControllerScratchIndex {
                schema: schema(),
                resources: Vec::new(),
            })
        }
    }
}

fn read_index_bytes(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("{}".to_string()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

fn salvage_compatible_resources(
    path: &Path,
    document: serde_json::Value,
    parse_context: &str,
) -> Result<ControllerScratchIndex> {
    let Some(document) = document.as_object() else {
        return quarantine_incompatible_index(
            path,
            parse_context,
            "top-level JSON value is not an object",
        );
    };
    let Some(resources) = document
        .get("resources")
        .and_then(serde_json::Value::as_array)
    else {
        return quarantine_incompatible_index(
            path,
            parse_context,
            "top-level resources field is not an array",
        );
    };
    let mut compatible_resources = Vec::with_capacity(resources.len());
    let mut incompatible_count = 0;
    for resource in resources {
        match serde_json::from_value(resource.clone()) {
            Ok(resource) => compatible_resources.push(resource),
            Err(_) => incompatible_count += 1,
        }
    }
    let preserved_path = preserve_index_bytes(path, "incompatible")?;
    eprintln!(
        "Homeboy salvaged {} compatible controller scratch resources from {} after typed parse failure ({} incompatible resource(s); context: {}); preserved original bytes at {}",
        compatible_resources.len(),
        path.display(),
        incompatible_count,
        parse_context,
        preserved_path.display()
    );
    Ok(ControllerScratchIndex {
        schema: document
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(schema),
        resources: compatible_resources,
    })
}

fn quarantine_incompatible_index(
    path: &Path,
    parse_context: &str,
    shape_context: &str,
) -> Result<ControllerScratchIndex> {
    let quarantine_path = preserve_index_bytes(path, "incompatible")?;
    eprintln!(
        "Homeboy quarantined controller scratch index {} to {} because it is syntactically valid but structurally incompatible ({}; parse context: {}); allocating a fresh scratch root",
        path.display(),
        quarantine_path.display(),
        shape_context,
        parse_context
    );
    Ok(ControllerScratchIndex {
        schema: schema(),
        resources: Vec::new(),
    })
}

fn preserve_index_bytes(path: &Path, classification: &str) -> Result<PathBuf> {
    let preserved_path = path.with_file_name(format!(
        "{}.{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("resources.json"),
        classification,
        uuid::Uuid::new_v4()
    ));
    fs::rename(path, &preserved_path).map_err(|error| {
        Error::internal_io(
            format!(
                "could not preserve controller scratch index {} at {}: {}",
                path.display(),
                preserved_path.display(),
                error
            ),
            Some(path.display().to_string()),
        )
    })?;
    Ok(preserved_path)
}

#[cfg(test)]
fn write_index(index: &ControllerScratchIndex) -> Result<()> {
    let index_path = index_path()?;
    with_index_lock(&index_path, || write_index_unlocked(index))
}

fn write_index_unlocked(index: &ControllerScratchIndex) -> Result<()> {
    write_index_at_unlocked(&index_path()?, index)
}

fn write_index_at_unlocked(path: &Path, index: &ControllerScratchIndex) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let raw = serde_json::to_vec_pretty(index)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, raw).map_err(|error| Error::internal_io(error.to_string(), None))?;
    fs::rename(&temporary, path).map_err(|error| Error::internal_io(error.to_string(), None))
}
fn schema() -> String {
    CONTROLLER_SCRATCH_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn resource(path: &Path, root: &Path) -> ControllerScratchResource {
        ControllerScratchResource {
            path: path.display().to_string(),
            run_id: "missing-terminal-run".to_string(),
            plan_id: "test-plan".to_string(),
            task_id: "task-1".to_string(),
            attempt: 0,
            root_bound: root.display().to_string(),
            owner_pid: u32::MAX,
            lifecycle_state: "active".to_string(),
            lease_id: "test-lease".to_string(),
            reconstructable: true,
            ephemeral: false,
            retention: "0s".to_string(),
            source_ref: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            finalized_at: Some(chrono::Utc::now().to_rfc3339()),
            terminal_reason: None,
            terminal_evidence: None,
            interrupted_at: None,
            git_worktree: None,
        }
    }

    #[test]
    fn active_owner_protects_expired_reconstructable_resource() {
        let root = tempfile::tempdir().expect("root");
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).expect("scratch");
        let mut resource = resource(&scratch, root.path());
        resource.owner_pid = std::process::id();

        assert_eq!(
            cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), None).expect("check"),
            Some("owner process is still running".to_string())
        );
    }

    #[test]
    fn active_durable_run_protects_a_stale_lease_pid() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            let run = ObservationStore::open_initialized()
                .expect("store")
                .start_run(homeboy_core::observation::NewRunRecord::builder("test").build())
                .expect("running run");
            let mut resource = resource(&scratch, root.path());
            resource.run_id = run.id;
            resource.finalized_at = None;

            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), None)
                    .expect("check"),
                Some("owning run is still active".to_string())
            );
            assert_eq!(resource.lifecycle_state, "active");
            assert!(resource.finalized_at.is_none());
        });
    }

    #[test]
    fn terminal_or_missing_run_with_dead_active_lease_becomes_orphaned_and_waits_retention() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource(&scratch, root.path())],
            })
            .expect("index");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("reconcile");
            assert_eq!(output.candidate_count, 0);
            assert_eq!(
                output
                    .skipped
                    .iter()
                    .find(|skipped| skipped.path == scratch.display().to_string())
                    .expect("scratch skip")
                    .reason,
                "terminal or missing run has a dead active lease owner; orphaned retention has started"
            );
            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .next()
                .expect("resource");
            assert_eq!(resource.lifecycle_state, "orphaned");
            assert_eq!(
                resource.terminal_reason.as_deref(),
                Some("terminal_run_or_missing_lease_owner")
            );
            assert_eq!(
                resource.interrupted_at.as_deref(),
                resource.finalized_at.as_deref()
            );
            let interrupted_at = resource.finalized_at.as_deref().expect("finalized");
            let interrupted_at = chrono::DateTime::parse_from_rfc3339(interrupted_at)
                .expect("timestamp")
                .with_timezone(&chrono::Utc);
            assert!(!retention_expired(
                resource.finalized_at.as_deref(),
                INTERRUPTED_RETENTION,
                &scratch,
                interrupted_at
            ));
            assert!(retention_expired(
                resource.finalized_at.as_deref(),
                INTERRUPTED_RETENTION,
                &scratch,
                interrupted_at + chrono::Duration::days(1)
            ));
            assert_eq!(output.retention_reasons.len(), 1);
            assert_eq!(
                output.retention_reasons[0].reason,
                "terminal or missing run has a dead active lease owner; orphaned retention has started"
            );
            assert_eq!(output.retention_reasons[0].resource_count, 1);
            assert_eq!(
                output.retention_reasons[0].owners[0].run_id,
                "missing-terminal-run"
            );
        });
    }

    #[test]
    fn retention_summary_is_bounded_and_groups_owners_by_reason() {
        let skipped = (0..=RETENTION_OWNER_SUMMARY_LIMIT)
            .map(|index| {
                (
                    "retention has not expired".to_string(),
                    Some(format!("run-{index:02}")),
                )
            })
            .collect::<Vec<_>>();

        let summary = summarize_retention(&skipped);

        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].resource_count, RETENTION_OWNER_SUMMARY_LIMIT + 1);
        assert_eq!(summary[0].owners.len(), RETENTION_OWNER_SUMMARY_LIMIT);
        assert_eq!(summary[0].additional_owner_count, 1);
        assert_eq!(summary[0].owners[0].run_id, "run-00");
    }

    #[test]
    fn allocation_is_unique_contained_and_durably_indexed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let first = allocate_attempt("run-1", "plan-1", "task-1", 1).expect("first");
            let second = allocate_attempt("run-1", "plan-1", "task-1", 2).expect("second");
            let index = read_index().expect("index");

            assert_ne!(first.path, second.path);
            assert!(first.path.is_dir());
            assert!(second.path.is_dir());
            let resource = index
                .resources
                .iter()
                .find(|resource| resource.lease_id == first.lease_id)
                .expect("first resource");
            assert!(Path::new(&resource.path).starts_with(&resource.root_bound));
            assert_eq!(resource.run_id, "run-1");
            assert_eq!(resource.plan_id, "plan-1");
            assert_eq!(resource.task_id, "task-1");
            assert_eq!(resource.attempt, 1);
            assert_eq!(resource.lifecycle_state, "active");
            assert_eq!(resource.owner_pid, std::process::id());
            assert!(resource.reconstructable);
            assert!(!resource.created_at.is_empty());
        });
    }

    #[test]
    fn interrupted_ephemeral_git_checkout_is_reclaimable() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let allocation = allocate_attempt("run-candidate", "promotion", "candidate", 1)
                .expect("allocate candidate checkout");
            mark_attempt_ephemeral(&allocation).expect("mark ephemeral");
            let output = Command::new("git")
                .args(["init"])
                .current_dir(&allocation.path)
                .output()
                .expect("initialize candidate checkout");
            assert!(output.status.success());
            fs::write(allocation.path.join("candidate.txt"), "candidate\n")
                .expect("write candidate state");
            abandon_attempt_for_test(&allocation).expect("simulate interrupted owner");

            let first = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: Some(0),
            })
            .expect("reconcile interrupted checkout");
            assert_eq!(first.candidate_count, 0);

            let second = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 1,
                full: false,
                retention_override_seconds: Some(0),
            })
            .expect("reap interrupted checkout");
            assert_eq!(second.applied_count, 1);
            assert!(!allocation.path.exists());
        });
    }

    #[test]
    fn allocation_quarantines_a_malformed_index_and_registers_a_fresh_lease() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let index_path = index_path().expect("index path");
            fs::create_dir_all(index_path.parent().expect("index parent")).expect("index parent");
            fs::write(&index_path, "{ stale").expect("malformed index");

            let allocation = allocate_attempt("run-1", "plan-1", "task-1", 2)
                .expect("allocation recovers malformed index");
            let index = read_index().expect("replacement index");

            assert_eq!(index.resources.len(), 1);
            assert_eq!(index.resources[0].lease_id, allocation.lease_id);
            let quarantined = fs::read_dir(index_path.parent().expect("index parent"))
                .expect("index directory")
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.starts_with("resources.json.corrupt-"))
                .collect::<Vec<_>>();
            assert_eq!(quarantined.len(), 1);
        });
    }

    #[test]
    fn allocation_salvages_valid_leases_from_a_typed_incompatible_resource() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let index_path = index_path().expect("index path");
            fs::create_dir_all(index_path.parent().expect("index parent")).expect("index parent");
            let valid = resource(Path::new("/scratch/valid"), Path::new("/scratch"));
            fs::write(
                &index_path,
                serde_json::json!({
                    "schema": CONTROLLER_SCRATCH_SCHEMA,
                    "resources": [
                        valid,
                        { "path": 42 }
                    ]
                })
                .to_string(),
            )
            .expect("typed incompatible index");

            let allocation = allocate_attempt("run-1", "plan-1", "task-1", 2)
                .expect("allocation salvages compatible leases");
            let index = read_index().expect("replacement index");

            assert_eq!(index.resources.len(), 2);
            assert!(index
                .resources
                .iter()
                .any(|resource| resource.lease_id == "test-lease"));
            assert!(index
                .resources
                .iter()
                .any(|resource| resource.lease_id == allocation.lease_id));
            let preserved = fs::read_dir(index_path.parent().expect("index parent"))
                .expect("index directory")
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.starts_with("resources.json.incompatible-"))
                .collect::<Vec<_>>();
            assert_eq!(preserved.len(), 1);
        });
    }

    #[test]
    fn concurrent_allocation_and_release_preserves_every_lease() {
        homeboy_core::test_support::with_isolated_home(|_| {
            const WORKERS: usize = 8;
            const ALLOCATIONS_PER_WORKER: usize = 12;
            let handles: Vec<_> = (0..WORKERS)
                .map(|worker| {
                    std::thread::spawn(move || {
                        for attempt in 1..=ALLOCATIONS_PER_WORKER {
                            let allocation = allocate_attempt(
                                &format!("run-{worker}"),
                                "parallel-plan",
                                &format!("task-{worker}"),
                                attempt as u32,
                            )
                            .expect("allocate");
                            release_attempt(&allocation, "completed", serde_json::json!({}))
                                .expect("release");
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("worker");
            }

            let index = read_index().expect("parse index after concurrent updates");
            assert_eq!(index.resources.len(), WORKERS * ALLOCATIONS_PER_WORKER);
            assert!(index
                .resources
                .iter()
                .all(|resource| resource.lifecycle_state == "released"));
            assert!(index
                .resources
                .iter()
                .all(|resource| resource.finalized_at.is_some()));
        });
    }

    #[test]
    fn release_is_idempotent_and_preserves_first_terminal_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let allocation = allocate_attempt("run-1", "plan-1", "task-1", 1).expect("allocate");
            release_attempt(
                &allocation,
                "provider_failure",
                serde_json::json!({ "artifact": "first" }),
            )
            .expect("first release");
            release_attempt(
                &allocation,
                "cancelled",
                serde_json::json!({ "artifact": "second" }),
            )
            .expect("replayed release");

            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.lease_id == allocation.lease_id)
                .expect("resource");
            assert_eq!(resource.lifecycle_state, "released");
            assert_eq!(
                resource.terminal_reason.as_deref(),
                Some("provider_failure")
            );
            assert_eq!(
                resource.terminal_evidence,
                Some(serde_json::json!({ "artifact": "first" }))
            );
            assert!(resource.finalized_at.is_some());
        });
    }

    #[test]
    fn cleanup_ignores_unregistered_system_temp_paths() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path =
                std::env::temp_dir().join(format!("homeboy-unregistered-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).expect("unregistered scratch");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("cleanup inventory");

            assert!(output.candidates.is_empty());
            assert!(output.skipped.is_empty());
            assert!(path.exists());
            fs::remove_dir(&path).expect("remove unregistered scratch");
        });
    }

    #[test]
    fn released_resource_waits_for_its_configured_retention() {
        let root = tempfile::tempdir().expect("root");
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).expect("scratch");
        let mut resource = resource(&scratch, root.path());
        resource.lifecycle_state = "released".to_string();
        resource.retention = "P7D".to_string();

        assert_eq!(
            cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), None).expect("check"),
            Some("retention has not expired".to_string())
        );
    }

    #[test]
    fn retention_override_lets_clean_released_resource_converge() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // A finalized, clean, terminal resource WITHIN its P7D window is
            // skipped by default but becomes eligible with `Some(0)` override.
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();
            resource.retention = "P7D".to_string();

            // Default: still within the 7-day window, so it is retained.
            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), None)
                    .expect("default check"),
                Some("retention has not expired".to_string())
            );

            // Pressure override (expire-immediately): now eligible (no block).
            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), Some(0))
                    .expect("override check"),
                None
            );
        });
    }

    #[test]
    fn retention_override_does_not_bypass_dirty_guard() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // A dirty/unpushed resource stays skipped EVEN with an aggressive
            // override — the override only relaxes the time window, never the
            // dirty/unpushed safety guard.
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            run_git(&scratch, &["init", "-b", "main"]);
            fs::write(scratch.join("untracked.txt"), "dirty").expect("dirty file");
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();

            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), Some(0))
                    .expect("override check"),
                Some("git checkout has dirty or unpushed state".to_string())
            );
            assert!(scratch.exists());
        });
    }

    #[test]
    fn generated_nested_dirty_repo_does_not_retain_scheduler_scratch() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            let generated = scratch.join("generated-repo");
            fs::create_dir_all(&generated).expect("generated repo");
            run_git(&generated, &["init", "-b", "main"]);
            fs::write(generated.join("untracked.txt"), "generated").expect("dirty fixture");
            let mut resource = resource(&scratch, root.path());
            resource.plan_id = "scheduler-plan".to_string();
            resource.lifecycle_state = "released".to_string();

            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), Some(0))
                    .expect("cleanup check"),
                None
            );
        });
    }

    #[test]
    fn dirty_scheduler_attempt_workspace_retains_its_scratch() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            let workspace = scratch.join("workspace");
            fs::create_dir_all(&workspace).expect("attempt workspace");
            run_git(&workspace, &["init", "-b", "main"]);
            fs::write(workspace.join("untracked.txt"), "candidate").expect("dirty candidate");
            let mut resource = resource(&scratch, root.path());
            resource.plan_id = "scheduler-plan".to_string();
            resource.lifecycle_state = "released".to_string();

            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), Some(0))
                    .expect("cleanup check"),
                Some("git checkout has dirty or unpushed state".to_string())
            );
        });
    }

    #[test]
    fn finalized_workspace_changes_are_recovered_before_nested_fixtures_converge() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let allocation =
                allocate_attempt("recovery-run", "scheduler-plan", "task", 1).expect("allocate");
            let workspace = allocation.path.join("workspace");
            fs::create_dir(&workspace).expect("workspace");
            run_git(&workspace, &["init", "-b", "main"]);
            fs::write(workspace.join("tracked.txt"), "base\n").expect("base file");
            run_git(&workspace, &["add", "."]);
            run_git(
                &workspace,
                &[
                    "-c",
                    "user.name=Homeboy",
                    "-c",
                    "user.email=homeboy@example.test",
                    "commit",
                    "-m",
                    "base",
                ],
            );
            fs::write(workspace.join("tracked.txt"), "candidate\n").expect("candidate change");
            let generated = allocation.path.join("generated-fixture");
            fs::create_dir(&generated).expect("generated fixture");
            run_git(&generated, &["init", "-b", "main"]);
            fs::write(generated.join("untracked.txt"), "fixture").expect("dirty fixture");
            abandon_attempt_for_test(&allocation).expect("dead owner");

            finalize_run("recovery-run").expect("recover and finalize");
            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.lease_id == allocation.lease_id)
                .expect("resource");
            let recovery =
                &resource.terminal_evidence.as_ref().expect("evidence")["workspace_recovery"];
            assert_eq!(recovery["state"], "recovered");
            assert!(Path::new(recovery["bundle"]["path"].as_str().expect("bundle path")).is_file());
            assert!(Path::new(recovery["patch"]["path"].as_str().expect("patch path")).is_file());

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: Some(0),
            })
            .expect("cleanup preview");
            assert_eq!(output.candidate_count, 1);
            assert_eq!(
                output.candidates[0].path,
                allocation.path.display().to_string()
            );
        });
    }

    #[test]
    fn workspace_changed_after_recovery_remains_retained() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let allocation =
                allocate_attempt("changed-run", "scheduler-plan", "task", 1).expect("allocate");
            let workspace = allocation.path.join("workspace");
            fs::create_dir(&workspace).expect("workspace");
            run_git(&workspace, &["init", "-b", "main"]);
            fs::write(workspace.join("tracked.txt"), "base\n").expect("base file");
            run_git(&workspace, &["add", "."]);
            run_git(
                &workspace,
                &[
                    "-c",
                    "user.name=Homeboy",
                    "-c",
                    "user.email=homeboy@example.test",
                    "commit",
                    "-m",
                    "base",
                ],
            );
            fs::write(workspace.join("tracked.txt"), "first\n").expect("first change");
            abandon_attempt_for_test(&allocation).expect("dead owner");
            finalize_run("changed-run").expect("recover and finalize");
            fs::write(workspace.join("tracked.txt"), "second\n").expect("later change");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: Some(0),
            })
            .expect("cleanup preview");
            assert_eq!(output.candidate_count, 0);
            assert_eq!(
                output.skipped[0].reason,
                "git checkout has dirty or unpushed state"
            );
        });
    }

    #[test]
    fn ignored_untracked_workspace_state_fails_closed_during_recovery() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let allocation =
                allocate_attempt("ignored-run", "scheduler-plan", "task", 1).expect("allocate");
            let workspace = allocation.path.join("workspace");
            fs::create_dir(&workspace).expect("workspace");
            run_git(&workspace, &["init", "-b", "main"]);
            fs::write(workspace.join(".gitignore"), "local-source\n").expect("ignore rule");
            fs::write(workspace.join("tracked.txt"), "base\n").expect("base file");
            run_git(&workspace, &["add", "."]);
            run_git(
                &workspace,
                &[
                    "-c",
                    "user.name=Homeboy",
                    "-c",
                    "user.email=homeboy@example.test",
                    "commit",
                    "-m",
                    "base",
                ],
            );
            fs::write(workspace.join("local-source"), "preserve\n").expect("local source");

            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.lease_id == allocation.lease_id)
                .expect("resource");
            let recovery = recover_authoritative_workspace_inner(&resource).expect("recover");

            assert_eq!(recovery["state"], "untracked_changes_retained");
        });
    }

    #[test]
    fn submodule_workspaces_remain_retained() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let submodule = root.path().join("submodule");
            fs::create_dir(&submodule).expect("submodule source");
            run_git(&submodule, &["init", "-b", "main"]);
            fs::write(submodule.join("source.txt"), "source\n").expect("submodule source file");
            run_git(&submodule, &["add", "."]);
            run_git(
                &submodule,
                &[
                    "-c",
                    "user.name=Homeboy",
                    "-c",
                    "user.email=homeboy@example.test",
                    "commit",
                    "-m",
                    "submodule",
                ],
            );
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            run_git(&scratch, &["init", "-b", "main"]);
            run_git(
                &scratch,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    submodule.to_str().expect("submodule path"),
                    "vendor/submodule",
                ],
            );
            let mut resource = resource(&scratch, root.path());
            resource.plan_id.clear();

            let recovery =
                recover_authoritative_workspace_inner(&resource).expect("inspect workspace");

            assert_eq!(recovery["state"], "submodules_retained");
        });
    }

    #[test]
    fn unknown_legacy_checkout_layout_fails_closed_with_recovery_command() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            let mut legacy = resource(&scratch, root.path());
            legacy.plan_id.clear();
            legacy.lifecycle_state = "interrupted".to_string();
            legacy.run_id = "legacy-run".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![legacy],
            })
            .expect("legacy resource");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: Some(0),
            })
            .expect("cleanup preview");

            assert_eq!(output.candidate_count, 0);
            assert_eq!(
                output.skipped[0].reason,
                "resource has no explicit authoritative Git checkout"
            );
            assert_eq!(
                output.skipped[0].recovery_command.as_deref(),
                Some("homeboy agent-task cancel legacy-run")
            );
        });
    }

    #[test]
    fn finalizing_a_run_starts_interrupted_scratch_retention() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let allocation =
                allocate_attempt("terminal-run", "plan", "task", 1).expect("scratch allocation");

            finalize_run("terminal-run").expect("finalize scratch");

            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.lease_id == allocation.lease_id)
                .expect("resource");
            assert_eq!(resource.lifecycle_state, "interrupted");
            assert!(resource.finalized_at.is_some());
            assert!(resource.interrupted_at.is_some());
            assert_eq!(
                resource.terminal_reason.as_deref(),
                Some("owning_run_terminalized")
            );
        });
    }

    #[test]
    fn retention_override_does_not_bypass_running_owner_guard() {
        // A resource whose owner process is still running stays skipped even
        // with the override — the override never relaxes the running-owner guard.
        let root = tempfile::tempdir().expect("root");
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).expect("scratch");
        let mut resource = resource(&scratch, root.path());
        resource.lifecycle_state = "released".to_string();
        resource.owner_pid = std::process::id();

        assert_eq!(
            cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), Some(0))
                .expect("override check"),
            Some("owner process is still running".to_string())
        );
    }

    #[test]
    fn skipped_rows_have_bounded_default_detail_and_lossless_full_detail() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // Large retained inventories keep exact aggregates while the default
            // response stays inside the shared item and byte presentation budget.
            let root = tempfile::tempdir().expect("root");
            let total = 125;
            let mut resources = Vec::with_capacity(total);
            for index in 0..total {
                let scratch = root.path().join(format!("scratch-{index}"));
                fs::create_dir(&scratch).expect("scratch");
                let mut resource = resource(&scratch, root.path());
                resource.run_id = format!("run-{index}");
                resource.lease_id = format!("lease-{index}");
                resource.lifecycle_state = "released".to_string();
                // Within the retention window, so every resource is skipped.
                resource.retention = "P7D".to_string();
                resources.push(resource);
            }
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources,
            })
            .expect("resource index");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("cleanup");

            assert_eq!(output.candidate_count, 0);
            // True total is preserved even though rendered rows are bounded.
            assert_eq!(output.skipped_count, total);
            assert!(output.skipped.len() <= OutputBudget::COLLECTION.max_items);
            assert!(output.skipped_detail.returned_bytes <= OutputBudget::COLLECTION.max_bytes);
            assert_eq!(output.skipped_detail.total_items, total);
            assert_eq!(
                output.skipped_detail.omitted_items,
                total - output.skipped.len()
            );
            assert!(output.skipped_detail.truncated);
            assert!(!output.skipped_detail.total_bytes_known);
            // The aggregate summary still accounts for every retained resource.
            let summarized: usize = output
                .retention_reasons
                .iter()
                .map(|reason| reason.resource_count)
                .sum();
            assert_eq!(summarized, total);

            let full = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: true,
                retention_override_seconds: None,
            })
            .expect("full cleanup");
            assert_eq!(full.skipped.len(), total);
            assert!(!full.skipped_detail.truncated);
        });
    }

    #[test]
    fn thousands_of_candidates_keep_default_output_bounded_and_apply_exactly_the_limit() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let total = 1_000;
            let mut resources = Vec::with_capacity(total);
            for index in 0..total {
                let scratch = root.path().join(format!("scratch-{index}"));
                fs::create_dir(&scratch).expect("scratch");
                fs::write(scratch.join("payload"), "x").expect("payload");
                let mut resource = resource(&scratch, root.path());
                resource.run_id = format!("run-{index}");
                resource.lease_id = format!("lease-{index}");
                resource.lifecycle_state = "released".to_string();
                resource.ephemeral = true;
                resources.push(resource);
            }
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources,
            })
            .expect("resource index");

            let preview = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: total,
                full: false,
                retention_override_seconds: None,
            })
            .expect("preview");
            assert_eq!(preview.candidate_count, total);
            assert_eq!(preview.candidates.len(), OutputBudget::COLLECTION.max_items);
            assert!(preview.candidate_detail.returned_bytes <= OutputBudget::COLLECTION.max_bytes);
            assert_eq!(preview.candidate_detail.total_items, total);
            assert_eq!(
                preview.candidate_detail.omitted_items,
                total - preview.candidates.len()
            );
            assert!(preview.candidate_detail.truncated);
            assert_eq!(preview.remaining_candidate_count, 0);

            let applied = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: total,
                full: false,
                retention_override_seconds: None,
            })
            .expect("apply");
            assert_eq!(applied.candidate_count, total);
            assert_eq!(applied.applied_count, total);
            assert_eq!(applied.reclaimed_bytes, preview.estimated_bytes);
            assert_eq!(applied.candidate_detail.total_items, total);
            assert_eq!(applied.candidates.len(), OutputBudget::COLLECTION.max_items);
            assert!(!root.path().join("scratch-0").exists());
            assert!(!root.path().join(format!("scratch-{}", total - 1)).exists());
        });
    }

    #[test]
    fn full_detail_is_lossless_for_thousands_of_candidates_without_expanding_default_output() {
        let candidates = (0..2_000)
            .map(|index| ControllerScratchCandidate {
                path: format!("/scratch/{index}"),
                run_id: format!("run-{index}"),
                task_id: format!("task-{index}"),
                size_bytes: index,
                owner_pid: u32::MAX,
                lease_id: format!("lease-{index}"),
                reason: String::new(),
                lifecycle_state: "released".to_string(),
                source_ref: None,
            })
            .collect::<Vec<_>>();
        let (default, default_detail) = present_detail(
            candidates.clone(),
            false,
            candidates.len(),
            "homeboy cleanup --include controller-scratch --full".to_string(),
        )
        .expect("default detail");
        assert_eq!(default.len(), OutputBudget::COLLECTION.max_items);
        assert_eq!(default_detail.omitted_items, 2_000 - default.len());
        let (full, full_detail) = present_detail(
            candidates,
            true,
            2_000,
            "homeboy cleanup --include controller-scratch --full".to_string(),
        )
        .expect("full detail");
        assert_eq!(full.len(), 2_000);
        assert!(!full_detail.truncated);
    }

    #[test]
    fn override_unlocked_bytes_are_reported_separately_from_default_policy() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // A released, clean resource still inside its default P7D window:
            // eligible only because of the override. Its bytes must be reported
            // as override-unlocked, not default-policy-eligible.
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            fs::write(scratch.join("generated.txt"), "some generated bytes").expect("content");
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();
            resource.retention = "P7D".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource],
            })
            .expect("resource index");

            // Default policy: retained, nothing eligible, no override-unlocked bytes.
            let default_run = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 10,
                full: false,
                retention_override_seconds: None,
            })
            .expect("default cleanup");
            assert_eq!(default_run.candidate_count, 0);
            assert_eq!(default_run.estimated_bytes, 0);
            assert_eq!(default_run.default_policy_eligible_bytes, 0);
            assert_eq!(default_run.override_unlocked_bytes, 0);

            // Pressure override: now eligible, and every eligible byte is
            // attributed to the override (default policy would reclaim none).
            let override_run = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 10,
                full: false,
                retention_override_seconds: Some(0),
            })
            .expect("override cleanup");
            assert_eq!(override_run.candidate_count, 1);
            assert!(override_run.estimated_bytes > 0);
            assert_eq!(override_run.default_policy_eligible_bytes, 0);
            assert_eq!(
                override_run.override_unlocked_bytes,
                override_run.estimated_bytes
            );
        });
    }

    #[test]
    fn dirty_or_unpushed_checkout_is_preserved() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            run_git(&scratch, &["init", "-b", "main"]);
            fs::write(scratch.join("untracked.txt"), "dirty").expect("dirty file");
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();

            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now(), None)
                    .expect("check"),
                Some("git checkout has dirty or unpushed state".to_string())
            );
            assert!(scratch.exists());
        });
    }

    #[test]
    fn terminal_reconstructable_resource_is_inventoried_and_removed_with_byte_accounting() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            let remote = root.path().join("remote.git");
            run_git(
                root.path(),
                &["init", "--bare", remote.to_str().expect("remote path")],
            );
            fs::create_dir(&scratch).expect("scratch");
            fs::write(scratch.join("generated.txt"), "generated bytes").expect("content");
            run_git(&scratch, &["init", "-b", "main"]);
            run_git(&scratch, &["config", "user.email", "homeboy@example.test"]);
            run_git(&scratch, &["config", "user.name", "Homeboy Test"]);
            run_git(&scratch, &["add", "."]);
            run_git(&scratch, &["commit", "-m", "initial"]);
            run_git(
                &scratch,
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.to_str().expect("remote path"),
                ],
            );
            run_git(&scratch, &["push", "-u", "origin", "main"]);
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();
            // Older provider registrations had no lease. Recovery must still
            // reconcile that owned resource after the normal safety gates.
            resource.lease_id.clear();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource],
            })
            .expect("resource index");

            let inventory = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("inventory");
            assert_eq!(inventory.candidate_count, 1);
            assert!(inventory.estimated_bytes > 0);
            assert_eq!(inventory.reclaimed_bytes, 0);
            assert_eq!(inventory.candidates[0].owner_pid, u32::MAX);
            assert_eq!(inventory.candidates[0].lifecycle_state, "released");
            assert!(inventory.candidates[0].reason.is_empty());

            let applied = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("apply");
            assert_eq!(applied.applied_count, 1);
            assert_eq!(applied.reclaimed_bytes, inventory.estimated_bytes);
            assert!(!scratch.exists());
            let retained = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.path == scratch.display().to_string())
                .expect("retained lifecycle evidence");
            assert_eq!(retained.lifecycle_state, "released");
            assert_eq!(retained.path, scratch.display().to_string());

            let repeated = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("repeated apply");
            assert_eq!(repeated.applied_count, 0);
            assert_eq!(repeated.candidate_count, 0);
        });
    }

    #[test]
    fn apply_revalidates_a_resource_that_becomes_live() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            let mut stored = resource(&scratch, root.path());
            stored.lifecycle_state = "released".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![stored],
            })
            .expect("index");

            let candidate = ControllerScratchCandidate {
                path: scratch.display().to_string(),
                run_id: "missing-terminal-run".to_string(),
                task_id: "task-1".to_string(),
                size_bytes: 0,
                owner_pid: u32::MAX,
                lease_id: "test-lease".to_string(),
                reason: String::new(),
                lifecycle_state: "released".to_string(),
                source_ref: None,
            };
            let index = read_index().expect("index");
            let resource = &mut index.resources.into_iter().next().expect("resource");
            resource.owner_pid = std::process::id();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource.clone()],
            })
            .expect("live index");

            assert!(matches!(
                remove_candidate(&candidate, chrono::Utc::now(), None).expect("remove"),
                ScratchRemoval::Skipped(reason) if reason == REMOVAL_RACE_REASON
            ));
            assert!(scratch.exists());
        });
    }

    #[test]
    fn bounded_cleanup_reports_continuation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let remote = root.path().join("remote.git");
            run_git(
                root.path(),
                &["init", "--bare", remote.to_str().expect("remote")],
            );
            let first = clean_checkout(root.path(), &remote, "first");
            let second = root.path().join("second");
            run_git(
                root.path(),
                &[
                    "clone",
                    "-b",
                    "main",
                    remote.to_str().expect("remote"),
                    second.to_str().expect("second"),
                ],
            );
            let mut first_resource = resource(&first, root.path());
            first_resource.lifecycle_state = "released".to_string();
            let mut second_resource = resource(&second, root.path());
            second_resource.lease_id = "second-lease".to_string();
            second_resource.lifecycle_state = "released".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![first_resource, second_resource],
            })
            .expect("index");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
                full: false,
                retention_override_seconds: None,
            })
            .expect("preview");
            assert_eq!(output.candidate_count, 2);
            assert_eq!(output.candidates.len(), 1);
            assert_eq!(output.remaining_candidate_count, 1);
            assert!(output.remaining_candidate_bytes > 0);
            assert!(output.has_more);
            assert_eq!(
                output.next_command.as_deref(),
                Some("homeboy cleanup --include controller-scratch --limit 1")
            );
            assert_eq!(
                output.drain_command,
                "homeboy cleanup --include controller-scratch --limit 10 --apply"
            );
        });
    }

    #[test]
    fn cleanup_commands_preserve_retention_override_and_action() {
        let options = ControllerScratchCleanupOptions {
            apply: true,
            limit: 3,
            full: false,
            retention_override_seconds: Some(2 * 86_400),
        };
        assert_eq!(
            cleanup_command(options),
            "homeboy cleanup --include controller-scratch --limit 3 --apply --older-than-days 2"
        );
        let detail = skipped_detail_metadata(
            &[],
            1,
            0,
            false,
            format!("{} --full", cleanup_command(options)),
        );
        assert_eq!(
            detail.export_command,
            "homeboy cleanup --include controller-scratch --limit 3 --apply --older-than-days 2 --full"
        );
    }

    fn clean_checkout(root: &Path, remote: &Path, name: &str) -> PathBuf {
        let scratch = root.join(name);
        fs::create_dir(&scratch).expect("scratch");
        fs::write(scratch.join("generated.txt"), name).expect("content");
        run_git(&scratch, &["init", "-b", "main"]);
        run_git(&scratch, &["config", "user.email", "homeboy@example.test"]);
        run_git(&scratch, &["config", "user.name", "Homeboy Test"]);
        run_git(&scratch, &["add", "."]);
        run_git(&scratch, &["commit", "-m", "initial"]);
        run_git(
            &scratch,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        run_git(&scratch, &["push", "-u", "origin", "main"]);
        scratch
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }

    /// Build a source repository with one pushed commit on `main` plus a
    /// linked attempt worktree detached at that commit, exactly the shape
    /// `prepare_attempt_workspace` produces.
    fn attempt_worktree(root: &Path, lease: &Path) -> (PathBuf, PathBuf) {
        let remote = root.join("remote.git");
        run_git(root, &["init", "--bare", remote.to_str().expect("remote")]);
        let source = clean_checkout(root, &remote, "source");
        fs::create_dir_all(lease).expect("lease");
        let worktree = lease.join("workspace");
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().expect("worktree"),
                "main",
            ],
        );
        (source, worktree)
    }

    /// The `.git` pointer file Git writes in a linked worktree names its exact
    /// registration. A normal checkout has no registration to prune.
    #[test]
    fn linked_worktree_registration_is_read_from_gits_own_pointer_file() {
        let root = tempfile::tempdir().expect("root");
        let lease = root.path().join("lease");
        let (source, worktree) = attempt_worktree(root.path(), &lease);

        let registration =
            linked_worktree_registration(&worktree).expect("linked worktree registration");
        assert_eq!(
            Path::new(&registration.source_root),
            source.canonicalize().expect("source").as_path()
        );
        assert_eq!(
            Path::new(&registration.path)
                .canonicalize()
                .expect("recorded worktree"),
            worktree.canonicalize().expect("worktree")
        );
        assert!(Path::new(&registration.registration).is_dir());
        assert!(
            Path::new(&registration.registration)
                .canonicalize()
                .expect("registration")
                .starts_with(
                    source
                        .join(".git/worktrees")
                        .canonicalize()
                        .expect("registrations")
                ),
            "registration must live under the source repository"
        );

        assert!(
            linked_worktree_registration(&source).is_none(),
            "a normal checkout has no linked registration"
        );
        assert!(
            linked_worktree_registration(&lease).is_none(),
            "a plain directory has no linked registration"
        );
    }

    /// The preservation proof, in both directions. A detached attempt worktree
    /// sitting on a pushed branch tip holds nothing unique; one commit later it
    /// holds the only copy of that commit.
    #[test]
    fn attempt_worktree_preservation_requires_an_anchoring_ref() {
        let root = tempfile::tempdir().expect("root");
        let lease = root.path().join("lease");
        let (source, worktree) = attempt_worktree(root.path(), &lease);

        assert!(
            attempt_worktree_commits_are_preserved(&worktree, &source),
            "a detached checkout at a pushed branch tip holds no unique commit"
        );

        fs::write(worktree.join("candidate.txt"), "candidate\n").expect("candidate");
        run_git(&worktree, &["add", "."]);
        run_git(&worktree, &["commit", "-m", "candidate"]);

        assert!(
            !attempt_worktree_commits_are_preserved(&worktree, &source),
            "a commit reachable only from the attempt HEAD must never be considered preserved"
        );

        // A linked worktree shares its source repository's object store, so a
        // branch created there anchors the same commit and the proof flips back.
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree)
            .output()
            .expect("attempt HEAD");
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        run_git(&source, &["branch", "adopted", &head]);
        assert!(
            attempt_worktree_commits_are_preserved(&worktree, &source),
            "an anchoring branch makes the same commit preserved"
        );
    }

    /// #10568: cleanup must unregister through Git, not delete the directory
    /// behind it. Before this fix the source repository kept a live
    /// `.git/worktrees/<id>` entry after every reclaimed attempt.
    #[test]
    fn cleanup_unregisters_a_linked_attempt_worktree_instead_of_stranding_it() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let lease = root.path().join("lease");
            let (source, worktree) = attempt_worktree(root.path(), &lease);
            let registration = linked_worktree_registration(&worktree)
                .expect("linked worktree registration")
                .registration;

            let mut stored = resource(&lease, root.path());
            stored.lifecycle_state = "released".to_string();
            stored.git_worktree = linked_worktree_registration(&worktree);
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![stored],
            })
            .expect("index");

            let preview = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 10,
                full: true,
                retention_override_seconds: None,
            })
            .expect("preview");
            assert_eq!(preview.candidate_count, 1, "{:?}", preview.skipped);
            assert_eq!(preview.registered_worktree_count, 1);

            let applied = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 10,
                full: true,
                retention_override_seconds: None,
            })
            .expect("apply");
            assert_eq!(applied.applied_count, 1, "{:?}", applied.skipped);
            assert!(!lease.exists());
            assert!(
                !Path::new(&registration).exists(),
                "the Git registration must not outlive the worktree"
            );
            let listed = Command::new("git")
                .args(["worktree", "list", "--porcelain"])
                .current_dir(&source)
                .output()
                .expect("git worktree list");
            assert!(
                !String::from_utf8_lossy(&listed.stdout)
                    .contains(worktree.to_str().expect("worktree")),
                "the source repository must no longer list the attempt worktree"
            );
        });
    }

    /// Unpushed, unmerged work is sacred: it is reported, never reclaimed.
    #[test]
    fn an_attempt_worktree_holding_the_only_copy_of_a_commit_is_reported_not_removed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let lease = root.path().join("lease");
            let (_source, worktree) = attempt_worktree(root.path(), &lease);
            fs::write(worktree.join("candidate.txt"), "candidate\n").expect("candidate");
            run_git(&worktree, &["add", "."]);
            run_git(&worktree, &["commit", "-m", "unique candidate"]);

            let mut stored = resource(&lease, root.path());
            stored.lifecycle_state = "released".to_string();
            stored.git_worktree = linked_worktree_registration(&worktree);
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![stored],
            })
            .expect("index");

            let applied = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 10,
                full: true,
                // Even the most aggressive retention override must not reach it.
                retention_override_seconds: Some(0),
            })
            .expect("apply");

            assert_eq!(applied.applied_count, 0);
            assert_eq!(applied.candidate_count, 0);
            assert_eq!(
                applied.skipped[0].reason,
                "git checkout has dirty or unpushed state"
            );
            assert!(worktree.join("candidate.txt").exists());
        });
    }

    /// A registration whose worktree directory is already gone is pruned by
    /// identity, and only when the registration still agrees it is ours.
    #[test]
    fn stranded_registrations_are_pruned_by_identity_only() {
        let root = tempfile::tempdir().expect("root");
        let lease = root.path().join("lease");
        let (_source, worktree) = attempt_worktree(root.path(), &lease);
        let registration =
            linked_worktree_registration(&worktree).expect("linked worktree registration");

        assert!(
            !prune_stranded_worktree_registration(&registration),
            "a live worktree must never have its registration pruned"
        );

        // Reproduce the historical leak: delete the directory behind Git.
        fs::remove_dir_all(&worktree).expect("delete worktree behind git");
        assert!(Path::new(&registration.registration).is_dir());

        let mismatched = ControllerScratchGitWorktree {
            path: worktree.display().to_string(),
            source_root: registration.source_root.clone(),
            registration: Path::new(&registration.registration)
                .parent()
                .expect("registrations")
                .join("some-other-attempt")
                .display()
                .to_string(),
        };
        assert!(
            !prune_stranded_worktree_registration(&mismatched),
            "a registration that does not name this worktree is not ours"
        );

        fs::write(Path::new(&registration.registration).join("locked"), "held")
            .expect("lock registration");
        assert!(
            !prune_stranded_worktree_registration(&registration),
            "a locked registration retains, exactly as `git worktree prune` does"
        );
        fs::remove_file(Path::new(&registration.registration).join("locked")).expect("unlock");

        assert!(prune_stranded_worktree_registration(&registration));
        assert!(!Path::new(&registration.registration).exists());
        assert!(
            !prune_stranded_worktree_registration(&registration),
            "pruning is idempotent"
        );
    }
}
