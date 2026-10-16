//! Durable, provider-agnostic records for operations that cross process boundaries.
//!
//! Each record is a coordination record in the observation store (#15947),
//! keyed by `owner_run_ref`. Every update is a read-modify-write guarded by a
//! revision compare-and-set and retried on conflict, so a recovered finalizer
//! can never race a still-running owner into a lost update.
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_core::observation::coordination::{
    CoordinationRecord, ExpectedRevision, RecordOutcome, RecordWrite,
};
use homeboy_core::observation::ObservationStore;
use homeboy_core::{paths, Error, Result};

/// Coordination namespace owned by release operation records.
pub(crate) const NAMESPACE: &str = "release-operation-records";

/// Bound on compare-and-set retries for one update. Each retry means another
/// writer committed in between, so exhausting this is a genuine livelock.
const MAX_UPDATE_ATTEMPTS: usize = 64;

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
    #[serde(default)]
    pub mutation_attempted: bool,
    pub continuation_evidence: Vec<String>,
    #[serde(default)]
    pub attributes: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinalizationClaim {
    Claimed {
        lease: String,
        record: OperationRecord,
    },
    AlreadyCompleted(OperationRecord),
    InProgress(OperationRecord),
}

impl OperationRecord {
    pub(crate) fn finalization_pending(&self) -> bool {
        self.finalization_status != "completed"
    }
}

/// Durable operation records in the observation store of one explicitly
/// injected home.
///
/// Stateful on purpose. A single release workspace lifecycle writes this store
/// roughly a dozen times — publish ownership, record the provisioned path,
/// mark a failed verification, claim finalization, complete it — and binding
/// the roots once at construction is what makes those writes provably the same
/// home's, rather than incidentally so (#7505).
pub struct OperationRecordStore {
    roots: paths::PathRoots,
}

impl OperationRecordStore {
    /// Bind the store to the roots its caller already resolved.
    pub fn in_roots(roots: &paths::PathRoots) -> Self {
        Self {
            roots: roots.clone(),
        }
    }

    fn open(&self) -> Result<ObservationStore> {
        ObservationStore::open_initialized_for_lifecycle_in_roots(&self.roots)
    }

    pub fn create(&self, record: &OperationRecord) -> Result<OperationRecord> {
        self.update(&record.owner_run_ref, |_| Ok(record.clone()))
    }

    /// Compare/update against the stored revision. A concurrent writer makes
    /// the write conflict; the update is then re-applied to the fresh record,
    /// so `update` may run more than once and must not consume its captures.
    pub(crate) fn update(
        &self,
        owner_run_ref: &str,
        mut update: impl FnMut(Option<OperationRecord>) -> Result<OperationRecord>,
    ) -> Result<OperationRecord> {
        let store = self.open()?;
        let mut current = store.coordination_record(NAMESPACE, owner_run_ref)?;
        for _ in 0..MAX_UPDATE_ATTEMPTS {
            let expected = current.as_ref().map_or(ExpectedRevision::Absent, |row| {
                ExpectedRevision::Revision(row.revision)
            });
            let existing = current.as_ref().map(parse_record).transpose()?;
            let next = update(existing)?;
            if next.owner_run_ref != owner_run_ref {
                return Err(Error::validation_invalid_argument(
                    "owner_run_ref",
                    "operation record owner reference cannot change",
                    Some(owner_run_ref.to_string()),
                    None,
                ));
            }
            let payload = serde_json::to_value(&next).map_err(|error| {
                Error::internal_json(error.to_string(), Some(owner_run_ref.to_string()))
            })?;
            match store.put_coordination_record(
                NAMESPACE,
                owner_run_ref,
                expected,
                RecordWrite {
                    state: &next.finalization_status,
                    payload: &payload,
                    expires_at_ms: None,
                },
            )? {
                RecordOutcome::Applied(_) => return Ok(next),
                RecordOutcome::Conflict { current: latest } => current = latest,
            }
        }
        Err(Error::internal_unexpected(format!(
            "operation record '{owner_run_ref}' kept changing during {MAX_UPDATE_ATTEMPTS} update attempts"
        )))
    }

