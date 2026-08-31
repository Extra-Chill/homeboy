use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use crate::component::{self, TargetSpec};
use crate::error::{Error, Result};
use crate::ownership;
use crate::{git, paths};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod queue_ops;
mod store_ops;
mod types;

static TASK_WORKTREE_REGISTRY_GATE: OnceLock<RwLock<()>> = OnceLock::new();
const MALFORMED_RECORD_REPAIR_LIMIT: usize = 20;

pub use crate::workspace_claim::WorkspaceIdentity;
pub use types::{
    authority_set_fingerprint, task_worktree_workspace_identity, AdoptedWorkspaceInventoryRecord,
    AdoptedWorkspaceRecord, BranchCleanupIntent, BranchCleanupStatus, CleanupPolicy,
    MissingActiveWorktree, MissingActiveWorktreeReason, TaskWorktreeRecord, TaskWorktreeState,
    TerminalWorkspaceAuthorityObservation, TerminalWorkspaceAuthorityProof, WorkspaceRefRecord,
    WorktreeAdoptOptions, WorktreeAdoptOutput, WorktreeAdoptedInventoryPage,
    WorktreeBranchCleanupReport, WorktreeCleanupCandidate, WorktreeCleanupCounts,
    WorktreeCleanupOptions, WorktreeCleanupOutput, WorktreeCleanupSkipped, WorktreeCreateAction,
    WorktreeCreateEvidence, WorktreeCreateOptions, WorktreeCreateOutput,
    WorktreeCreateReconciliation, WorktreeHandoffFreshness, WorktreeHandoffFreshnessProof,
    WorktreeImportOptions, WorktreeImportOutput, WorktreeInventoryApplyRefusal,
    WorktreeInventoryAuthorization, WorktreeInventoryCrossTab, WorktreeInventoryLocalEvidence,
    WorktreeInventoryOptions, WorktreeInventoryOutput, WorktreeInventoryRecord,
    WorktreeLeaseActivity, WorktreeListOutput, WorktreeLivenessAuthority, WorktreeOwnershipProbe,
    WorktreeQueueCreateFailure, WorktreeQueueCreateOptions, WorktreeQueueCreateOutput,
    WorktreeQueueCreateRequest, WorktreeQueueCreateRow, WorktreeQueueCreateStatus,
    WorktreeQueueLockHolder, WorktreeReconciliationAction, WorktreeReconciliationAuthority,
    WorktreeReconciliationResult, WorktreeRemoveOptions, WorktreeRemoveOutput,
    WorktreeSafetyReport, WorktreeStatusOutput, TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY,
    TERMINAL_WORKSPACE_AUTHORITY_SCHEMA,
};

/// The managed handle a repo and branch pair resolves to. Creation slugifies the
/// branch, so `fix/1234-x` becomes `repo@fix-1234-x` and the handle a caller must
/// pass is not the branch it typed. Callers that report a missing destination use
/// this to name the handle they actually looked for instead of leaving the reader
/// to guess the slug rule.
pub fn handle_for_branch(repo: &str, branch: &str) -> String {
    queue_ops::worktree_handle(repo, branch)
}

pub fn create(options: WorktreeCreateOptions) -> Result<WorktreeCreateOutput> {
    create_with_store(options, &metadata_dir()?)
}

pub fn adopt(options: WorktreeAdoptOptions) -> Result<WorktreeAdoptOutput> {
    adopt_with_store(options, &adopted_metadata_dir()?)
}

pub fn import(options: WorktreeImportOptions) -> Result<WorktreeImportOutput> {
    import_with_store(options, &metadata_dir()?)
}

pub fn list() -> Result<WorktreeListOutput> {
    with_task_worktree_registry_read_lock(list_unlocked)
}

/// Report the live write holder for a checkout path, including component
/// subdirectories. Unmanaged paths return `None` rather than failing closed.
pub fn ownership_probe(path: &Path) -> Result<Option<WorktreeOwnershipProbe>> {
    let data_root = paths::homeboy_data()?;
    ownership_probe_in_root(path, &data_root, now_ms())
}

