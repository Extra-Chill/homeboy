use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use crate::component::{self, TargetSpec};
use crate::error::{Error, Result};
use crate::ownership;
use crate::{git, paths};
use fs4::fs_std::FileExt;
use serde::Serialize;

mod queue_ops;
mod store_ops;
mod types;

static TASK_WORKTREE_REGISTRY_GATE: OnceLock<RwLock<()>> = OnceLock::new();
const MALFORMED_RECORD_REPAIR_LIMIT: usize = 20;

pub use types::{
    AdoptedWorkspaceRecord, BranchCleanupIntent, BranchCleanupStatus, CleanupPolicy,
    TaskWorktreeRecord, TaskWorktreeState, WorkspaceRefRecord, WorktreeAdoptOptions,
    WorktreeAdoptOutput, WorktreeBranchCleanupReport, WorktreeCleanupCandidate,
    WorktreeCleanupCounts, WorktreeCleanupOptions, WorktreeCleanupOutput, WorktreeCleanupSkipped,
    WorktreeCreateOptions, WorktreeCreateOutput, WorktreeListOutput, WorktreeQueueCreateOptions,
    WorktreeQueueCreateOutput, WorktreeQueueCreateRow, WorktreeQueueCreateStatus,
    WorktreeQueueLockHolder, WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeSafetyReport,
    WorktreeStatusOutput,
};

pub fn create(options: WorktreeCreateOptions) -> Result<WorktreeCreateOutput> {
    create_with_store(options, &metadata_dir()?)
}

pub fn adopt(options: WorktreeAdoptOptions) -> Result<WorktreeAdoptOutput> {
    adopt_with_store(options, &adopted_metadata_dir()?)
}

pub fn list() -> Result<WorktreeListOutput> {
    with_task_worktree_registry_read_lock(list_unlocked)
}

pub(crate) fn list_unlocked() -> Result<WorktreeListOutput> {
    list_with_store(&metadata_dir()?)
}

/// Evidence retained when a malformed task-worktree record is removed from the
/// active registry. The original bytes and this provenance sidecar remain under
/// `.quarantine`, so uncertain activity is never silently discarded.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskWorktreeRegistryQuarantine {
    pub record_path: String,
    pub quarantine_path: String,
    pub provenance_path: String,
    pub reason: String,
    pub quarantined_at: String,
}

/// Move a bounded number of unreadable records out of the active registry.
/// Callers must treat any returned quarantine as unknown activity for their
/// current operation; the next operation can safely read the repaired set.
pub(crate) fn quarantine_malformed_task_worktree_records(
) -> Result<Vec<TaskWorktreeRegistryQuarantine>> {
    with_task_worktree_registry_write_lock(|| {
        let store = metadata_dir()?;
        if !store.exists() {
            return Ok(Vec::new());
        }
        let quarantine = store.join(".quarantine");
        let mut entries: Vec<_> = fs::read_dir(&store)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(store.display().to_string()))
            })?
            .collect::<std::result::Result<_, _>>()
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(store.display().to_string()))
            })?;
        entries.sort_by_key(|entry| entry.file_name());

        let mut quarantined = Vec::new();
        for entry in entries {
            if quarantined.len() == MALFORMED_RECORD_REPAIR_LIMIT
                || entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    != Some("json")
            {
                continue;
            }
            let path = entry.path();
            let Err(error) = read_record_path(&path) else {
                continue;
            };
            fs::create_dir_all(&quarantine).map_err(|error| {
                Error::internal_io(error.to_string(), Some(quarantine.display().to_string()))
            })?;
            let quarantined_at = chrono::Utc::now().to_rfc3339();
            let name = entry.file_name().to_string_lossy().to_string();
            let quarantined_path = quarantine.join(format!("{name}-{}.json", uuid::Uuid::new_v4()));
            let provenance_path = quarantined_path.with_extension("provenance.json");
            let record = TaskWorktreeRegistryQuarantine {
                record_path: path.display().to_string(),
                quarantine_path: quarantined_path.display().to_string(),
                provenance_path: provenance_path.display().to_string(),
                reason: error.message,
                quarantined_at,
            };
            let provenance = serde_json::to_string_pretty(&record).map_err(|error| {
                Error::internal_json(error.to_string(), Some(record.provenance_path.clone()))
            })?;
            crate::io::write_output_file_atomically(
                &provenance_path,
                format!("{provenance}\n"),
                crate::io::OutputWriteOptions::file(),
            )
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(record.provenance_path.clone()))
            })?;
            if let Err(error) = fs::rename(&path, &quarantined_path) {
                let _ = fs::remove_file(&provenance_path);
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(path.display().to_string()),
                ));
            }
            quarantined.push(record);
        }
        Ok(quarantined)
    })
}

/// Hold a shared lease over the task-worktree registry while a caller reads
/// liveness and mutates a worktree-local resource. Registry writers take the
/// matching exclusive lease before atomically publishing their next snapshot.
pub(crate) fn with_task_worktree_registry_read_lock<T>(
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _gate = TASK_WORKTREE_REGISTRY_GATE
        .get_or_init(|| RwLock::new(()))
        .read()
        .map_err(|_| Error::internal_unexpected("task worktree registry read gate poisoned"))?;
    let lock = open_task_worktree_registry_lock()?;
    lock.lock_shared().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock task worktree registry for read".to_string()),
        )
    })?;
    operation()
}

