//! Durable reaping audit for native task worktrees.
//!
//! Every path that removes a registered task worktree records one append-only
//! audit line under the data root, answering "what was removed, by which
//! session/run, and why". The audit is written inside the registry-managed
//! removal fence, immediately after the removal itself was durably published,
//! so it can never describe a removal that did not happen.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::types::{TaskWorktreeReapingAudit, TaskWorktreeRecord};
use crate::error::{Error, Result};

pub const TASK_WORKTREE_REAPING_AUDIT_SCHEMA: &str = "homeboy/task-worktree-reaping/v1";
pub const TASK_WORKTREE_REAPING_AUDIT_FILE: &str = "task-worktree-reaping.jsonl";

pub fn reaping_audit_path(data_root: &Path) -> PathBuf {
    data_root.join(TASK_WORKTREE_REAPING_AUDIT_FILE)
}

pub(super) fn append_reaping_audit_in_root(
    data_root: &Path,
    record: &TaskWorktreeRecord,
    at_ms: u64,
    reason: Option<&str>,
    reaper: Option<&str>,
    force: bool,
    branch_deleted: bool,
) -> Result<TaskWorktreeReapingAudit> {
    let audit = TaskWorktreeReapingAudit {
        schema: TASK_WORKTREE_REAPING_AUDIT_SCHEMA.into(),
        at_ms,
        record_id: record.id.clone(),
        component_id: record.component_id.clone(),
        worktree_path: record.worktree_path.clone(),
        branch: record.branch.clone(),
        run_id: record.run_id.clone(),
        lifecycle_revision: record.lifecycle_revision,
        force,
        reason: reason.map(str::to_string),
        reaper: reaper.map(str::to_string),
        removed: true,
        branch_deleted,
    };
    let path = reaping_audit_path(data_root);
    fs_append_line(&path, &audit)?;
    Ok(audit)
}

pub fn read_task_worktree_reaping_audit_in_root(
    data_root: &Path,
) -> Result<Vec<TaskWorktreeReapingAudit>> {
    let path = reaping_audit_path(data_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let mut records = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(line).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("{} line {}", path.display(), index + 1)),
            )
        })?);
    }
    Ok(records)
}

fn fs_append_line(path: &Path, audit: &TaskWorktreeReapingAudit) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::to_writer(&mut file, audit).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    writeln!(file)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    file.sync_all()
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    Ok(())
}