/// Refuse a write when a different live owner holds the managed checkout.
/// `force` is an explicit operator override, not an authority receipt.
pub fn enforce_write_ownership(
    path: &Path,
    caller_owner_id: Option<&str>,
    force: bool,
) -> Result<Option<WorktreeOwnershipProbe>> {
    let probe = ownership_probe(path)?;
    if force {
        return Ok(probe);
    }
    let Some(status) = probe.as_ref() else {
        return Ok(None);
    };
    let foreign = status
        .live_holders
        .iter()
        .filter(|holder| Some(holder.as_str()) != caller_owner_id)
        .cloned()
        .collect::<Vec<_>>();
    if !foreign.is_empty() {
        return Err(Error::validation_invalid_argument(
            "worktree_write_ownership",
            format!(
                "refusing to write managed worktree {}: held by live session(s) {}; pass --force to override",
                status.handle,
                foreign.join(", ")
            ),
            Some(status.path.clone()),
            Some(vec!["override: --force".to_string()]),
        ));
    }
    Ok(probe)
}

fn ownership_probe_in_root(
    path: &Path,
    data_root: &Path,
    now_ms: u64,
) -> Result<Option<WorktreeOwnershipProbe>> {
    let requested = normalize_existing_path(path);
    let record = list_with_store(&metadata_dir_in_root(data_root))?
        .worktrees
        .into_iter()
        .filter(|record| {
            let root = normalize_existing_path(Path::new(&record.worktree_path));
            requested == root || requested.starts_with(&root)
        })
        .max_by_key(|record| Path::new(&record.worktree_path).components().count());
    let Some(record) = record else {
        return Ok(None);
    };
    let workspace = record.effective_workspace_identity()?;
    let owners = crate::workspace_claim::WorkspaceClaimStore::new(
        data_root.join(crate::workspace_claim::LOCAL_WORKSPACE_CLAIMS_DIR),
    )
    .owner_status(&workspace, now_ms)?;
    let live_holders = owners
        .iter()
        .map(|owner| owner.owner_id.clone())
        .collect::<Vec<_>>();
    let live_holder = record
        .run_id
        .as_ref()
        .filter(|run_id| live_holders.contains(run_id))
        .cloned()
        .or_else(|| live_holders.first().cloned());
    let holder = live_holder.or_else(|| record.run_id.clone());
    let stopped =
        record.state == TaskWorktreeState::Removed || record.terminal_disposition.is_some();
    let activity = if stopped {
        WorktreeLeaseActivity::Stopped
    } else if live_holders.is_empty() {
        WorktreeLeaseActivity::Stale
    } else {
        WorktreeLeaseActivity::Live
    };
    let lease_expires_at_ms = holder.as_ref().and_then(|holder| {
        owners
            .iter()
            .find(|owner| &owner.owner_id == holder)
            .map(|owner| owner.expires_at_ms)
    });
    Ok(Some(WorktreeOwnershipProbe {
        handle: record.id,
        path: record.worktree_path,
        holder,
        lifecycle_state: record
            .terminal_disposition
            .unwrap_or_else(|| match record.state {
                TaskWorktreeState::Active => "active".to_string(),
                TaskWorktreeState::Removed => "removed".to_string(),
            }),
        heartbeat_fresh: !owners.is_empty(),
        activity,
        lease_expires_at_ms,
        live_holders,
    }))
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

pub(crate) fn list_workspace_refs() -> Result<Vec<WorkspaceRefRecord>> {
    let mut records = list()?
        .worktrees
        .into_iter()
        .map(WorkspaceRefRecord::Task)
        .collect::<Vec<_>>();
    records.extend(
        list_adopted_with_store(&adopted_metadata_dir()?)?
            .into_iter()
            .map(WorkspaceRefRecord::Adopted),
    );
    records.sort_by(|left, right| left.handle().cmp(right.handle()));
    Ok(records)
}

pub fn inventory(
    options: WorktreeInventoryOptions,
    authority: &dyn WorktreeReconciliationAuthority,
) -> Result<WorktreeInventoryOutput> {
    inventory_with_store_and_authority(
        options,
        &metadata_dir()?,
        &adopted_metadata_dir()?,
        authority,
    )
}

/// Cache exact terminal authority evidence before a reconciliation claim is
/// acquired. The record revision is intentionally unchanged: the receipt binds
/// the revision that was observed and is checked again under the claim/registry
/// lease before mutation.
pub fn persist_terminal_workspace_authority(
    id: &str,
    expected_revision: u64,
    proof: TerminalWorkspaceAuthorityProof,
) -> Result<()> {
    with_task_worktree_registry_write_lock(|| {
        let store = metadata_dir()?;
        let mut record = read_record(&store, id)?;
        if record.lifecycle_revision != expected_revision
            || !proof.exact_for(&record, record.run_id.as_deref())
        {
            return Err(Error::validation_invalid_argument(
                "terminal_workspace_authority",
                "terminal workspace authority proof does not exactly bind the current manifest",
                Some(id.to_string()),
                None,
            ));
        }
        match &record.terminal_workspace_authority {
            Some(existing) if existing != &proof => Err(Error::validation_invalid_argument(
                "terminal_workspace_authority",
                "terminal workspace authority proof conflicts with immutable manifest evidence",
                Some(id.to_string()),
                None,
            )),
            Some(_) => Ok(()),
            None => {
                record.terminal_workspace_authority = Some(proof);
                store_ops::write_record_unlocked(&store, &record)
            }
        }
    })
}

/// Bind one terminal outcome to the native lifecycle owner. Cleanup remains a
/// separate authority-gated operation; finalization only makes successful runs
/// eligible and durably preserves every non-successful workspace.
pub fn finalize_provider_lifecycle(
    id: &str,
    owner_run_ref: &str,
    disposition: crate::worktree_provider::WorktreeTerminalDisposition,
) -> Result<TaskWorktreeRecord> {
    with_task_worktree_registry_write_lock(|| {
        let store = metadata_dir()?;
        let mut record = read_record(&store, id)?;
        if record.run_id.as_deref() != Some(owner_run_ref) {
            return Err(Error::validation_invalid_argument(
                "owner_run_ref",
                "native worktree lifecycle owner does not match the finalization request",
                Some(owner_run_ref.to_string()),
                None,
            ));
        }
        if let Some(existing) = record.terminal_disposition.as_deref() {
            if existing != disposition.as_str() {
                return Err(Error::validation_invalid_argument(
                    "terminal_disposition",
                    "native worktree lifecycle already finalized with a different disposition",
                    Some(existing.to_string()),
                    None,
                ));
            }
            return Ok(record);
        }
        record.cleanup_policy =
            if disposition == crate::worktree_provider::WorktreeTerminalDisposition::Succeeded {
                CleanupPolicy::RemoveWhenSafe
            } else {
                CleanupPolicy::PreserveOnFailure
            };
        record.terminal_disposition = Some(disposition.as_str().to_string());
        record.lifecycle_revision = record.lifecycle_revision.checked_add(1).ok_or_else(|| {
            Error::validation_invalid_argument(
                "lifecycle_revision",
                "task worktree lifecycle revision overflowed during finalization",
                Some(id.to_string()),
                None,
            )
        })?;
        record.terminal_workspace_authority = None;
        store_ops::write_record_unlocked(&store, &record)?;
        Ok(record)
    })
}

pub(crate) fn list_unlocked() -> Result<WorktreeListOutput> {
    list_with_store(&metadata_dir()?)
}

/// Evidence retained when a malformed task-worktree record is removed from the
/// active registry. The original bytes and this provenance sidecar remain under
/// `.quarantine`, so uncertain activity is never silently discarded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskWorktreeRegistryQuarantine {
    pub record_path: String,
    pub quarantine_path: String,
    pub provenance_path: String,
    pub reason: String,
    #[serde(default)]
    pub planned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_at: Option<String>,
    pub quarantined_at: String,
}

