use std::fs;
use std::path::{Path, PathBuf};

use crate::component::{self, TargetSpec};
use crate::error::{Error, Result};
use crate::ownership;
use crate::{git, paths};

mod queue_ops;
mod store_ops;
mod types;

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
    list_with_store(&metadata_dir()?)
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
    if let Ok(record) = read_record(&metadata_dir()?, handle) {
        return Ok(WorkspaceRefRecord::Task(record));
    }
    read_adopted_record(&adopted_metadata_dir()?, handle).map(WorkspaceRefRecord::Adopted)
}

pub fn remove(options: WorktreeRemoveOptions) -> Result<WorktreeRemoveOutput> {
    remove_with_store(options, &metadata_dir()?)
}

pub fn cleanup(options: WorktreeCleanupOptions) -> Result<WorktreeCleanupOutput> {
    let store = metadata_dir()?;
    cleanup_with_store(options, &store)
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
