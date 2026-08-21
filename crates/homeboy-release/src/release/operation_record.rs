//! Durable, provider-agnostic records for operations that cross process boundaries.
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_core::{paths, Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationRecord {
    pub owner_run_ref: String,
    pub operation: String,
    pub subject: String,
    pub provider: String,
    pub handle: String,
    pub path: Option<String>,
    pub source_sha: String,
    pub cleanup_policy: String,
    pub lifecycle_state: String,
    pub terminal_disposition: Option<String>,
    pub finalization_status: String,
    #[serde(default)]
    pub finalization_lease: Option<String>,
    #[serde(default)]
    pub finalization_lease_started_ms: Option<u128>,
    pub attempt_count: u32,
    pub continuation_evidence: Vec<String>,
    #[serde(default)]
    pub attributes: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizationClaim {
    Claimed {
        lease: String,
        record: OperationRecord,
    },
    AlreadyCompleted(OperationRecord),
    InProgress(OperationRecord),
}

impl OperationRecord {
    pub fn finalization_pending(&self) -> bool {
        self.finalization_status != "completed"
    }
}

/// Durable operation records below one explicitly injected data root.
///
/// Stateful on purpose. A single release workspace lifecycle writes this store
/// roughly a dozen times — publish ownership, record the provisioned path,
/// mark a failed verification, claim finalization, complete it — and the
/// ambient form resolved a data root independently for every one of them.
/// Binding the root once at construction is what makes those writes provably
/// the same home's, rather than incidentally so (#7505).
pub struct OperationRecordStore {
    data_root: PathBuf,
}

impl OperationRecordStore {
    /// Bind the store to the data root its caller already resolved.
    pub fn in_roots(roots: &paths::PathRoots) -> Self {
        Self {
            data_root: roots.data().to_path_buf(),
        }
    }

    pub fn create(&self, record: &OperationRecord) -> Result<OperationRecord> {
        self.update(&record.owner_run_ref, |_| Ok(record.clone()))
    }

    /// Compare/update is serialized across processes and atomically replaces the
    /// record, so a recovered finalizer cannot race a still-running owner.
    ///
    /// The lock and the record path derive from one root. The ambient form
    /// resolved the data root twice — once inside `lock()` and again inside
    /// `record_path()` — so a repoint between them could have serialized
    /// against one home's `.lock` while replacing the other home's record.
    pub fn update(
        &self,
        owner_run_ref: &str,
        update: impl FnOnce(Option<OperationRecord>) -> Result<OperationRecord>,
    ) -> Result<OperationRecord> {
        let data_root = self.data_root.as_path();
        let _lock = lock_in_roots(data_root)?;
        let path = record_path_in_roots(data_root, owner_run_ref);
        let current = read_path(&path)?;
        let next = update(current)?;
        if next.owner_run_ref != owner_run_ref {
            return Err(Error::validation_invalid_argument(
                "owner_run_ref",
                "operation record owner reference cannot change",
                Some(owner_run_ref.to_string()),
                None,
            ));
        }
        let json = serde_json::to_vec_pretty(&next).map_err(|error| {
            Error::internal_json(error.to_string(), Some(owner_run_ref.to_string()))
        })?;
        homeboy_core::io::write_output_file_atomically(
            &path,
            [json, b"\n".to_vec()].concat(),
            homeboy_core::io::OutputWriteOptions::artifact(),
        )
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
        Ok(next)
    }

    pub fn load(&self, owner_run_ref: &str) -> Result<Option<OperationRecord>> {
        let data_root = self.data_root.as_path();
        let _lock = lock_in_roots(data_root)?;
        read_path(&record_path_in_roots(data_root, owner_run_ref))
    }

    /// Claims finalization before an external provider call. A live lease is
    /// never stolen; only a bounded stale lease can be retried.
    pub fn claim_finalization(&self, owner_run_ref: &str) -> Result<FinalizationClaim> {
        const STALE_LEASE_MS: u128 = 5 * 60 * 1000;
        let now = now_ms();
        let candidate_lease = uuid::Uuid::new_v4().to_string();
        self.update(owner_run_ref, |record| {
            let mut record = record.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "owner_run_ref",
                    "operation record does not exist",
                    Some(owner_run_ref.to_string()),
                    None,
                )
            })?;
            if record.finalization_status == "completed" {
                return Ok(record);
            }
            if record.finalization_status == "finalizing"
                && record
                    .finalization_lease_started_ms
                    .is_some_and(|started| now.saturating_sub(started) < STALE_LEASE_MS)
            {
                return Ok(record);
            }
            record.finalization_status = "finalizing".to_string();
            record.lifecycle_state = "finalizing".to_string();
            record.attempt_count += 1;
            record.finalization_lease = Some(candidate_lease.clone());
            record.finalization_lease_started_ms = Some(now);
            Ok(record)
        })
        .map(|record| {
            if record.finalization_status == "completed" {
                FinalizationClaim::AlreadyCompleted(record)
            } else if let Some(lease) = record.finalization_lease.clone() {
                if lease == candidate_lease {
                    FinalizationClaim::Claimed { lease, record }
                } else {
                    FinalizationClaim::InProgress(record)
                }
            } else {
                FinalizationClaim::InProgress(record)
            }
        })
    }

    pub fn complete_finalization(
        &self,
        owner_run_ref: &str,
        lease: &str,
    ) -> Result<OperationRecord> {
        self.update(owner_run_ref, |record| {
            let mut record = record.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "owner_run_ref",
                    "operation record does not exist",
                    Some(owner_run_ref.to_string()),
                    None,
                )
            })?;
            if record.finalization_lease.as_deref() != Some(lease) {
                return Ok(record);
            }
            record.finalization_status = "completed".to_string();
            record.lifecycle_state = "finalized".to_string();
            record.finalization_lease = None;
            record.finalization_lease_started_ms = None;
            record
                .continuation_evidence
                .push("provider finalization completed".to_string());
            Ok(record)
        })
    }

    pub fn fail_finalization(
        &self,
        owner_run_ref: &str,
        lease: &str,
        error: String,
    ) -> Result<OperationRecord> {
        self.update(owner_run_ref, |record| {
            let mut record = record.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "owner_run_ref",
                    "operation record does not exist",
                    Some(owner_run_ref.to_string()),
                    None,
                )
            })?;
            if record.finalization_lease.as_deref() != Some(lease) {
                return Ok(record);
            }
            record.finalization_status = "failed".to_string();
            record.lifecycle_state = "finalization_pending".to_string();
            record.finalization_lease = None;
            record.finalization_lease_started_ms = None;
            record.continuation_evidence.push(error);
            Ok(record)
        })
    }

    pub fn pending_for_subject(
        &self,
        operation: &str,
        subject: &str,
    ) -> Result<Vec<OperationRecord>> {
        self.for_subject(operation, subject, true)
    }

    pub fn for_subject(
        &self,
        operation: &str,
        subject: &str,
        pending_only: bool,
    ) -> Result<Vec<OperationRecord>> {
        let data_root = self.data_root.as_path();
        let _lock = lock_in_roots(data_root)?;
        let dir = store_dir_in_roots(data_root);
        let mut records = Vec::new();
        if !dir.exists() {
            return Ok(records);
        }
        for entry in fs::read_dir(&dir).map_err(|error| {
            Error::internal_io(error.to_string(), Some(dir.display().to_string()))
        })? {
            let entry = entry.map_err(|error| {
                Error::internal_io(error.to_string(), Some(dir.display().to_string()))
            })?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Some(record) = read_path(&entry.path())? {
                if record.operation == operation
                    && record.subject == subject
                    && (!pending_only || record.finalization_pending())
                {
                    records.push(record);
                }
            }
        }
        records.sort_by(|left, right| left.owner_run_ref.cmp(&right.owner_run_ref));
        Ok(records)
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// The operation-record store below an explicitly injected data root.
///
/// The ambient form resolved `paths::observation_db()` and took its parent.
/// `observation_db()` is `homeboy_data()?.join("homeboy.sqlite")`, so that
/// parent is exactly the data root: the database path was only ever a detour
/// through it, and this store has never read or written the database itself.
/// Naming the data root directly removes both the detour and the infallible
/// `parent()` failure branch it required.
fn store_dir_in_roots(data_root: &Path) -> PathBuf {
    data_root.join("operation-records")
}

fn record_path_in_roots(data_root: &Path, owner_run_ref: &str) -> PathBuf {
    store_dir_in_roots(data_root).join(format!(
        "{}.json",
        paths::sanitize_path_segment(owner_run_ref)
    ))
}

fn lock_in_roots(data_root: &Path) -> Result<std::fs::File> {
    let dir = store_dir_in_roots(data_root);
    fs::create_dir_all(&dir)
        .map_err(|error| Error::internal_io(error.to_string(), Some(dir.display().to_string())))?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // The lock is never written; retain any existing bytes while reopening it.
        .truncate(false)
        .open(dir.join(".lock"))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open operation record lock".to_string()),
            )
        })?;
    file.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock operation records".to_string()),
        )
    })?;
    Ok(file)
}