/// Plan or apply bounded malformed-record quarantine. Every returned record
/// remains active protection until `clear_task_worktree_registry_quarantine`
/// records an explicit verified terminal reconciliation.
pub(crate) fn reconcile_malformed_task_worktree_records(
    apply: bool,
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

        let mut quarantined = active_task_worktree_registry_quarantines(&quarantine)?;
        let mut repaired_count = 0;
        for entry in entries {
            if repaired_count == MALFORMED_RECORD_REPAIR_LIMIT
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
            let quarantined_at = chrono::Utc::now().to_rfc3339();
            let name = entry.file_name().to_string_lossy().to_string();
            let quarantined_path = quarantine.join(format!("{name}-{}.json", uuid::Uuid::new_v4()));
            let provenance_path = quarantined_path.with_extension("provenance.json");
            let mut record = TaskWorktreeRegistryQuarantine {
                record_path: path.display().to_string(),
                quarantine_path: quarantined_path.display().to_string(),
                provenance_path: provenance_path.display().to_string(),
                reason: error.message,
                planned: !apply,
                cleared_at: None,
                quarantined_at,
            };
            if !apply {
                quarantined.push(record);
                repaired_count += 1;
                continue;
            }
            fs::create_dir_all(&quarantine).map_err(|error| {
                Error::internal_io(error.to_string(), Some(quarantine.display().to_string()))
            })?;
            record.planned = false;
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
            repaired_count += 1;
        }
        Ok(quarantined)
    })
}

