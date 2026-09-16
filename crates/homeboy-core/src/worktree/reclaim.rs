//! First-class workspace reclamation.
//!
//! Reclaim releases disk space held by completed task-worktree loops. It is
//! claim-aware: the workspace claim store refuses to let this (or any other)
//! reaper remove a workspace that an active run still holds through a live
//! owner lease. When a reclaim plan is applied, every removal goes through the
//! same fence-held `remove` path as every other reaping route, so it is
//! auditable like them, and every expired owner lease met along the way is
//! released durably and reported as part of the reclaim output.

use std::path::Path;

use super::reaping;
use super::store_ops::{remove_with_store_until, workspace_claim_store_for_worktrees};
use super::types::{
    TaskWorktreeRecord, TaskWorktreeState, WorktreeReclaimCandidate, WorktreeReclaimCounts,
    WorktreeReclaimOptions, WorktreeReclaimOutput, WorktreeReclaimRemoved, WorktreeReclaimSkipped,
    WorktreeRemoveOptions, WorktreeStaleLeaseRelease,
};
use crate::error::{Error, Result};

/// Classify one active record against the reclaim gates and claim store.
fn reclaim_candidate(
    record: &TaskWorktreeRecord,
    live_owners: Vec<String>,
) -> (WorktreeReclaimCandidate, Option<u64>) {
    let label = |live_owners: &[String]| live_owners.join(", ");
    if !live_owners.is_empty() {
        return (
            WorktreeReclaimCandidate {
                record: record.clone(),
                reclaimable: false,
                live_owners: live_owners.clone(),
                reclaimable_bytes: None,
                reasons: vec![format!(
                    "refuses to reclaim workspace claimed by {} durable live owner lease(s); holder session/run(s): {}",
                    live_owners.len(),
                    label(&live_owners)
                )],
            },
            None,
        );
    }
    if record.cleanup_policy == super::types::CleanupPolicy::PreserveOnFailure {
        return (
            WorktreeReclaimCandidate {
                record: record.clone(),
                reclaimable: false,
                live_owners,
                reclaimable_bytes: None,
                reasons: vec!["cleanup policy PreserveOnFailure is not reclaimable".to_string()],
            },
            None,
        );
    }
    if record.terminal_disposition.as_deref() != Some("succeeded") {
        let owner = record.run_id.clone();
        return (
            WorktreeReclaimCandidate {
                record: record.clone(),
                reclaimable: false,
                live_owners,
                reclaimable_bytes: None,
                reasons: vec![match owner {
                    Some(owner) => format!(
                        "reclaim requires explicit succeeded finalization from lifecycle owner `{owner}`"
                    ),
                    None => "reclaim requires explicit succeeded finalization".to_string(),
                }],
            },
            None,
        );
    }
    let worktree = Path::new(&record.worktree_path);
    if !worktree.exists() {
        return (
            WorktreeReclaimCandidate {
                record: record.clone(),
                reclaimable: false,
                live_owners,
                reclaimable_bytes: None,
                reasons: vec![format!(
                    "missing active worktree {} requires `worktree inventory --apply` reconciliation authority",
                    record.worktree_path
                )],
            },
            None,
        );
    }
    let reclaimable_bytes = crate::capacity::demand_for_tree(worktree)
        .ok()
        .map(|demand| demand.bytes);
    (
        WorktreeReclaimCandidate {
            record: record.clone(),
            reclaimable: live_owners.is_empty(),
            live_owners,
            reclaimable_bytes,
            reasons: vec!["completed loop with terminal succeeded finalization".to_string()],
        },
        reclaimable_bytes,
    )
}

pub(super) fn reclaim_with_store(
    options: WorktreeReclaimOptions,
    store_dir: &Path,
) -> Result<WorktreeReclaimOutput> {
    let claims = workspace_claim_store_for_worktrees(store_dir)?;
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    let mut removed = Vec::new();
    let mut released_stale_leases = Vec::new();
    let audit_path = reaping::reaping_audit_path(store_dir.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "task worktree store `{}` has no data root",
            store_dir.display()
        ))
    })?)
    .display()
    .to_string();

    let records: Vec<TaskWorktreeRecord> = super::list_with_store(store_dir)?
        .worktrees
        .into_iter()
        .filter(|record| record.state == TaskWorktreeState::Active)
        .take(options.limit.max(1))
        .collect();

    for record in records {
        let workspace = record.effective_workspace_identity()?;
        let live_owners = claims
            .owner_status(&workspace, now_ms())?
            .into_iter()
            .map(|owner| owner.owner_id)
            .collect();
        let (candidate, bytes) = reclaim_candidate(&record, live_owners);
        if !candidate.reclaimable {
            skipped.push(WorktreeReclaimSkipped {
                record: record.clone(),
                live_owners: candidate.live_owners.clone(),
                reclaimable_bytes: None,
                reasons: candidate.reasons.clone(),
            });
            candidates.push(candidate);
            continue;
        }
        candidates.push(candidate);
        if options.dry_run {
            continue;
        }
        // Reclaim is claim-aware end-to-end: the claim store re-refuses at the
        // fence if an owner registered between the plan and the apply.
        let remove = remove_with_store_until(
            WorktreeRemoveOptions {
                id: record.id.clone(),
                force: false,
                cleanup_branch: options.cleanup_branches,
                allow_unmerged_branch: options.allow_unmerged_branches,
                reason: Some("worktree reclaim".to_string()),
                reaper: None,
            },
            store_dir,
            None,
        )?;
        // The loop is complete and the workspace is gone: durably release the
        // workspace's remaining stale registrations (expired owner leases) and
        // report exactly which ones were released.
        if let Some(released) = claims.prune_expired_owner_leases(&workspace, now_ms())? {
            if !released.is_empty() {
                released_stale_leases.push(WorktreeStaleLeaseRelease {
                    workspace,
                    released_owners: released,
                });
            }
        }
        let reclaimed_bytes = bytes;
        removed.push(WorktreeReclaimRemoved {
            remove,
            reclaimed_bytes,
        });
    }

    let counts = WorktreeReclaimCounts {
        candidates: candidates.iter().filter(|c| c.reclaimable).count(),
        removed: removed.len(),
        skipped: skipped.len(),
        reclaimable_bytes: candidates
            .iter()
            .filter(|c| c.reclaimable)
            .filter_map(|c| c.reclaimable_bytes)
            .sum(),
        reclaimed_bytes: removed
            .iter()
            .filter_map(|applied| applied.reclaimed_bytes)
            .sum(),
        released_stale_leases: released_stale_leases
            .iter()
            .map(|release| release.released_owners.len())
            .sum(),
    };
    Ok(WorktreeReclaimOutput {
        dry_run: options.dry_run,
        counts,
        candidates,
        removed,
        skipped,
        released_stale_leases,
        audit_path,
    })
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}
