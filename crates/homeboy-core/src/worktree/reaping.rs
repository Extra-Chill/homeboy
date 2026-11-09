//! Durable reaping audit for native task worktrees.
//!
//! Every path that removes a registered task worktree appends one row to the
//! observation store's coordination audit (#15947), answering "what was
//! removed, by which session/run, and why". The audit is written inside the
//! registry-managed removal fence, immediately after the removal itself was
//! durably published, so it can never describe a removal that did not happen.

use std::path::Path;

use super::types::{TaskWorktreeReapingAudit, TaskWorktreeRecord};
use crate::error::{Error, Result};
use crate::observation::coordination::MAX_COORDINATION_AUDIT_READ;
use crate::observation::ObservationStore;

pub const TASK_WORKTREE_REAPING_AUDIT_SCHEMA: &str = "homeboy/task-worktree-reaping/v1";

/// Coordination namespace owned by the task-worktree reaping audit.
pub const TASK_WORKTREE_REAPING_AUDIT_NAMESPACE: &str = "task-worktree-reaping";

const EVENT_REMOVED: &str = "removed";

pub(super) fn append_reaping_audit_in_root(
    data_root: &Path,
    record: &TaskWorktreeRecord,
    at_ms: u64,
    reason: Option<&str>,
    reaper: Option<&str>,
    force: bool,
    branch_deleted: bool,
) -> Result<TaskWorktreeReapingAudit> {
    // Candidate refs belong to the Cook, not to the lifetime of its staged
    // index. Drop the pin only when its owned destination is actually reaped.
    let store = open_store(data_root)?;
    if let Some(run_id) = record.run_id.as_deref() {
        if let Some(run) = store.get_run(run_id)? {
            if let Some(cook_id) = run
                .metadata_json
                .pointer("/agent_task_run/metadata/cook_id")
                .and_then(serde_json::Value::as_str)
            {
                let reference = format!("refs/homeboy/candidates/{cook_id}");
                let output = std::process::Command::new("git")
                    .args([
                        "-C",
                        &record.source_checkout,
                        "update-ref",
                        "-d",
                        &reference,
                    ])
                    .output()
                    .map_err(|error| Error::internal_io(error.to_string(), None))?;
                if !output.status.success() {
                    return Err(Error::git_command_failed(
                        String::from_utf8_lossy(&output.stderr).to_string(),
                    ));
                }
            }
        }
    }
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
    let payload = serde_json::to_value(&audit).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("task worktree reaping audit".to_string()),
        )
    })?;
    let subject = if audit.worktree_path.trim().is_empty() {
        audit.record_id.as_str()
    } else {
        audit.worktree_path.as_str()
    };
    open_store(data_root)?.append_coordination_audit(
        TASK_WORKTREE_REAPING_AUDIT_NAMESPACE,
        subject,
        EVENT_REMOVED,
        &payload,
    )?;
    Ok(audit)
}

/// Every retained reaping audit under `data_root`, oldest first. The read is
/// bounded by [`MAX_COORDINATION_AUDIT_READ`] newest entries.
pub fn read_task_worktree_reaping_audit_in_root(
    data_root: &Path,
) -> Result<Vec<TaskWorktreeReapingAudit>> {
    let entries = open_store(data_root)?.coordination_audit_in_namespace(
        TASK_WORKTREE_REAPING_AUDIT_NAMESPACE,
        MAX_COORDINATION_AUDIT_READ,
    )?;
    entries
        .into_iter()
        .rev()
        .map(|entry| {
            serde_json::from_value(entry.payload).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some(format!(
                        "{} audit sequence {}",
                        TASK_WORKTREE_REAPING_AUDIT_NAMESPACE, entry.sequence
                    )),
                )
            })
        })
        .collect()
}

fn open_store(data_root: &Path) -> Result<ObservationStore> {
    ObservationStore::open_initialized_for_lifecycle_at(crate::paths::observation_db_in_root(
        data_root,
    ))
}