/// Mark one retained quarantine as terminally reconciled without deleting its
/// original bytes. The caller supplies the explicit verified-terminal decision.
pub fn clear_task_worktree_registry_quarantine(
    provenance_path: &Path,
    verified_terminal: bool,
) -> Result<TaskWorktreeRegistryQuarantine> {
    if !verified_terminal {
        return Err(Error::validation_invalid_argument(
            "verified_terminal",
            "clearing quarantined worktree evidence requires verified terminal reconciliation",
            Some(provenance_path.display().to_string()),
            None,
        ));
    }
    with_task_worktree_registry_write_lock(|| {
        let quarantine = metadata_dir()?.join(".quarantine");
        if !provenance_path.starts_with(&quarantine) {
            return Err(Error::validation_invalid_argument(
                "provenance_path",
                "quarantine provenance path is outside the task-worktree registry",
                Some(provenance_path.display().to_string()),
                None,
            ));
        }
        let raw = fs::read_to_string(provenance_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(provenance_path.display().to_string()),
            )
        })?;
        let mut record: TaskWorktreeRegistryQuarantine =
            serde_json::from_str(&raw).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some(provenance_path.display().to_string()),
                )
            })?;
        if !Path::new(&record.quarantine_path).exists() {
            return Err(Error::validation_invalid_argument(
                "provenance_path",
                "quarantined worktree evidence is missing",
                Some(record.quarantine_path.clone()),
                None,
            ));
        }
        record.planned = false;
        record.cleared_at = Some(chrono::Utc::now().to_rfc3339());
        let raw = serde_json::to_string_pretty(&record).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(provenance_path.display().to_string()),
            )
        })?;
        crate::io::write_output_file_atomically(
            provenance_path,
            format!("{raw}\n"),
            crate::io::OutputWriteOptions::file(),
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(provenance_path.display().to_string()),
            )
        })?;
        Ok(record)
    })
}

pub fn list_task_worktree_registry_quarantines() -> Result<Vec<TaskWorktreeRegistryQuarantine>> {
    with_task_worktree_registry_read_lock(|| {
        active_task_worktree_registry_quarantines(&metadata_dir()?.join(".quarantine"))
    })
}