pub(super) fn with_task_worktree_registry_write_lock<T>(
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _gate = TASK_WORKTREE_REGISTRY_GATE
        .get_or_init(|| RwLock::new(()))
        .write()
        .map_err(|_| Error::internal_unexpected("task worktree registry write gate poisoned"))?;
    let lock = open_task_worktree_registry_lock()?;
    lock.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock task worktree registry for write".to_string()),
        )
    })?;
    operation()
}

fn open_task_worktree_registry_lock() -> Result<std::fs::File> {
    let store = metadata_dir()?;
    let parent = store.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "task worktree store has no parent: {}",
            store.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(parent.join("task-worktrees.lock"))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open task worktree registry lock".to_string()),
            )
        })
}

pub fn status(id: &str) -> Result<WorktreeStatusOutput> {
    status_with_store(id, &metadata_dir()?)
}

pub fn resolve(id: &str) -> Result<TaskWorktreeRecord> {
    read_record(&metadata_dir()?, id)
}

/// Returns `None` only when no Homeboy task-worktree record exists. Callers
/// that support an external provider can use this to avoid masking corrupt
/// Homeboy records with a provider fallback.
pub fn resolve_if_present(id: &str) -> Result<Option<TaskWorktreeRecord>> {
    let path = record_path(&metadata_dir()?, id);
    if !path.exists() {
        return Ok(None);
    }
    read_record_path(&path).map(Some)
}

pub fn resolve_workspace_ref(handle: &str) -> Result<WorkspaceRefRecord> {
    resolve_workspace_ref_if_present(handle)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_ref",
            "Workspace handle does not match a Homeboy task worktree or adopted workspace",
            Some(handle.to_string()),
            None,
        )
    })
}

/// Returns `None` only when neither Homeboy workspace registry contains the
/// handle. Corrupt records remain errors so callers never mask them with an
/// external provider fallback.
pub fn resolve_workspace_ref_if_present(handle: &str) -> Result<Option<WorkspaceRefRecord>> {
    let task_path = record_path(&metadata_dir()?, handle);
    if task_path.exists() {
        return read_record_path(&task_path)
            .map(WorkspaceRefRecord::Task)
            .map(Some);
    }

    let adopted_path = record_path(&adopted_metadata_dir()?, handle);
    if adopted_path.exists() {
        return read_adopted_record_path(&adopted_path)
            .map(WorkspaceRefRecord::Adopted)
            .map(Some);
    }

    Ok(None)
}

pub fn remove(options: WorktreeRemoveOptions) -> Result<WorktreeRemoveOutput> {
    remove_with_store(options, &metadata_dir()?)
}

pub fn cleanup(options: WorktreeCleanupOptions) -> Result<WorktreeCleanupOutput> {
    let store = metadata_dir()?;
    cleanup_with_store(options, &store)
}

/// Register an active task-worktree record against the current test home.
///
/// Other modules gate behavior on task-worktree liveness and need a registered
/// active checkout to exercise it. Keeping the helper here means the record
/// store layout stays owned by this module instead of being reimplemented by
/// every caller's test fixtures.
#[cfg(test)]
pub(crate) fn record_active_for_test(id: &str, worktree_path: &Path) {
    let record = TaskWorktreeRecord {
        id: id.to_string(),
        component_id: "fixture".to_string(),
        source_checkout: worktree_path.to_string_lossy().to_string(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        branch: format!("task/{id}"),
        base_ref: "main".to_string(),
        task_url: None,
        run_id: None,
        cleanup_policy: CleanupPolicy::RemoveWhenSafe,
        branch_cleanup_intent: BranchCleanupIntent::default(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        state: TaskWorktreeState::Active,
    };
    let store = metadata_dir().expect("task worktree store");
    write_record(&store, &record).expect("write task worktree record");
}

#[cfg(test)]
pub(crate) fn remove_record_for_test(id: &str) {
    let store = metadata_dir().expect("task worktree store");
    fs::remove_file(record_path(&store, id)).expect("remove task worktree record");
}

use store_ops::*;

pub fn queue_create(options: WorktreeQueueCreateOptions) -> Result<WorktreeQueueCreateOutput> {
    let mut rows = Vec::new();
    let total = options.branches.len();
    for (index, branch) in options.branches.iter().enumerate() {
        let command = worktree_create_command(&options, branch);
        let handle = worktree_handle(&options.repo, branch);

        if options.dry_run {
            rows.push(queue_row(
                branch,
                handle,
                command,
                WorktreeQueueCreateStatus::Queued,
            ));
            continue;
        }

        match create(WorktreeCreateOptions {
            component_id: options.repo.clone(),
            branch: branch.clone(),
            from: Some(options.from.clone()),
            task_url: options.task_url.clone(),
            run_id: None,
            cleanup_policy: None,
        }) {
            Ok(created) => {
                let mut row =
                    queue_row(branch, handle, command, WorktreeQueueCreateStatus::Created);
                row.path = Some(created.record.worktree_path);
                rows.push(row);
            }
            Err(error) => {
                let mut row = queue_row(branch, handle, command, WorktreeQueueCreateStatus::Failed);
                row.error = Some(error.message);
                rows.push(row);
                for queued_branch in options.branches.iter().take(total).skip(index + 1) {
                    rows.push(queue_row(
                        queued_branch,
                        worktree_handle(&options.repo, queued_branch),
                        worktree_create_command(&options, queued_branch),
                        WorktreeQueueCreateStatus::Queued,
                    ));
                }
                break;
            }
        }
    }

    Ok(WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: options.repo,
        base_ref: options.from,
        dry_run: options.dry_run,
        rows,
    })
}

use queue_ops::*;

#[cfg(test)]
mod tests;