    pub fn load(&self, owner_run_ref: &str) -> Result<Option<OperationRecord>> {
        self.open()?
            .coordination_record(NAMESPACE, owner_run_ref)?
            .as_ref()
            .map(parse_record)
            .transpose()
    }

    /// Claims finalization before an external provider call. A live lease is
    /// never stolen; only a bounded stale lease can be retried.
    pub(crate) fn claim_finalization(&self, owner_run_ref: &str) -> Result<FinalizationClaim> {
        // Configured provider mutation timeouts may be as high as fifteen
        // minutes. Never steal a lease while that mutation can still be live.
        const STALE_LEASE_MS: u128 = 16 * 60 * 1000;
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

    pub(crate) fn complete_finalization(
        &self,
        owner_run_ref: &str,
        lease: &str,
        receipt: Value,
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
                return Err(Error::validation_invalid_argument(
                    "finalization_lease",
                    "finalization lease no longer matches durable authority",
                    Some(owner_run_ref.to_string()),
                    None,
                ));
            }
            record.finalization_status = "completed".to_string();
            record.lifecycle_state = "finalized".to_string();
            record.finalization_lease = None;
            record.finalization_lease_started_ms = None;
            record
                .attributes
                .insert("finalization_receipt".to_string(), receipt.clone());
            record
                .continuation_evidence
                .push("provider finalization completed".to_string());
            Ok(record)
        })
    }

    pub(crate) fn mark_mutation_attempted(
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
                return Err(Error::validation_invalid_argument(
                    "finalization_lease",
                    "finalization lease no longer matches durable authority",
                    Some(owner_run_ref.to_string()),
                    None,
                ));
            }
            record.mutation_attempted = true;
            Ok(record)
        })
    }

    pub(crate) fn fail_finalization(
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
            record.continuation_evidence.push(error.clone());
            Ok(record)
        })
    }

    pub fn for_subject(
        &self,
        operation: &str,
        subject: &str,
        pending_only: bool,
    ) -> Result<Vec<OperationRecord>> {
        let mut records = Vec::new();
        for row in self.open()?.coordination_records(NAMESPACE)? {
            let record = parse_record(&row)?;
            if record.operation == operation
                && record.subject == subject
                && (!pending_only || record.finalization_pending())
            {
                records.push(record);
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

fn parse_record(row: &CoordinationRecord) -> Result<OperationRecord> {
    serde_json::from_value(row.payload.clone())
        .map_err(|error| Error::internal_json(error.to_string(), Some(row.key.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    /// A record store bound to a caller-owned data root.
    ///
    /// Deliberately not `with_isolated_home`: the store opens the observation
    /// database below the injected data root and nothing else, so a test can
    /// name its own directory instead of mutating `HOME` and queueing behind
    /// the process-global mutex that guards it (#7505). Config and artifacts
    /// share the directory because this store never consults them.
    fn test_store_at(data_root: &std::path::Path) -> OperationRecordStore {
        OperationRecordStore::in_roots(&homeboy_core::paths::PathRoots::new(
            data_root.to_path_buf(),
            data_root.to_path_buf(),
            data_root.to_path_buf(),
        ))
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
            mutation_attempted: false,
            continuation_evidence: vec!["created".to_string()],
            attributes: serde_json::Map::new(),
        }
    }

    #[test]
    fn persists_reloads_and_compare_updates_operation_records() {
        let data_root = tempfile::tempdir().expect("data root");
        let store = test_store_at(data_root.path());
        let record = record();
        store.create(&record).expect("persist");
        let reloaded = store
            .load(&record.owner_run_ref)
            .expect("load")
            .expect("record");
        assert_eq!(reloaded, record);
        let updated = store
            .update(&record.owner_run_ref, |current| {
                let mut current = current.expect("record");
                current.attempt_count += 1;
                current.finalization_status = "completed".to_string();
                Ok(current)
            })
            .expect("atomic update");
        assert_eq!(updated.attempt_count, 1);
        assert!(!store
            .for_subject("provider_workspace", "component", true)
            .expect("pending")
            .iter()
            .any(|record| record.owner_run_ref == "release/test-owner"));
    }

    #[test]
    fn records_survive_reopening_the_store_and_list_by_subject() {
        let data_root = tempfile::tempdir().expect("data root");
        let pending = record();
        let mut completed = record();
        completed.owner_run_ref = "release/completed-owner".to_string();
        completed.finalization_status = "completed".to_string();
        let mut other_subject = record();
        other_subject.owner_run_ref = "release/other-owner".to_string();
        other_subject.subject = "other".to_string();
        {
            let store = test_store_at(data_root.path());
            for record in [&pending, &completed, &other_subject] {
                store.create(record).expect("persist");
            }
        }

        let reopened = test_store_at(data_root.path());
        assert_eq!(
            reopened.load(&pending.owner_run_ref).expect("load"),
            Some(pending.clone())
        );
        let owners = |pending_only| {
            reopened
                .for_subject("provider_workspace", "component", pending_only)
                .expect("for subject")
                .into_iter()
                .map(|record| record.owner_run_ref)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            owners(false),
            ["release/completed-owner", "release/test-owner"]
        );
        assert_eq!(owners(true), ["release/test-owner"]);
        assert!(reopened.load("release/missing").expect("load").is_none());
    }

    #[test]
    fn concurrent_updates_are_never_lost() {
        const WRITERS: usize = 8;
        let data_root = tempfile::tempdir().expect("data root");
        let record = record();
        test_store_at(data_root.path())
            .create(&record)
            .expect("persist");
        let barrier = Arc::new(Barrier::new(WRITERS));
        let workers = (0..WRITERS)
            .map(|writer| {
                let barrier = Arc::clone(&barrier);
                let owner = record.owner_run_ref.clone();
                let root = data_root.path().to_path_buf();
                std::thread::spawn(move || {
                    let store = test_store_at(&root);
                    barrier.wait();
                    store
                        .update(&owner, |current| {
                            let mut current = current.expect("record");
                            current
                                .continuation_evidence
                                .push(format!("writer {writer}"));
                            Ok(current)
                        })
                        .expect("update");
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("writer thread");
        }
        let stored = test_store_at(data_root.path())
            .load(&record.owner_run_ref)
            .expect("load")
            .expect("record");
        assert_eq!(stored.continuation_evidence.len(), WRITERS + 1);
    }

    #[test]
    fn concurrent_finalizers_claim_one_provider_effect() {
        let data_root = tempfile::tempdir().expect("data root");
        let store = test_store_at(data_root.path());
        let record = record();
        store.create(&record).expect("persist");
        let barrier = Arc::new(Barrier::new(2));
        let effects = Arc::new(AtomicUsize::new(0));
        let workers = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let effects = Arc::clone(&effects);
                let owner = record.owner_run_ref.clone();
                // Each worker builds its own store over the SAME root, which is
                // what two racing processes actually do. The ambient form had
                // them inherit one mutated `HOME` instead, so the contention it
                // exercised was weaker than the contention it claims to test.
                let root = data_root.path().to_path_buf();
                std::thread::spawn(move || {
                    let store = test_store_at(&root);
                    barrier.wait();
                    if let FinalizationClaim::Claimed { lease, .. } =
                        store.claim_finalization(&owner).expect("claim")
                    {
                        // This is the provider-effect boundary: only a claimant may cross it.
                        effects.fetch_add(1, Ordering::SeqCst);
                        store
                            .complete_finalization(
                                &owner,
                                &lease,
                                serde_json::json!({"test": true}),
                            )
                            .expect("complete");
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("finalizer thread");
        }
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let completed = store
            .load(&record.owner_run_ref)
            .expect("load")
            .expect("record");
        assert_eq!(completed.finalization_status, "completed");
        assert!(matches!(
            store
                .claim_finalization(&record.owner_run_ref)
                .expect("completed claim"),
            FinalizationClaim::AlreadyCompleted(_)
        ));
    }
}