fn active_task_worktree_registry_quarantines(
    quarantine: &Path,
) -> Result<Vec<TaskWorktreeRegistryQuarantine>> {
    if !quarantine.exists() {
        return Ok(Vec::new());
    }
    let mut quarantines = Vec::new();
    for entry in fs::read_dir(quarantine).map_err(|error| {
        Error::internal_io(error.to_string(), Some(quarantine.display().to_string()))
    })? {
        let path = entry
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(quarantine.display().to_string()))
            })?
            .path();
        if !path.to_string_lossy().ends_with(".provenance.json") {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        let record: TaskWorktreeRegistryQuarantine =
            serde_json::from_str(&raw).map_err(|error| {
                Error::internal_json(error.to_string(), Some(path.display().to_string()))
            })?;
        if record.cleared_at.is_none() {
            quarantines.push(record);
        }
    }
    quarantines.sort_by(|left, right| left.provenance_path.cmp(&right.provenance_path));
    Ok(quarantines)
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
    let store = metadata_dir()?;
    let parent = store.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "task worktree store has no parent: {}",
            store.display()
        ))
    })?;
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(parent.join("task-worktrees.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return operation(),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some("open task worktree registry lock for read".to_string()),
            ));
        }
    };
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
        .truncate(false)
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
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn record_active_for_test(id: &str, worktree_path: &Path) {
    record_for_test(id, worktree_path, worktree_path, TaskWorktreeState::Active);
}

#[cfg(test)]
pub(crate) fn record_active_with_source_for_test(
    id: &str,
    source_checkout: &Path,
    worktree_path: &Path,
) {
    record_for_test(
        id,
        source_checkout,
        worktree_path,
        TaskWorktreeState::Active,
    );
}

#[cfg(test)]
pub(crate) fn record_removed_for_test(id: &str, worktree_path: &Path) {
    record_for_test(id, worktree_path, worktree_path, TaskWorktreeState::Removed);
}