fn read_path(path: &Path) -> Result<Option<OperationRecord>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    /// The record store for the isolated home each test below installs.
    ///
    /// `with_isolated_home` establishes the home; this binds a store to it once,
    /// the same way the release boundary binds one for a whole command (#7505).
    fn test_store() -> OperationRecordStore {
        OperationRecordStore::in_roots(
            &homeboy_core::paths::PathRoots::from_environment().expect("path roots"),
        )
    }

    fn record() -> OperationRecord {
        OperationRecord {
            owner_run_ref: "release/test-owner".to_string(),
            operation: "provider_workspace".to_string(),
            subject: "component".to_string(),
            provider: "provider".to_string(),
            handle: "handle".to_string(),
            path: Some("/workspace".to_string()),
            source_sha: "abc".to_string(),
            cleanup_policy: "remove_on_success".to_string(),
            lifecycle_state: "provisioned".to_string(),
            terminal_disposition: None,
            finalization_status: "pending".to_string(),
            finalization_lease: None,
            finalization_lease_started_ms: None,
            attempt_count: 0,
            continuation_evidence: vec!["created".to_string()],
            attributes: serde_json::Map::new(),
        }
    }

    #[test]
    fn persists_reloads_and_compare_updates_operation_records() {
        with_isolated_home(|_| {
            let record = record();
            test_store().create(&record).expect("persist");
            let reloaded = test_store()
                .load(&record.owner_run_ref)
                .expect("load")
                .expect("record");
            assert_eq!(reloaded, record);
            let updated = test_store()
                .update(&record.owner_run_ref, |current| {
                    let mut current = current.expect("record");
                    current.attempt_count += 1;
                    current.finalization_status = "completed".to_string();
                    Ok(current)
                })
                .expect("atomic update");
            assert_eq!(updated.attempt_count, 1);
            assert!(!test_store()
                .pending_for_subject("provider_workspace", "component")
                .expect("pending")
                .iter()
                .any(|record| record.owner_run_ref == "release/test-owner"));
        });
    }

    #[test]
    fn concurrent_finalizers_claim_one_provider_effect() {
        with_isolated_home(|_| {
            let record = record();
            test_store().create(&record).expect("persist");
            let barrier = Arc::new(Barrier::new(2));
            let effects = Arc::new(AtomicUsize::new(0));
            let workers = (0..2)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    let effects = Arc::clone(&effects);
                    let owner = record.owner_run_ref.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        if let FinalizationClaim::Claimed { lease, .. } =
                            test_store().claim_finalization(&owner).expect("claim")
                        {
                            // This is the provider-effect boundary: only a claimant may cross it.
                            effects.fetch_add(1, Ordering::SeqCst);
                            test_store()
                                .complete_finalization(&owner, &lease)
                                .expect("complete");
                        }
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().expect("finalizer thread");
            }
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            let completed = test_store()
                .load(&record.owner_run_ref)
                .expect("load")
                .expect("record");
            assert_eq!(completed.finalization_status, "completed");
            assert!(matches!(
                test_store()
                    .claim_finalization(&record.owner_run_ref)
                    .expect("completed claim"),
                FinalizationClaim::AlreadyCompleted(_)
            ));
        });
    }
}