#[cfg(any(test, feature = "test-support"))]
fn record_for_test(
    id: &str,
    source_checkout: &Path,
    worktree_path: &Path,
    state: TaskWorktreeState,
) {
    let record = TaskWorktreeRecord {
        id: id.to_string(),
        component_id: "fixture".to_string(),
        source_checkout: source_checkout.to_string_lossy().to_string(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        branch: format!("task/{id}"),
        base_ref: "main".to_string(),
        task_url: None,
        run_id: None,
        cleanup_policy: CleanupPolicy::RemoveWhenSafe,
        terminal_disposition: None,
        branch_cleanup_intent: BranchCleanupIntent::default(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        state,
        workspace_identity: Some(
            WorkspaceIdentity::new("task-worktree", format!("fixture/{id}"))
                .expect("test workspace identity"),
        ),
        lifecycle_revision: 0,
        terminal_workspace_authority: None,
    };
    let store = metadata_dir().expect("task worktree store");
    write_record(&store, &record).expect("write task worktree record");
}

pub(crate) fn safety_report_for_provider(
    record: &TaskWorktreeRecord,
) -> Result<WorktreeSafetyReport> {
    safety_report(record)
}

#[cfg(test)]
pub(crate) fn remove_record_for_test(id: &str) {
    let store = metadata_dir().expect("task worktree store");
    fs::remove_file(record_path(&store, id)).expect("remove task worktree record");
}

use store_ops::*;

pub fn queue_create(options: WorktreeQueueCreateOptions) -> Result<WorktreeQueueCreateOutput> {
    let mut rows = Vec::new();
    let total = options.requests.len();
    for (index, request) in options.requests.iter().enumerate() {
        let command = worktree_create_command(&options, request);
        let handle = worktree_handle(&options.repo, &request.branch);

        if options.dry_run {
            match planned_create_path(&options.repo, &request.branch, &options.from) {
                Ok(path) => {
                    let mut row = queue_row(
                        &request.branch,
                        handle,
                        command,
                        WorktreeQueueCreateStatus::WouldCreate,
                    );
                    row.path = Some(path);
                    rows.push(row);
                }
                Err(error) => {
                    let mut row = queue_row(
                        &request.branch,
                        handle,
                        command,
                        WorktreeQueueCreateStatus::Failed,
                    );
                    row.failure = Some(queue_failure(&error));
                    row.error = Some(error.message);
                    rows.push(row);
                }
            }
            continue;
        }

        let created = if let Some(lifecycle) = &request.provider_lifecycle {
            let task_url = request.task_url.clone().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "task_url",
                    "provider-owned queue worktree requires task_url",
                    Some(handle.clone()),
                    None,
                )
            })?;
            crate::worktree_provider::ensure_worktree_provision_from_config(
                &crate::worktree_provider::WorktreeProvisionIntent {
                    handle: handle.clone(),
                    repo: options.repo.clone(),
                    base: options.from.clone(),
                    head: request.branch.clone(),
                    task_url: Some(task_url),
                },
                lifecycle,
                None,
                &crate::defaults::load_config(),
            )
            .map(|provision| provision.destination.ownership.path)
        } else {
            create(WorktreeCreateOptions {
                component_id: options.repo.clone(),
                branch: request.branch.clone(),
                from: Some(options.from.clone()),
                task_url: request.task_url.clone(),
                run_id: request.run_id.clone(),
                cleanup_policy: None,
                require_handoff_freshness: false,
            })
            .map(|created| created.record.worktree_path)
        };
        match created {
            Ok(path) => {
                let mut row = queue_row(
                    &request.branch,
                    handle,
                    command,
                    WorktreeQueueCreateStatus::Created,
                );
                row.path = Some(path);
                rows.push(row);
            }
            Err(error) => {
                let mut row = queue_row(
                    &request.branch,
                    handle,
                    command,
                    WorktreeQueueCreateStatus::Failed,
                );
                row.failure = Some(queue_failure(&error));
                row.error = Some(error.message);
                rows.push(row);
                for queued_request in options.requests.iter().take(total).skip(index + 1) {
                    rows.push(queue_row(
                        &queued_request.branch,
                        worktree_handle(&options.repo, &queued_request.branch),
                        worktree_create_command(&options, queued_request),
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

fn queue_failure(error: &Error) -> WorktreeQueueCreateFailure {
    let provider_id = error
        .details
        .get("worktree_provider_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let classification = error
        .details
        .get("worktree_provider_call_classification")
        .and_then(Value::as_str)
        .unwrap_or_else(|| error.code.as_str())
        .to_string();
    let phase = error
        .details
        .get("worktree_provider_phase")
        .and_then(Value::as_str)
        .unwrap_or("worktree_preflight")
        .to_string();
    WorktreeQueueCreateFailure {
        code: error.code.as_str().to_string(),
        classification,
        phase,
        message: error.message.clone(),
        provider_id,
        details: error.details.clone(),
    }
}

/// Validate a prospective native worktree creation and return the exact path
/// that `create` will use, without creating a branch, checkout, or record.
pub fn planned_create_path(repo: &str, branch: &str, from: &str) -> Result<String> {
    let target = component::resolve_target(TargetSpec {
        component_id: Some(repo),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: false,
        accept_bare_directory: false,
        ..TargetSpec::default()
    })?;
    let source_checkout = queue_ops::source_checkout_for_worktree(&target)?;
    git::run_git(
        &source_checkout,
        &["rev-parse", "--verify", &format!("{from}^{{commit}}")],
        "git rev-parse --verify",
    )?;
    let parent = source_checkout.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source_checkout.display()
        ))
    })?;
    let path = parent.join(handle_for_branch(&target.component_id, branch));
    if path.exists() {
        return Err(Error::validation_invalid_argument(
            "branch",
            "Task worktree path already exists",
            Some(path.display().to_string()),
            Some(vec![
                "Use a unique branch name or remove the existing task worktree".to_string(),
            ]),
        ));
    }
    Ok(path.display().to_string())
}

use queue_ops::*;

#[cfg(test)]
mod tests;
