use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::persistence::read_durable_store;
#[cfg(test)]
use super::persistence::reconcile_stale_jobs;
use super::persistence::{
    apply_event_retention, compact_terminal_jobs, job_not_found, lookup_tombstone,
    prepare_tombstone_store, recovered_terminal_from_result, timestamp_ms, tombstone_store_report,
    validate_transition, write_durable_store_with_tombstones, JobStoreCompactionEvidence,
    ReplayTombstoneKind, DEFAULT_EVENT_RETENTION_LIMIT, DEFAULT_TERMINAL_JOB_RETENTION_BYTES,
    DEFAULT_TERMINAL_JOB_RETENTION_LIMIT,
};
use super::remote_runner;
use super::remote_runner::JobArtifactMetadata;
use super::types::{Job, JobEvent, JobEventKind, JobStatus, RunnerJobProjection};
use crate::error::{Error, Result};
use crate::runner_execution_envelope::PathMaterializationPlan;
use crate::source_snapshot::SourceSnapshot;

mod reconciliation;

/// A reservation bounds the interval between durable admission and persisting a
/// child identity. The child is normally spawned immediately after admission;
/// a longer-lived record means no child was durably confirmed.
const LOCAL_CHILD_RESERVATION_LEASE_MS: u64 = 60_000;
/// Admissions protect the controller-to-daemon handoff window. A stopped
/// controller must eventually stop consuming daemon replacement capacity.
pub(crate) const ADMISSION_RESERVATION_LEASE_MS: u64 = 30_000;

#[derive(Debug, Clone)]
pub(crate) struct AdmissionReservation {
    pub(crate) job: Job,
    pub(crate) token: String,
    pub(crate) expires_at_ms: u64,
    pub(crate) created: bool,
    /// Exact direct-daemon authority registered before this reservation commit.
    #[allow(
        dead_code,
        reason = "Reservation provenance asserted by cfg(test) admission paths; production branches on the reservation itself."
    )]
    pub(crate) workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
}

#[derive(Debug, Clone, Default)]
pub struct JobStore {
    pub(super) inner: Arc<Mutex<JobStoreInner>>,
    pub(super) next_event_sequence: Arc<AtomicU64>,
    pub(super) persistence: Option<Arc<JobStorePersistence>>,
    pub(super) daemon_lease_id: Option<String>,
    #[cfg(test)]
    terminal_write_failures: Arc<AtomicU64>,
    #[cfg(test)]
    durable_write_failures: Arc<AtomicU64>,
    #[cfg(test)]
    durable_write_skips: Arc<AtomicU64>,
}

impl JobStore {
    /// Stable process-local ownership key for runtime state associated with this
    /// durable queue. Independent test stores must never share supervisors.
    pub(crate) fn runtime_registry_scope(&self) -> String {
        match &self.persistence {
            Some(persistence) => std::fs::canonicalize(&persistence.path)
                .unwrap_or_else(|_| persistence.path.clone())
                .to_string_lossy()
                .into_owned(),
            None => format!("memory:{:p}", std::sync::Arc::as_ptr(&self.inner)),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct JobStorePersistence {
    pub(super) path: PathBuf,
    pub(super) event_retention_limit: usize,
    pub(super) terminal_job_retention_limit: usize,
    pub(super) terminal_job_retention_bytes: usize,
}

/// The advisory lock held for one authoritative durable-store transaction.
///
/// Keep this private: callers must use [`JobStore::durable_transaction`] so
/// they cannot mutate a stale in-memory snapshot between reload and commit.
pub(super) struct DurableStoreTransaction {
    // `flock` is process-wide on some targets but is not reentrant across file
    // descriptors. Serialize local stores before taking the cross-process lock.
    _process_guard: MutexGuard<'static, ()>,
    _file: File,
}

#[derive(Debug, Clone, Default)]
pub(super) struct JobStoreInner {
    pub(super) jobs: HashMap<Uuid, StoredJob>,
    pub(super) submission_keys: HashMap<String, RemoteRunnerSubmission>,
    pub(super) expired_submission_keys: HashMap<String, RemoteRunnerSubmission>,
    pub(super) controller_submissions: HashMap<String, ControllerJobSubmission>,
    pub(super) expired_controller_submissions: HashMap<String, ControllerJobSubmission>,
    pub(super) compaction: Option<JobStoreCompactionEvidence>,
}

/// A durable, caller-owned admission identity. The fingerprint makes reuse of a
/// key with different work fail closed instead of silently selecting a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RemoteRunnerSubmission {
    pub(super) fingerprint: String,
    pub(super) job_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ControllerJobState {
    pub(crate) job_type: String,
    pub(crate) version: u32,
    /// Private daemon-store state. It is never copied into `JobEvent` data.
    pub(crate) request: Value,
    /// Driver-owned safe projection exposed in the queued event and API logs.
    pub(crate) public_request: Value,
    pub(crate) request_digest: String,
    /// Optional semantic identity that coalesces matching work only while its
    /// durable job remains nonterminal. The caller's idempotency key still
    /// permanently identifies this particular submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_idempotency_key: Option<String>,
    /// The controller-minted durable run this job executes for, declared by
    /// the driver from its typed request and persisted at admission — before
    /// any driver work can escape the daemon lifecycle. Recovery reconciles
    /// this job from the linked run's terminal state without parsing the
    /// opaque request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) linked_durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) checkpoint: Option<Value>,
    #[serde(default)]
    pub(crate) cancellation_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cancellation_reason: Option<String>,
    /// A durable owner marker is written before a daemon dispatches this job.
    /// Startup replaces an abandoned marker with one recovery claim before it
    /// invokes the driver's recovery entry point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) execution_claim_id: Option<String>,
    #[serde(default)]
    pub(crate) recovery_attempted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ControllerJobSubmission {
    pub(super) fingerprint: String,
    pub(super) job_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) terminal_job: Option<Job>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredJob {
    pub(super) job: Job,
    pub(super) events: Vec<JobEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) admission_idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) controller_job: Option<ControllerJobState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) admission_lease: Option<AdmissionLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) remote_runner: Option<remote_runner::StoredRemoteRunnerJob>,
    /// Typed execution identity for a daemon-local child submitted on behalf of
    /// a remote runner. This lets `/jobs` project the accepted runner job without
    /// inventing a synthetic durable run ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) local_runner: Option<LocalRunnerJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) local_child: Option<LocalChildExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AdmissionLease {
    token: String,
    expires_at_ms: u64,
    #[serde(default)]
    renewals: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalRunnerJob {
    pub(crate) runner_id: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) lifecycle: Option<super::remote_runner::RunnerJobLifecycleMetadata>,
    /// Reconciliation claims remain reverse-broker-only authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_claim_binding: Option<crate::workspace_claim::WorkspaceClaimBinding>,
    /// Exact direct-daemon authority required to execute this queued job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[allow(
        dead_code,
        reason = "Reservation provenance asserted by cfg(test) admission paths; production branches on the reservation itself."
    )]
    pub(crate) workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LocalChildExecution {
    reservation_id: String,
    /// Missing only on records written before reservation leases existed. Those
    /// records remain fail-closed because Homeboy cannot prove their spawn state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reservation_expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process: Option<LocalChildProcessIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct LocalChildProcessIdentity {
    pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_group_id: Option<u32>,
    discriminator: LocalChildStartDiscriminator,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LocalChildStartDiscriminator {
    LinuxProcStatStarttimeTicks { ticks: u64 },
    Unsupported { evidence: String },
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct DurableJobStore {
    pub(super) jobs: Vec<StoredJob>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(super) submission_keys: HashMap<String, RemoteRunnerSubmission>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(super) expired_submission_keys: HashMap<String, RemoteRunnerSubmission>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(super) controller_submissions: HashMap<String, ControllerJobSubmission>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(super) expired_controller_submissions: HashMap<String, ControllerJobSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) compaction: Option<JobStoreCompactionEvidence>,
}

#[derive(Debug)]
pub struct JobRunner {
    pub job_id: Uuid,
    pub handle: JoinHandle<()>,
}

/// Durable inputs shared by local-runner job creation and capacity admission.
#[derive(Clone)]
pub(crate) struct LocalRunnerJobRequest {
    pub(crate) operation: String,
    pub(crate) source_snapshot: Option<SourceSnapshot>,
    pub(crate) metadata: Option<Value>,
    pub(crate) path_materialization_plan: Option<PathMaterializationPlan>,
    pub(crate) local_runner: Option<LocalRunnerJob>,
    pub(crate) admission_idempotency_key: Option<String>,
}

/// The durable result of a controller submission lookup or admission.
pub(crate) enum ControllerJobSubmissionOutcome {
    Submitted(Uuid),
    Existing(Box<Job>),
}

pub(crate) enum ControllerJobStartOutcome {
    Claimed(ControllerJobState),
    Existing,
}

#[derive(Debug, Clone)]
pub struct JobHandle {
    store: JobStore,
    job_id: Uuid,
}

impl JobStore {
    pub(super) fn begin_durable_transaction(&self) -> Result<Option<DurableStoreTransaction>> {
        let Some(persistence) = &self.persistence else {
            return Ok(None);
        };
        static DURABLE_STORE_PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let process_guard = DURABLE_STORE_PROCESS_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("durable store process mutex poisoned");
        let parent = persistence
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
        let name = persistence
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("jobs.json");
        let path = parent.join(format!(".{name}.transaction.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // This persistent lock has no payload; opening it must retain any existing bytes.
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("open {}", path.display())))
            })?;
        file.lock_exclusive().map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("lock {}", path.display())))
        })?;
        Ok(Some(DurableStoreTransaction {
            _process_guard: process_guard,
            _file: file,
        }))
    }

    pub(super) fn reload_durable_snapshot_already_locked(
        &self,
        path: &std::path::Path,
        inner: &mut JobStoreInner,
    ) -> Result<()> {
        let durable = read_durable_store(path)?;
        remote_runner::validate_stored_remote_runner_jobs(&durable.jobs)?;
        let next_sequence = durable
            .jobs
            .iter()
            .flat_map(|stored| stored.events.iter().map(|event| event.sequence))
            .max()
            .unwrap_or_default();
        inner.jobs = durable
            .jobs
            .into_iter()
            .map(|stored| (stored.job.id, stored))
            .collect();
        inner.submission_keys = durable.submission_keys;
        inner.expired_submission_keys = durable.expired_submission_keys;
        inner.controller_submissions = durable.controller_submissions;
        inner.expired_controller_submissions = durable.expired_controller_submissions;
        inner.compaction = durable.compaction;
        self.next_event_sequence
            .fetch_max(next_sequence, Ordering::SeqCst);
        Ok(())
    }

    /// Apply one mutation to the current durable snapshot and commit it before
    /// releasing the inter-process lock. The closure is the only supported path
    /// for a durable whole-snapshot mutation.
    pub(super) fn durable_transaction<T>(
        &self,
        mutation: impl FnOnce(&mut JobStoreInner) -> Result<T>,
    ) -> Result<T> {
        let transaction = self.begin_durable_transaction()?;
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        if let Some(persistence) = &self.persistence {
            self.reload_durable_snapshot_already_locked(&persistence.path, &mut inner)?;
        }
        let prior = inner.clone();
        let output = match mutation(&mut inner) {
            Ok(output) => output,
            Err(error) => {
                *inner = prior;
                return Err(error);
            }
        };
        if transaction.is_some() {
            if let Err(error) = self.persist_inner_already_locked(&mut inner) {
                *inner = prior;
                return Err(error);
            }
        }
        drop(inner);
        drop(transaction);
        Ok(output)
    }

    /// Commit the supplied authoritative in-memory state. The caller holds the
    /// durable transaction lock and `inner` mutex; this helper never reloads or
    /// acquires either lock.
    pub(super) fn persist_inner_already_locked(&self, inner: &mut JobStoreInner) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        #[cfg(test)]
        if self
            .durable_write_skips
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_err()
            && self
                .durable_write_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        {
            return Err(Error::internal_io(
                "injected durable store write failure",
                None,
            ));
        }
        let mut durable = DurableJobStore {
            jobs: inner.jobs.values().cloned().collect(),
            submission_keys: inner.submission_keys.clone(),
            expired_submission_keys: inner.expired_submission_keys.clone(),
            controller_submissions: inner.controller_submissions.clone(),
            expired_controller_submissions: inner.expired_controller_submissions.clone(),
            compaction: inner.compaction.clone(),
        };
        compact_terminal_jobs(
            &mut durable,
            persistence.event_retention_limit,
            persistence.terminal_job_retention_limit,
            persistence.terminal_job_retention_bytes,
        );
        write_durable_store_with_tombstones(&persistence.path, &mut durable)?;
        inner.jobs = durable
            .jobs
            .into_iter()
            .map(|stored| (stored.job.id, stored))
            .collect();
        inner.submission_keys = durable.submission_keys;
        inner.expired_submission_keys = durable.expired_submission_keys;
        inner.controller_submissions = durable.controller_submissions;
        inner.expired_controller_submissions = durable.expired_controller_submissions;
        inner.compaction = durable.compaction;
        Ok(())
    }

    /// Count non-terminal jobs without opening or reconciling the durable store.
    ///
    /// Daemon status runs in a separate CLI process, so using [`Self::open`]
    /// here would reconcile live jobs as though the daemon had restarted.
    pub(crate) fn active_count_at_path(path: impl Into<PathBuf>) -> Result<usize> {
        let path = path.into();
        if !path.exists() {
            return Ok(0);
        }
        let content = fs::read_to_string(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
        })?;
        let durable: DurableJobStore = serde_json::from_str(&content)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))?;
        let now = timestamp_ms();
        Ok(durable
            .jobs
            .into_iter()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            // Status commands run outside the daemon and must not persist a
            // lifecycle transition, but an expired admission is no longer an
            // owner of daemon capacity. Startup/the daemon reconciler records
            // the terminal transition durably.
            .filter(|stored| {
                stored.job.operation != "runner.admission"
                    || stored
                        .admission_lease
                        .as_ref()
                        .is_none_or(|lease| lease.expires_at_ms > now)
            })
            .count())
    }

    /// Report the durable owners that a daemon would retain after opening this
    /// store. Tombstone identities remain on disk and are deliberately excluded
    /// from resident owner counts.
    pub fn retained_owner_report_at_path(path: impl Into<PathBuf>) -> Result<Value> {
        let path = path.into();
        if !path.exists() {
            return Ok(serde_json::json!({
                "jobs": { "count": 0, "bytes": 0, "active_count": 0, "terminal_count": 0 },
                "live_submission_keys": { "count": 0, "bytes": 0 },
                "legacy_expired_submission_keys": { "count": 0, "bytes": 0 },
                "replay_tombstone_journal": { "count": 0, "bytes": 0, "resident": false },
            }));
        }
        let content = fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?;
        let durable: DurableJobStore = serde_json::from_str(&content)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        let job_bytes = durable
            .jobs
            .iter()
            .map(|job| {
                serde_json::to_vec(job)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0)
            })
            .sum::<usize>();
        let active_count = durable
            .jobs
            .iter()
            .filter(|job| !job.job.status.is_terminal())
            .count();
        let terminal_count = durable.jobs.len() - active_count;
        let live_submission_keys =
            durable.submission_keys.len() + durable.controller_submissions.len();
        let live_submission_bytes = serde_json::to_vec(&durable.submission_keys)
            .unwrap_or_default()
            .len()
            + serde_json::to_vec(&durable.controller_submissions)
                .unwrap_or_default()
                .len();
        let legacy_count =
            durable.expired_submission_keys.len() + durable.expired_controller_submissions.len();
        let legacy_bytes = serde_json::to_vec(&durable.expired_submission_keys)
            .unwrap_or_default()
            .len()
            + serde_json::to_vec(&durable.expired_controller_submissions)
                .unwrap_or_default()
                .len();
        let (journal_count, journal_bytes) = tombstone_store_report(&path)?;
        Ok(serde_json::json!({
            "jobs": { "count": durable.jobs.len(), "bytes": job_bytes, "active_count": active_count, "terminal_count": terminal_count },
            "live_submission_keys": { "count": live_submission_keys, "bytes": live_submission_bytes },
            "legacy_expired_submission_keys": { "count": legacy_count, "bytes": legacy_bytes },
            "replay_tombstone_journal": { "count": journal_count, "bytes": journal_bytes, "resident": false },
        }))
    }

    #[cfg(test)]
    pub(crate) fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_retention(
            path,
            DEFAULT_EVENT_RETENTION_LIMIT,
            DEFAULT_TERMINAL_JOB_RETENTION_LIMIT,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_with_event_retention(
        path: impl Into<PathBuf>,
        event_retention_limit: usize,
    ) -> Result<Self> {
        Self::open_with_retention(
            path,
            event_retention_limit,
            DEFAULT_TERMINAL_JOB_RETENTION_LIMIT,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_with_retention(
        path: impl Into<PathBuf>,
        event_retention_limit: usize,
        terminal_job_retention_limit: usize,
    ) -> Result<Self> {
        Self::open_with_retention_and_terminal_byte_limit(
            path,
            event_retention_limit,
            terminal_job_retention_limit,
            DEFAULT_TERMINAL_JOB_RETENTION_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_with_retention_and_terminal_byte_limit(
        path: impl Into<PathBuf>,
        event_retention_limit: usize,
        terminal_job_retention_limit: usize,
        terminal_job_retention_bytes: usize,
    ) -> Result<Self> {
        let path = path.into();
        prepare_tombstone_store(&path)?;
        let mut durable = read_durable_store(&path)?;
        let event_retention_limit = event_retention_limit.max(1);
        let terminal_job_retention_limit = terminal_job_retention_limit.max(1);
        let terminal_job_retention_bytes = terminal_job_retention_bytes.max(1);
        let next_sequence = reconcile_stale_jobs(&mut durable, event_retention_limit);
        compact_terminal_jobs(
            &mut durable,
            event_retention_limit,
            terminal_job_retention_limit,
            terminal_job_retention_bytes,
        );
        let store = Self {
            inner: Arc::new(Mutex::new(JobStoreInner {
                jobs: durable
                    .jobs
                    .into_iter()
                    .map(|stored| (stored.job.id, stored))
                    .collect(),
                submission_keys: durable.submission_keys,
                expired_submission_keys: durable.expired_submission_keys,
                controller_submissions: durable.controller_submissions,
                expired_controller_submissions: durable.expired_controller_submissions,
                compaction: durable.compaction,
            })),
            next_event_sequence: Arc::new(AtomicU64::new(next_sequence)),
            persistence: Some(Arc::new(JobStorePersistence {
                path,
                event_retention_limit,
                terminal_job_retention_limit,
                terminal_job_retention_bytes,
            })),
            daemon_lease_id: None,
            #[cfg(test)]
            terminal_write_failures: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            durable_write_failures: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            durable_write_skips: Arc::new(AtomicU64::new(0)),
        };

        store.durable_transaction(|inner| {
            let mut durable = DurableJobStore {
                jobs: inner.jobs.values().cloned().collect(),
                submission_keys: inner.submission_keys.clone(),
                expired_submission_keys: inner.expired_submission_keys.clone(),
                controller_submissions: inner.controller_submissions.clone(),
                expired_controller_submissions: inner.expired_controller_submissions.clone(),
                compaction: inner.compaction.clone(),
            };
            let next_sequence = reconcile_stale_jobs(&mut durable, event_retention_limit);
            inner.jobs = durable
                .jobs
                .into_iter()
                .map(|stored| (stored.job.id, stored))
                .collect();
            inner.submission_keys = durable.submission_keys;
            inner.expired_submission_keys = durable.expired_submission_keys;
            inner.controller_submissions = durable.controller_submissions;
            inner.expired_controller_submissions = durable.expired_controller_submissions;
            inner.compaction = durable.compaction;
            store
                .next_event_sequence
                .fetch_max(next_sequence, Ordering::SeqCst);
            Ok(())
        })?;
        Ok(store)
    }

    /// Open durable jobs without treating active records as an implicit daemon
    /// restart. Daemon lifecycle recovery must select ownership explicitly.
    pub fn open_without_reconciliation(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        prepare_tombstone_store(&path)?;
        let raw = fs::read(&path).unwrap_or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                b"{\"jobs\":[]}".to_vec()
            } else {
                Vec::new()
            }
        });
        if raw.is_empty() && path.exists() {
            return Err(Error::internal_io(
                "read durable job store",
                Some(path.display().to_string()),
            ));
        }
        Self::open_without_reconciliation_from_bytes(path, &raw)
    }

    pub fn open_without_reconciliation_from_bytes(
        path: impl Into<PathBuf>,
        raw: &[u8],
    ) -> Result<Self> {
        Self::open_without_reconciliation_from_bytes_with_retention(
            path,
            raw,
            DEFAULT_EVENT_RETENTION_LIMIT,
            DEFAULT_TERMINAL_JOB_RETENTION_LIMIT,
            DEFAULT_TERMINAL_JOB_RETENTION_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_without_reconciliation_with_retention(
        path: impl Into<PathBuf>,
        event_retention_limit: usize,
        terminal_job_retention_limit: usize,
    ) -> Result<Self> {
        Self::open_without_reconciliation_with_retention_and_terminal_byte_limit(
            path,
            event_retention_limit,
            terminal_job_retention_limit,
            DEFAULT_TERMINAL_JOB_RETENTION_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_without_reconciliation_with_retention_and_terminal_byte_limit(
        path: impl Into<PathBuf>,
        event_retention_limit: usize,
        terminal_job_retention_limit: usize,
        terminal_job_retention_bytes: usize,
    ) -> Result<Self> {
        let path = path.into();
        let raw = fs::read(&path).unwrap_or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                b"{\"jobs\":[]}".to_vec()
            } else {
                Vec::new()
            }
        });
        if raw.is_empty() && path.exists() {
            return Err(Error::internal_io(
                "read durable job store",
                Some(path.display().to_string()),
            ));
        }
        Self::open_without_reconciliation_from_bytes_with_retention(
            path,
            &raw,
            event_retention_limit,
            terminal_job_retention_limit,
            terminal_job_retention_bytes,
        )
    }

    fn open_without_reconciliation_from_bytes_with_retention(
        path: impl Into<PathBuf>,
        raw: &[u8],
        event_retention_limit: usize,
        terminal_job_retention_limit: usize,
        terminal_job_retention_bytes: usize,
    ) -> Result<Self> {
        let path = path.into();
        prepare_tombstone_store(&path)?;
        let mut durable: DurableJobStore = serde_json::from_slice(raw)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))?;
        remote_runner::validate_stored_remote_runner_jobs(&durable.jobs)?;
        let event_retention_limit = event_retention_limit.max(1);
        let terminal_job_retention_limit = terminal_job_retention_limit.max(1);
        let terminal_job_retention_bytes = terminal_job_retention_bytes.max(1);
        compact_terminal_jobs(
            &mut durable,
            event_retention_limit,
            terminal_job_retention_limit,
            terminal_job_retention_bytes,
        );
        let next_sequence = durable
            .jobs
            .iter()
            .flat_map(|stored| stored.events.iter().map(|event| event.sequence))
            .max()
            .unwrap_or(0);
        let store = Self {
            inner: Arc::new(Mutex::new(JobStoreInner {
                jobs: durable
                    .jobs
                    .into_iter()
                    .map(|stored| (stored.job.id, stored))
                    .collect(),
                submission_keys: durable.submission_keys,
                expired_submission_keys: durable.expired_submission_keys,
                controller_submissions: durable.controller_submissions,
                expired_controller_submissions: durable.expired_controller_submissions,
                compaction: durable.compaction,
            })),
            next_event_sequence: Arc::new(AtomicU64::new(next_sequence)),
            persistence: Some(Arc::new(JobStorePersistence {
                path,
                event_retention_limit,
                terminal_job_retention_limit,
                terminal_job_retention_bytes,
            })),
            daemon_lease_id: None,
            #[cfg(test)]
            terminal_write_failures: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            durable_write_failures: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            durable_write_skips: Arc::new(AtomicU64::new(0)),
        };
        store.durable_transaction(|_| Ok(()))?;
        Ok(store)
    }

    pub(crate) fn with_daemon_lease(mut self, daemon_lease_id: String) -> Self {
        self.daemon_lease_id = Some(daemon_lease_id);
        self
    }

    /// Snapshot-less job creation convenience. Production code creates jobs via
    /// [`JobStore::run_background_with_source_snapshot`] →
    /// [`JobStore::create_with_source_snapshot`]; this shorthand is only used by
    /// the store's unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn create(&self, operation: impl Into<String>) -> Job {
        self.create_with_source_snapshot(operation, None)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn create_with_source_snapshot(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
    ) -> Job {
        self.create_with_source_snapshot_metadata_and_path_materialization_plan(
            operation,
            source_snapshot,
            None,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn create_with_source_snapshot_and_metadata(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
    ) -> Job {
        self.create_with_source_snapshot_metadata_and_path_materialization_plan(
            operation,
            source_snapshot,
            metadata,
            None,
        )
    }

    pub(crate) fn create_with_source_snapshot_metadata_and_path_materialization_plan(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
    ) -> Job {
        self.create_with_source_snapshot_metadata_path_materialization_and_local_runner(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn create_test_local_runner_job(&self, local_runner: Option<LocalRunnerJob>) -> Job {
        self.create_with_source_snapshot_metadata_path_materialization_and_local_runner(
            "runner.exec",
            None,
            None,
            None,
            local_runner,
        )
    }

    fn create_with_source_snapshot_metadata_path_materialization_and_local_runner(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
        local_runner: Option<LocalRunnerJob>,
    ) -> Job {
        self.create_or_reuse_active_local_runner_job(LocalRunnerJobRequest {
            operation: operation.into(),
            source_snapshot,
            metadata,
            path_materialization_plan,
            local_runner,
            admission_idempotency_key: None,
        })
        .0
    }

    /// Create or renew a caller-owned admission under one store lock. Replays
    /// renew only the live reservation; terminal identities are never revived.
    pub(crate) fn create_or_renew_admission_at(
        &self,
        metadata: Value,
        idempotency_key: &str,
        now: u64,
        workspace_claim_binding: Option<crate::workspace_claim::WorkspaceClaimBinding>,
        workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
    ) -> Result<AdmissionReservation> {
        self.durable_transaction(|inner| {
            if let Some(stored) = inner
                .jobs
                .values_mut()
                .filter(|stored| {
                    stored.admission_idempotency_key.as_deref() == Some(idempotency_key)
                })
                .min_by_key(|stored| (stored.job.created_at_ms, stored.job.id))
            {
                if stored.job.status.is_terminal() {
                    return Err(Error::validation_invalid_argument(
                        "idempotency_key",
                        "admission idempotency key belongs to a terminal reservation",
                        Some(idempotency_key.to_string()),
                        None,
                    ));
                }
                if stored
                    .local_runner
                    .as_ref()
                    .and_then(|runner| runner.workspace_claim_binding.as_ref())
                    != workspace_claim_binding.as_ref()
                {
                    return Err(Error::validation_invalid_argument(
                        "workspace_claim_binding",
                        "admission idempotency key belongs to a differently bound reservation",
                        Some(idempotency_key.to_string()),
                        None,
                    ));
                }
                if stored
                    .local_runner
                    .as_ref()
                    .and_then(|runner| runner.workspace_owner_lease.as_ref())
                    != workspace_owner_lease.as_ref()
                {
                    return Err(Error::validation_invalid_argument(
                        "workspace_owner_lease",
                        "admission idempotency key belongs to a differently leased reservation",
                        Some(idempotency_key.to_string()),
                        None,
                    ));
                }
                let lease = stored.admission_lease.as_mut().ok_or_else(|| {
                    Error::internal_unexpected("active admission reservation is missing its lease")
                })?;
                lease.expires_at_ms = now.saturating_add(ADMISSION_RESERVATION_LEASE_MS);
                lease.renewals = lease.renewals.saturating_add(1);
                stored.job.updated_at_ms = now;
                let reservation = AdmissionReservation {
                    job: stored.job.clone(),
                    token: lease.token.clone(),
                    expires_at_ms: lease.expires_at_ms,
                    created: false,
                    workspace_owner_lease: stored
                        .local_runner
                        .as_ref()
                        .and_then(|runner| runner.workspace_owner_lease.clone()),
                };
                return Ok(reservation);
            }
            self.create_admission_inner(
                inner,
                metadata,
                Some(idempotency_key.to_string()),
                now,
                workspace_claim_binding,
                workspace_owner_lease,
            )
        })
    }

    pub(crate) fn create_admission_at(
        &self,
        metadata: Value,
        now: u64,
        workspace_claim_binding: Option<crate::workspace_claim::WorkspaceClaimBinding>,
        workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
    ) -> Result<AdmissionReservation> {
        self.durable_transaction(|inner| {
            self.create_admission_inner(
                inner,
                metadata,
                None,
                now,
                workspace_claim_binding,
                workspace_owner_lease,
            )
        })
    }

    /// Legacy, tokenless admission used only by pre-lease protocol clients
    /// during a rolling daemon upgrade.
    pub(crate) fn create_or_reuse_active_admission(
        &self,
        metadata: Value,
        idempotency_key: &str,
    ) -> (Job, bool) {
        self.create_or_reuse_active_local_runner_job(LocalRunnerJobRequest {
            operation: "runner.admission".to_string(),
            source_snapshot: None,
            metadata: Some(metadata),
            path_materialization_plan: None,
            local_runner: None,
            admission_idempotency_key: Some(idempotency_key.to_string()),
        })
    }

    fn create_admission_inner(
        &self,
        inner: &mut JobStoreInner,
        metadata: Value,
        idempotency_key: Option<String>,
        now: u64,
        workspace_claim_binding: Option<crate::workspace_claim::WorkspaceClaimBinding>,
        workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
    ) -> Result<AdmissionReservation> {
        let (job, created) = self.create_or_reuse_active_local_runner_job_inner(
            inner,
            LocalRunnerJobRequest {
                operation: "runner.admission".to_string(),
                source_snapshot: None,
                metadata: Some(metadata),
                path_materialization_plan: None,
                local_runner: Some(LocalRunnerJob {
                    runner_id: "admission".to_string(),
                    command: Vec::new(),
                    cwd: None,
                    lifecycle: None,
                    workspace_claim_binding,
                    workspace_owner_lease: workspace_owner_lease.clone(),
                }),
                admission_idempotency_key: idempotency_key.clone(),
            },
        )?;
        let stored = inner.jobs.get_mut(&job.id).expect("admission exists");
        if !created {
            let (token, expires_at_ms) = {
                let lease = stored.admission_lease.as_mut().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "idempotency_key",
                        "admission idempotency key belongs to a legacy reservation",
                        idempotency_key.clone(),
                        None,
                    )
                })?;
                lease.expires_at_ms = now.saturating_add(ADMISSION_RESERVATION_LEASE_MS);
                lease.renewals = lease.renewals.saturating_add(1);
                (lease.token.clone(), lease.expires_at_ms)
            };
            stored.job.updated_at_ms = now;
            let reservation = AdmissionReservation {
                job: stored.job.clone(),
                token,
                expires_at_ms,
                created: false,
                workspace_owner_lease,
            };
            return Ok(reservation);
        }
        let lease = AdmissionLease {
            token: Uuid::new_v4().to_string(),
            expires_at_ms: now.saturating_add(ADMISSION_RESERVATION_LEASE_MS),
            renewals: 0,
        };
        stored.admission_lease = Some(lease.clone());
        Ok(AdmissionReservation {
            job,
            token: lease.token,
            expires_at_ms: lease.expires_at_ms,
            created,
            workspace_owner_lease,
        })
    }

    /// Commit an admission renewal and its exact direct workspace authority in
    /// one durable-store transaction. The daemon renews the owner lease first;
    /// callers must retain the returned lease as the only valid cleanup token.
    pub(crate) fn renew_admission_with_workspace_owner_lease_at(
        &self,
        job_id: Uuid,
        token: &str,
        expected_owner_lease: Option<&crate::workspace_claim::WorkspaceOwnerLease>,
        renewed_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
        now: u64,
    ) -> Result<AdmissionReservation> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let current_owner_lease = stored.local_runner.as_ref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not an admission reservation",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if current_owner_lease.workspace_owner_lease.as_ref() != expected_owner_lease {
                return Err(Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    "admission renewal lease does not match the durable reservation",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            let (reservation_token, expires_at_ms) = {
                let lease = Self::admission_lease_for_live_job(stored, token, now)?;
                lease.expires_at_ms = now.saturating_add(ADMISSION_RESERVATION_LEASE_MS);
                lease.renewals = lease.renewals.saturating_add(1);
                (lease.token.clone(), lease.expires_at_ms)
            };
            stored.job.updated_at_ms = now;
            stored
                .local_runner
                .as_mut()
                .expect("admission local runner was checked")
                .workspace_owner_lease = renewed_owner_lease.clone();
            Ok(AdmissionReservation {
                job: stored.job.clone(),
                token: reservation_token,
                expires_at_ms,
                created: false,
                workspace_owner_lease: renewed_owner_lease,
            })
        })
    }

    /// Check the exact live admission capability and owner lease before an
    /// authority renewal. The replacement transaction below repeats this check
    /// as its CAS predicate because another request may win in the meantime.
    pub(crate) fn authorize_admission_renewal_with_workspace_owner_lease_at(
        &self,
        job_id: Uuid,
        token: &str,
        expected_owner_lease: Option<&crate::workspace_claim::WorkspaceOwnerLease>,
        now: u64,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        let current_owner_lease = stored.local_runner.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "job_id",
                "job is not an admission reservation",
                Some(job_id.to_string()),
                None,
            )
        })?;
        if current_owner_lease.workspace_owner_lease.as_ref() != expected_owner_lease {
            return Err(Error::validation_invalid_argument(
                "workspace_owner_lease",
                "admission renewal lease does not match the durable reservation",
                Some(job_id.to_string()),
                None,
            ));
        }
        Self::admission_lease_for_live_job(stored, token, now)?;
        Ok(())
    }

    /// Fail closed when authority renewal succeeded but its replacement lease
    /// could not be durably recorded. The expected lease keeps a concurrent
    /// successful renewal from being overwritten or terminalized.
    pub(crate) fn fail_admission_renewal_after_owner_replacement_failure(
        &self,
        job_id: Uuid,
        token: &str,
        expected_owner_lease: Option<&crate::workspace_claim::WorkspaceOwnerLease>,
        error: &Error,
        now: u64,
    ) -> Result<()> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let current_owner_lease = stored.local_runner.as_ref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not an admission reservation",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if current_owner_lease.workspace_owner_lease.as_ref() != expected_owner_lease {
                return Err(Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    "admission renewal lease no longer matches the durable reservation",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            Self::admission_lease_for_live_job(stored, token, now)?;
            stored
                .local_runner
                .as_mut()
                .expect("admission local runner was checked")
                .workspace_owner_lease = None;
            Self::append_event_already_locked(
                self,
                inner,
                job_id,
                JobEventKind::Error,
                Some("workspace owner lease renewal could not be persisted".to_string()),
                Some(serde_json::json!({
                    "schema": "homeboy/workspace-owner-lease-renewal-failure/v1",
                    "error_code": error.code.as_str(),
                    "error": error.to_string(),
                })),
            )?;
            let stored = inner.jobs.get_mut(&job_id).expect("admission job exists");
            stored.job.status = JobStatus::Failed;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason =
                Some("workspace owner lease renewal could not be persisted".to_string());
            Ok(())
        })
    }

    pub(crate) fn release_admission_at(&self, job_id: Uuid, token: &str, now: u64) -> Result<Job> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let lease = stored.admission_lease.as_ref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not an admission reservation",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if lease.token != token {
                return Err(Error::validation_invalid_argument(
                    "admission_token",
                    "admission reservation token does not match",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if !stored.job.status.is_terminal() {
                stored.job.status = JobStatus::Cancelled;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = Some("admission reservation released".to_string());
            }
            let job = stored.job.clone();
            Ok(job)
        })
    }

    pub(crate) fn admission_is_leased(&self, job_id: Uuid) -> Result<bool> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        Ok(stored.admission_lease.is_some())
    }

    pub(crate) fn admission_workspace_claim_binding(
        &self,
        job_id: Uuid,
    ) -> Result<Option<crate::workspace_claim::WorkspaceClaimBinding>> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if stored.job.operation != "runner.admission" {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "job is not an admission reservation",
                Some(job_id.to_string()),
                None,
            ));
        }
        Ok(stored
            .local_runner
            .as_ref()
            .and_then(|runner| runner.workspace_claim_binding.clone()))
    }

    pub(crate) fn admission_workspace_owner_lease(
        &self,
        job_id: Uuid,
    ) -> Result<Option<crate::workspace_claim::WorkspaceOwnerLease>> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if stored.job.operation != "runner.admission" {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "job is not an admission reservation",
                Some(job_id.to_string()),
                None,
            ));
        }
        Ok(stored
            .local_runner
            .as_ref()
            .and_then(|runner| runner.workspace_owner_lease.clone()))
    }

    pub(crate) fn reconcile_expired_admissions_at(&self, now: u64) -> Result<Vec<Uuid>> {
        self.durable_transaction(|inner| {
            let expired = inner
                .jobs
                .values_mut()
                .filter_map(|stored| {
                    let lease = stored.admission_lease.as_ref()?;
                    if !stored.job.status.is_terminal() && lease.expires_at_ms <= now {
                        stored.job.status = JobStatus::Failed;
                        stored.job.updated_at_ms = now;
                        stored.job.finished_at_ms = Some(now);
                        stored.job.stale_reason =
                            Some("admission reservation lease expired".to_string());
                        Some(stored.job.id)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            Ok(expired)
        })
    }

    pub(crate) fn reconcile_expired_admissions(&self) -> Result<Vec<Uuid>> {
        self.reconcile_expired_admissions_at(timestamp_ms())
    }

    /// Insert a new queued local-runner job, or reuse the existing non-terminal
    /// job for the same controller-minted `durable_run_id`.
    ///
    /// The dedup lookup and the insert happen under one lock, so two
    /// near-simultaneous first submissions of the same durable run id cannot
    /// both create a job — the enqueue-time race the daemon's transport-layer
    /// idempotency check cannot close. Returns `(job, created)`; `created` is
    /// `false` when an existing active job was reused, letting the caller skip
    /// spawning a duplicate worker for it.
    fn create_or_reuse_active_local_runner_job(
        &self,
        request: LocalRunnerJobRequest,
    ) -> (Job, bool) {
        self.durable_transaction(|inner| {
            self.create_or_reuse_active_local_runner_job_inner(inner, request)
        })
        .expect("local runner job creation must persist")
    }

    fn create_or_reuse_active_local_runner_job_inner(
        &self,
        inner: &mut JobStoreInner,
        request: LocalRunnerJobRequest,
    ) -> Result<(Job, bool)> {
        let now = timestamp_ms();
        let LocalRunnerJobRequest {
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            local_runner,
            admission_idempotency_key,
        } = request;
        let runner_job_projection = metadata
            .as_ref()
            .and_then(|metadata| metadata.get("runner_job_projection"))
            .cloned()
            .and_then(|projection| serde_json::from_value::<RunnerJobProjection>(projection).ok());
        let durable_run_id = local_runner
            .as_ref()
            .and_then(|local| local.lifecycle.as_ref())
            .and_then(|lifecycle| lifecycle.durable_run_id.clone())
            .filter(|run_id| !run_id.trim().is_empty());
        let job = Job {
            id: Uuid::new_v4(),
            operation: operation.clone(),
            status: JobStatus::Queued,
            created_at_ms: now,
            updated_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
            event_count: 0,
            source_snapshot,
            path_materialization_plan,
            stale_reason: None,
            daemon_lease_id: self.daemon_lease_id.clone(),
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection,
        };

        if let Some(idempotency_key) = admission_idempotency_key.as_deref() {
            if let Some(existing) = inner
                .jobs
                .values()
                .filter(|stored| {
                    matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                })
                .filter(|stored| {
                    (local_runner.is_none() || stored.job.operation != "runner.admission")
                        && stored.admission_idempotency_key.as_deref() == Some(idempotency_key)
                })
                .min_by_key(|stored| (stored.job.created_at_ms, stored.job.id))
                .map(|stored| stored.job.clone())
            {
                return Ok((existing, false));
            }
        } else if let Some(durable_run_id) = durable_run_id.as_deref() {
            if let Some(existing) = inner
                .jobs
                .values()
                .filter(|stored| {
                    matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                })
                .filter(|stored| {
                    stored_job_durable_run_id(stored).as_deref() == Some(durable_run_id)
                })
                .min_by_key(|stored| (stored.job.created_at_ms, stored.job.id))
                .map(|stored| stored.job.clone())
            {
                return Ok((existing, false));
            }
        }
        // Admissions carry LocalRunnerJob solely to persist direct workspace
        // authority. Only a real runner execution consumes an admission.
        let consumes_admission = local_runner.is_some() && operation != "runner.admission";
        inner.jobs.insert(
            job.id,
            StoredJob {
                job: job.clone(),
                events: Vec::new(),
                admission_idempotency_key: admission_idempotency_key.clone(),
                controller_job: None,
                admission_lease: None,
                remote_runner: None,
                local_runner,
                local_child: None,
            },
        );
        if let (Some(idempotency_key), true) =
            (admission_idempotency_key.as_deref(), consumes_admission)
        {
            for stored in inner.jobs.values_mut() {
                if stored.job.operation == "runner.admission"
                    && matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                    && stored.admission_idempotency_key.as_deref() == Some(idempotency_key)
                {
                    stored.job.status = JobStatus::Cancelled;
                    stored.job.updated_at_ms = now;
                    stored.job.finished_at_ms = Some(now);
                    stored.job.stale_reason =
                        Some("admission reservation consumed by runner execution".to_string());
                }
            }
        }
        let data = metadata.unwrap_or_else(|| serde_json::json!({ "status": JobStatus::Queued }));
        Self::append_event_already_locked(
            self,
            inner,
            job.id,
            JobEventKind::Status,
            Some("job queued".to_string()),
            Some(data),
        )?;
        Ok((
            inner
                .jobs
                .get(&job.id)
                .expect("newly-created job exists")
                .job
                .clone(),
            true,
        ))
    }

    fn admission_lease_for_live_job<'a>(
        stored: &'a mut StoredJob,
        token: &str,
        now: u64,
    ) -> Result<&'a mut AdmissionLease> {
        let lease = stored.admission_lease.as_mut().ok_or_else(|| {
            Error::validation_invalid_argument(
                "job_id",
                "job is not an admission reservation",
                Some(stored.job.id.to_string()),
                None,
            )
        })?;
        if lease.token != token {
            return Err(Error::validation_invalid_argument(
                "admission_token",
                "admission reservation token does not match",
                Some(stored.job.id.to_string()),
                None,
            ));
        }
        if stored.job.status.is_terminal() || lease.expires_at_ms <= now {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "admission reservation is terminal or expired",
                Some(stored.job.id.to_string()),
                None,
            ));
        }
        Ok(lease)
    }

    pub fn get(&self, job_id: Uuid) -> Result<Job> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        Ok(stored.job.clone())
    }

    pub(crate) fn handle(&self, job_id: Uuid) -> JobHandle {
        JobHandle {
            store: self.clone(),
            job_id,
        }
    }

    pub(crate) fn list(&self) -> Vec<Job> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut jobs: Vec<Job> = inner
            .jobs
            .values()
            .map(|stored| stored.job.clone())
            .collect();
        jobs.sort_by_key(|job| (job.created_at_ms, job.id));
        jobs
    }

    pub fn events(&self, job_id: Uuid) -> Result<Vec<JobEvent>> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        Ok(stored.events.clone())
    }

    pub fn start(&self, job_id: Uuid) -> Result<Job> {
        self.transition(job_id, JobStatus::Running, "job started")
    }

    pub(crate) fn complete(&self, job_id: Uuid, result: Option<Value>) -> Result<Job> {
        self.ensure_transition(job_id, JobStatus::Succeeded)?;
        if let Some(data) = result {
            self.append_event(job_id, JobEventKind::Result, None, Some(data))?;
        }
        self.transition(job_id, JobStatus::Succeeded, "job succeeded")
    }

    pub(crate) fn fail(&self, job_id: Uuid, error: impl Into<String>) -> Result<Job> {
        self.fail_with_data(job_id, error, None)
    }

    pub(crate) fn fail_with_data(
        &self,
        job_id: Uuid,
        error: impl Into<String>,
        data: Option<Value>,
    ) -> Result<Job> {
        self.ensure_transition(job_id, JobStatus::Failed)?;
        let error = error.into();
        self.append_event(job_id, JobEventKind::Error, Some(error.clone()), data)?;
        self.transition(job_id, JobStatus::Failed, error)
    }

    pub fn cancel(&self, job_id: Uuid, reason: impl Into<String>) -> Result<Job> {
        if self.controller_job_state(job_id).is_ok() {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "controller jobs must be cancelled through POST /controller/jobs/{id}/cancel so the controller driver can stop owned work",
                Some(job_id.to_string()),
                Some(vec![format!("POST /controller/jobs/{job_id}/cancel")]),
            ));
        }
        self.transition(job_id, JobStatus::Cancelled, reason.into())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_durable_writes(&self, count: u64) {
        self.durable_write_failures.store(count, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn skip_next_durable_writes(&self, count: u64) {
        self.durable_write_skips.store(count, Ordering::SeqCst);
    }

    pub(crate) fn controller_job_state(&self, job_id: Uuid) -> Result<ControllerJobState> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?
            .controller_job
            .clone()
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not a controller job",
                    Some(job_id.to_string()),
                    None,
                )
            })
    }

    pub(crate) fn request_controller_cancellation(
        &self,
        job_id: Uuid,
        reason: String,
    ) -> Result<Job> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let controller = stored.controller_job.as_mut().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not a controller job",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if stored.job.status == JobStatus::Cancelled || controller.cancellation_requested {
                return Ok(stored.job.clone());
            }
            if stored.job.status.is_terminal() {
                return Err(Error::validation_invalid_argument(
                    "status",
                    "cannot cancel a terminal controller job that did not stop by cancellation",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            // Persist the intent before releasing this lock. Dispatchers only observe
            // cancellation through this durable state, never through a transient flag.
            controller.cancellation_requested = true;
            controller.cancellation_reason = Some(reason.clone());
            let claimless = stored.job.status == JobStatus::Queued
                && controller.execution_claim_id.is_none()
                && controller.checkpoint.is_none();
            let now = timestamp_ms();
            stored.job.updated_at_ms = now;
            if claimless {
                // A response-handoff failure leaves no worker to observe this request.
                // Terminalize while holding the claim lock so dispatch cannot start it.
                stored.job.status = JobStatus::Cancelled;
                stored.job.finished_at_ms = Some(now);
            }
            if claimless {
                Self::append_event_already_locked(
                    self,
                    inner,
                    job_id,
                    JobEventKind::Status,
                    Some(reason),
                    Some(serde_json::json!({ "status": JobStatus::Cancelled })),
                )?;
            } else {
                Self::append_event_already_locked(
                    self,
                    inner,
                    job_id,
                    JobEventKind::Progress,
                    Some("controller job cancellation requested".to_string()),
                    Some(
                        serde_json::json!({ "phase": "cancellation_requested", "reason": reason }),
                    ),
                )?;
            }
            Ok(inner
                .jobs
                .get(&job_id)
                .expect("controller job exists")
                .job
                .clone())
        })
    }

    pub(crate) fn controller_cancellation_requested(&self, job_id: Uuid) -> bool {
        self.inner
            .lock()
            .expect("job store mutex poisoned")
            .jobs
            .get(&job_id)
            .and_then(|stored| stored.controller_job.as_ref())
            .is_some_and(|controller| controller.cancellation_requested)
    }

    pub(crate) fn record_controller_prepared(&self, job_id: Uuid, prepared: Value) -> Result<()> {
        self.durable_transaction(|inner| {
            let controller = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?
                .controller_job
                .as_mut()
                .ok_or_else(|| {
                    Error::internal_unexpected("controller worker lost its typed state")
                })?;
            controller.checkpoint = Some(prepared);
            Ok(())
        })
    }

    pub(crate) fn complete_controller_cancellation(&self, job_id: Uuid) -> Result<Job> {
        let state = self.controller_job_state(job_id)?;
        if !state.cancellation_requested {
            return Err(Error::internal_unexpected(
                "controller cancellation completed without a recorded request",
            ));
        }
        self.terminalize_controller_job(
            job_id,
            JobStatus::Cancelled,
            JobEventKind::Status,
            state
                .cancellation_reason
                .unwrap_or_else(|| "controller job cancellation completed".to_string()),
            serde_json::json!({ "status": JobStatus::Cancelled }),
        )
    }

    pub(crate) fn fail_controller_cancellation(
        &self,
        job_id: Uuid,
        message: String,
        data: Value,
    ) -> Result<Job> {
        self.terminalize_controller_job(
            job_id,
            JobStatus::Failed,
            JobEventKind::Error,
            message,
            serde_json::json!({ "phase": "cancellation_failed", "error": data }),
        )
    }

    pub(crate) fn fail_controller_error(
        &self,
        job_id: Uuid,
        message: String,
        data: Value,
    ) -> Result<Job> {
        self.terminalize_controller_job(
            job_id,
            JobStatus::Failed,
            JobEventKind::Error,
            message,
            data,
        )
    }

    /// Commit controller terminal evidence, status, and the idempotency projection
    /// in one durable write. Failed writes restore the entire in-memory snapshot.
    pub(crate) fn complete_controller_success(&self, job_id: Uuid, result: Value) -> Result<Job> {
        self.terminalize_controller_job(
            job_id,
            JobStatus::Succeeded,
            JobEventKind::Result,
            "job succeeded".to_string(),
            result,
        )
    }

    fn terminalize_controller_job(
        &self,
        job_id: Uuid,
        status: JobStatus,
        event_kind: JobEventKind,
        message: String,
        data: Value,
    ) -> Result<Job> {
        self.durable_transaction(|inner| {
            #[cfg(test)]
            if self
                .terminal_write_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(Error::internal_io(
                    "injected controller terminal persistence failure",
                    None,
                ));
            }
            let now = timestamp_ms();
            let first_sequence = self.next_event_sequence.fetch_add(2, Ordering::SeqCst) + 1;
            let job = {
                let stored = inner
                    .jobs
                    .get_mut(&job_id)
                    .ok_or_else(|| job_not_found(job_id))?;
                let controller = stored.controller_job.as_mut().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "job_id",
                        "job is not a controller job",
                        Some(job_id.to_string()),
                        None,
                    )
                })?;
                if status == JobStatus::Succeeded && controller.cancellation_requested {
                    return Err(Error::validation_invalid_argument(
                        "status",
                        "cannot complete a controller job after durable cancellation was requested",
                        Some(job_id.to_string()),
                        None,
                    ));
                }
                validate_transition(stored.job.status, status)?;
                stored.events.push(JobEvent {
                    sequence: first_sequence,
                    job_id,
                    kind: event_kind,
                    timestamp_ms: now,
                    message: Some(message.clone()),
                    data: Some(data),
                });
                stored.events.push(JobEvent {
                    sequence: first_sequence + 1,
                    job_id,
                    kind: JobEventKind::Status,
                    timestamp_ms: now,
                    message: Some(message),
                    data: Some(serde_json::json!({ "status": status })),
                });
                apply_event_retention(&mut stored.events, self.event_retention_limit());
                stored.job.event_count = stored.events.len();
                stored.job.status = status;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                controller.execution_claim_id = None;
                stored.job.clone()
            };
            for submission in inner.controller_submissions.values_mut() {
                if submission.job_id == job_id {
                    submission.terminal_job = Some(job.clone());
                }
            }
            Ok(job)
        })
    }

    pub(crate) fn append_event(
        &self,
        job_id: Uuid,
        kind: JobEventKind,
        message: Option<String>,
        data: Option<Value>,
    ) -> Result<JobEvent> {
        self.durable_transaction(|inner| {
            Self::append_event_already_locked(self, inner, job_id, kind, message, data)
        })
    }

    pub(crate) fn run_background<T, F>(&self, operation: impl Into<String>, run: F) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        self.run_background_with_source_snapshot(operation, None, run)
    }

    /// Persist a controller-owned typed envelope before spawning its driver.
    /// A key identifies the request permanently, including its terminal result.
    pub(crate) fn admit_controller_job(
        &self,
        operation: String,
        idempotency_key: String,
        controller_job: ControllerJobState,
    ) -> Result<ControllerJobSubmissionOutcome> {
        let fingerprint = controller_submission_fingerprint(&controller_job);
        let now = timestamp_ms();
        self.durable_transaction(|inner| {
        if let Some(existing) = inner.controller_submissions.get(&idempotency_key) {
            if existing.fingerprint != fingerprint {
                return Err(controller_idempotency_conflict(&idempotency_key));
            }
            let job = inner.jobs.get(&existing.job_id).ok_or_else(|| {
                Error::internal_unexpected("controller submission index points at a missing job")
            })?;
                return Ok(ControllerJobSubmissionOutcome::Existing(Box::new(job.job.clone())));
        }
        if inner
            .expired_controller_submissions
            .contains_key(&idempotency_key)
        {
            let submission = inner
                .expired_controller_submissions
                .get(&idempotency_key)
                .expect("submission exists");
            if submission.fingerprint != fingerprint {
                return Err(controller_idempotency_conflict(&idempotency_key));
            }
            if let Some(job) = submission.terminal_job.clone() {
                return Ok(ControllerJobSubmissionOutcome::Existing(Box::new(job)));
            }
            return Err(Error::validation_invalid_argument(
                "idempotency_key",
                "controller idempotency key belongs to compacted terminal work; choose a new key for a new attempt",
                Some(idempotency_key),
                None,
            ));
        }
        if let Some(tombstone) = self
            .persistence
            .as_ref()
            .map(|persistence| {
                lookup_tombstone(
                    &persistence.path,
                    ReplayTombstoneKind::Controller,
                    &idempotency_key,
                )
            })
            .transpose()?
            .flatten()
        {
            if tombstone.fingerprint != fingerprint {
                return Err(controller_idempotency_conflict(&idempotency_key));
            }
            if let Some(job) = tombstone.terminal_job {
                return Ok(ControllerJobSubmissionOutcome::Existing(Box::new(job)));
            }
            return Err(Error::validation_invalid_argument(
                "idempotency_key",
                "controller idempotency key belongs to compacted terminal work; choose a new key for a new attempt",
                Some(idempotency_key),
                None,
            ));
        }
        if let Some(active_idempotency_key) = controller_job.active_idempotency_key.as_deref() {
            let active = inner.jobs.values().find(|stored| {
                !stored.job.status.is_terminal()
                    && stored
                        .controller_job
                        .as_ref()
                        .and_then(|state| state.active_idempotency_key.as_deref())
                        == Some(active_idempotency_key)
            });
            if let Some(active) = active {
                let active_state = active
                    .controller_job
                    .as_ref()
                    .expect("active controller submission has controller state");
                if controller_submission_fingerprint(active_state) != fingerprint {
                    return Err(controller_idempotency_conflict(active_idempotency_key));
                }
                return Ok(ControllerJobSubmissionOutcome::Existing(Box::new(active.job.clone())));
            }
        }
        let job = Job {
            id: Uuid::new_v4(),
            operation,
            status: JobStatus::Queued,
            created_at_ms: now,
            updated_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: self.daemon_lease_id.clone(),
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };
        let job_id = job.id;
        let mut event_data = serde_json::json!({ "controller_job": controller_job.public_request });
        if let Some(run_id) = event_data["controller_job"]["durable_run_id"]
            .as_str()
            .filter(|run_id| !run_id.trim().is_empty())
        {
            event_data["durable_run_id"] = serde_json::json!(run_id);
        }
        let initial_event = JobEvent {
            sequence: self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1,
            job_id,
            kind: JobEventKind::Status,
            timestamp_ms: now,
            message: Some("job queued".to_string()),
            data: Some(event_data),
        };
        let mut stored = StoredJob {
            job,
            events: vec![initial_event],
            admission_idempotency_key: None,
            controller_job: Some(controller_job),
            admission_lease: None,
            remote_runner: None,
            local_runner: None,
            local_child: None,
        };
        stored.job.event_count = stored.events.len();
        inner.jobs.insert(job_id, stored);
        inner.controller_submissions.insert(
            idempotency_key.clone(),
            ControllerJobSubmission {
                fingerprint,
                job_id,
                terminal_job: None,
            },
        );
        Ok(ControllerJobSubmissionOutcome::Submitted(job_id))
        })
    }

    /// Claim a controller job before starting a worker. This is intentionally a
    /// durable transition: a failed HTTP response leaves a queued, claimless job.
    pub(crate) fn claim_controller_execution(
        &self,
        job_id: Uuid,
        recovery: bool,
    ) -> Result<ControllerJobState> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let controller = stored.controller_job.as_mut().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not a controller job",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if stored.job.status.is_terminal()
                || (!recovery && controller.execution_claim_id.is_some())
            {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "controller job is already claimed or terminal",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if recovery && (controller.checkpoint.is_none() || controller.recovery_attempted) {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "controller job has no recoverable checkpoint",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            controller.execution_claim_id = Some(Uuid::new_v4().to_string());
            controller.recovery_attempted |= recovery;
            stored.job.status = JobStatus::Running;
            stored.job.started_at_ms.get_or_insert_with(timestamp_ms);
            stored.job.updated_at_ms = timestamp_ms();
            let controller = controller.clone();
            Ok(controller)
        })
    }

    /// Atomically claim queued work for the explicit second phase of controller
    /// submission. A retry observes the durable current job rather than starting
    /// another worker.
    pub(crate) fn start_controller_execution(
        &self,
        job_id: Uuid,
    ) -> Result<ControllerJobStartOutcome> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let controller = stored.controller_job.as_mut().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not a controller job",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if stored.job.status != JobStatus::Queued || controller.execution_claim_id.is_some() {
                return Ok(ControllerJobStartOutcome::Existing);
            }
            controller.execution_claim_id = Some(Uuid::new_v4().to_string());
            stored.job.status = JobStatus::Running;
            stored.job.started_at_ms.get_or_insert_with(timestamp_ms);
            stored.job.updated_at_ms = timestamp_ms();
            let controller = controller.clone();
            Ok(ControllerJobStartOutcome::Claimed(controller))
        })
    }

    pub(crate) fn active_controller_jobs(&self) -> Vec<(Uuid, ControllerJobState)> {
        self.inner
            .lock()
            .expect("job store mutex poisoned")
            .jobs
            .values()
            .filter(|stored| stored.job.status == JobStatus::Running)
            .filter_map(|stored| {
                stored
                    .controller_job
                    .clone()
                    .map(|state| (stored.job.id, state))
            })
            .collect()
    }

    pub(crate) fn run_background_with_source_snapshot<T, F>(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        run: F,
    ) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        self.run_background_with_source_snapshot_metadata_and_path_materialization_plan(
            operation,
            source_snapshot,
            None,
            None,
            run,
        )
    }

    pub(crate) fn run_background_with_source_snapshot_metadata_and_path_materialization_plan<T, F>(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
        run: F,
    ) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        self.run_background_with_start_policy(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            true,
            run,
        )
    }

    /// Fallible variant used by the daemon request boundary so a failed durable
    /// queue commit can roll back a just-registered workspace owner.
    pub(crate) fn try_run_capacity_queued_local_child_background_with_source_snapshot_metadata_path_materialization_and_local_runner<
        T,
        F,
    >(
        &self,
        request: LocalRunnerJobRequest,
        capacity: usize,
        run: F,
    ) -> Result<(JobRunner, bool)>
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        let (job, created) = self.durable_transaction(|inner| {
            self.create_or_reuse_active_local_runner_job_inner(inner, request.clone())
        })?;
        let job_id = job.id;
        // An idempotent resubmission reused an already-enqueued job that already
        // has its own worker. Do not spawn a second worker for it — return a
        // handle to a thread that completes immediately so the caller's
        // `JobRunner` contract is preserved.
        if !created {
            let handle = thread::spawn(|| {});
            return Ok((JobRunner { job_id, handle }, false));
        }
        let handle_store = self.clone();
        let worker_store = self.clone();
        let handle = thread::spawn(move || {
            let job_handle = JobHandle {
                store: handle_store,
                job_id,
            };
            loop {
                if job_handle.is_cancelled() {
                    return;
                }
                match worker_store.reserve_local_child_with_runner_capacity(
                    job_id,
                    &request
                        .local_runner
                        .as_ref()
                        .expect("capacity admission requires a local runner")
                        .runner_id,
                    capacity,
                ) {
                    Ok(true) => break,
                    Ok(false) => thread::sleep(std::time::Duration::from_millis(10)),
                    Err(_) => return,
                }
            }
            let _ = job_handle.progress(serde_json::json!({
                "phase": "local_child_worker_started",
            }));
            match run(job_handle) {
                Ok(output) => {
                    let _ = worker_store.complete(job_id, serde_json::to_value(output).ok());
                }
                Err(error) => {
                    if error.details["retain_active"].as_bool() == Some(true) {
                        return;
                    }
                    let error_message = error.to_string();
                    let failure_data = serde_json::json!({
                        "phase": "local_child_worker_failed_before_child_identity",
                        "error": error_message,
                        "error_code": error.code.as_str(),
                        "error_details": error.details,
                    });
                    if worker_store
                        .get(job_id)
                        .is_ok_and(|job| job.status == JobStatus::Queued)
                    {
                        let _ = worker_store.append_event(
                            job_id,
                            JobEventKind::Progress,
                            Some("local child worker failed before child identity".to_string()),
                            Some(failure_data.clone()),
                        );
                    }
                    let _ = worker_store.fail_with_data(job_id, error_message, Some(failure_data));
                }
            }
        });
        Ok((JobRunner { job_id, handle }, true))
    }

    fn run_background_with_start_policy<T, F>(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
        start_before_run: bool,
        run: F,
    ) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        let job = self.create_with_source_snapshot_metadata_and_path_materialization_plan(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
        );
        let job_id = job.id;
        let handle_store = self.clone();
        let worker_store = self.clone();

        let handle = thread::spawn(move || {
            if start_before_run && worker_store.start(job_id).is_err() {
                return;
            }
            let job_handle = JobHandle {
                store: handle_store,
                job_id,
            };

            match run(job_handle) {
                Ok(output) => {
                    let result = serde_json::to_value(output).ok();
                    let _ = worker_store.complete(job_id, result);
                }
                Err(err) => {
                    let _ = worker_store.fail(job_id, err.to_string());
                }
            }
        });

        JobRunner { job_id, handle }
    }

    pub(super) fn transition(
        &self,
        job_id: Uuid,
        next_status: JobStatus,
        message: impl Into<String>,
    ) -> Result<Job> {
        let message = message.into();
        let terminal_owner_lease = self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            // Cancellation may be retried after the daemon has already recorded it.
            // It is a no-op so callers receive the authoritative terminal job without
            // adding a duplicate cancellation event.
            if stored.job.status == JobStatus::Cancelled && next_status == JobStatus::Cancelled {
                let owner_lease = stored
                    .local_runner
                    .as_ref()
                    .and_then(|runner| runner.workspace_owner_lease.clone());
                return Ok((stored.job.clone(), owner_lease));
            }
            if next_status == JobStatus::Succeeded
                && stored
                    .controller_job
                    .as_ref()
                    .is_some_and(|controller| controller.cancellation_requested)
            {
                return Err(Error::validation_invalid_argument(
                    "status",
                    "cannot complete a controller job after durable cancellation was requested",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            validate_transition(stored.job.status, next_status)?;

            let now = timestamp_ms();
            stored.job.status = next_status;
            stored.job.updated_at_ms = now;
            if next_status == JobStatus::Cancelled {
                // Preserve the cancellation cause in the job projection as well
                // as the status event, so a drained generation remains diagnosable.
                stored.job.stale_reason = Some(message.clone());
            }
            if next_status == JobStatus::Running {
                stored.job.started_at_ms = Some(now);
            }
            if next_status.is_terminal() {
                stored.job.finished_at_ms = Some(now);
            }
            Self::append_event_already_locked(
                self,
                inner,
                job_id,
                JobEventKind::Status,
                Some(message),
                Some(serde_json::json!({ "status": next_status })),
            )?;
            let job = inner
                .jobs
                .get(&job_id)
                .map(|stored| stored.job.clone())
                .ok_or_else(|| job_not_found(job_id))?;
            let owner_lease = next_status
                .is_terminal()
                .then(|| {
                    inner.jobs.get(&job_id).and_then(|stored| {
                        stored
                            .local_runner
                            .as_ref()
                            .and_then(|runner| runner.workspace_owner_lease.clone())
                    })
                })
                .flatten();
            Ok((job, owner_lease))
        })?;
        // The terminal job is already durable. Cleanup is intentionally best
        // effort so an authority-store I/O failure cannot hide its outcome.
        if let Some(lease) = terminal_owner_lease.1 {
            // Release through the exact factory that registered the lease.
            // `JobStore` carries no data root -- its `persistence.path` hangs
            // off the daemon *config* root (`daemon_jobs_file`), not the data
            // root the claim authority lives under -- so this reach stays
            // ambient. It must at least not rebuild the store name by hand:
            // a duplicated literal here is a release addressing a store the
            // register never wrote to.
            if let Ok(store) = crate::daemon::daemon_workspace_claim_store() {
                if let Err(error) = store.release_owner(&lease, timestamp_ms()) {
                    let _ = self.record_workspace_owner_cleanup_failure(job_id, &error);
                }
            }
        }
        Ok(terminal_owner_lease.0)
    }

    /// Exact direct-owner leases attached to terminal records. Keeping them
    /// durable makes restart cleanup idempotent and auditable.
    pub(crate) fn terminal_workspace_owner_leases(
        &self,
    ) -> Vec<(Uuid, crate::workspace_claim::WorkspaceOwnerLease)> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .values()
            .filter(|stored| stored.job.status.is_terminal())
            .filter_map(|stored| {
                stored
                    .local_runner
                    .as_ref()
                    .and_then(|runner| runner.workspace_owner_lease.clone())
                    .map(|lease| (stored.job.id, lease))
            })
            .collect()
    }

    /// A release failure is evidence on the terminal job, never a replacement
    /// for that job's already-durable outcome.
    pub(crate) fn record_workspace_owner_cleanup_failure(
        &self,
        job_id: Uuid,
        error: &Error,
    ) -> Result<()> {
        self.append_terminal_evidence(
            job_id,
            JobEventKind::Progress,
            Some("workspace owner lease cleanup failed".to_string()),
            Some(serde_json::json!({
                "schema": "homeboy/workspace-owner-lease-cleanup/v1",
                "outcome_preserved": true,
                "error": error.to_string(),
                "error_code": error.code.as_str(),
            })),
        )
        .map(|_| ())
    }

    /// Replace a direct job's exact owner token in the same durable record that
    /// terminal cleanup reads. Authority renewal without this write is unsafe:
    /// a restart would otherwise retain only a stale epoch.
    pub(crate) fn replace_local_runner_workspace_owner_lease(
        &self,
        job_id: Uuid,
        expected: &crate::workspace_claim::WorkspaceOwnerLease,
        renewed: crate::workspace_claim::WorkspaceOwnerLease,
    ) -> Result<()> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            if stored.job.status.is_terminal() {
                return Err(Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    "cannot renew workspace authority for a terminal job",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            let local = stored.local_runner.as_mut().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    "job has no direct workspace owner lease",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if local.workspace_owner_lease.as_ref() != Some(expected) {
                return Err(Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    "renewed workspace authority no longer matches the durable job lease",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            local.workspace_owner_lease = Some(renewed);
            stored.job.updated_at_ms = timestamp_ms();
            Ok(())
        })
    }

    /// Reached only from the `#[cfg(test)]` reconciliation paths in
    /// `store::reconciliation`; production appends terminal evidence through
    /// `append_status_event_with_data_at`. Kept because those tests assert real
    /// reconciliation behavior, suppressed per item so the next unused method in
    /// this impl still fails the build.
    #[allow(
        dead_code,
        reason = "Reached only from cfg(test) reconciliation paths that assert real behavior."
    )]
    pub(super) fn append_status_event_with_data(
        &self,
        job_id: Uuid,
        status: JobStatus,
        message: impl Into<String>,
        mut data: Value,
    ) -> Result<JobEvent> {
        if !data.is_object() {
            data = serde_json::json!({ "metadata": data });
        }
        if let Some(object) = data.as_object_mut() {
            object.insert("status".to_string(), serde_json::json!(status));
        }
        self.append_event(
            job_id,
            JobEventKind::Status,
            Some(message.into()),
            Some(data),
        )
    }

    pub(crate) fn reserve_local_child_with_runner_capacity(
        &self,
        job_id: Uuid,
        runner_id: &str,
        capacity: usize,
    ) -> Result<bool> {
        self.reserve_local_child_at_with_runner_capacity(
            job_id,
            timestamp_ms(),
            Some((runner_id, capacity)),
        )
    }

    pub(crate) fn reserve_local_child_at_with_runner_capacity(
        &self,
        job_id: Uuid,
        now: u64,
        runner_capacity: Option<(&str, usize)>,
    ) -> Result<bool> {
        let reservation_id = Uuid::new_v4().to_string();
        let reservation_expires_at_ms = now.saturating_add(LOCAL_CHILD_RESERVATION_LEASE_MS);
        self.durable_transaction(|inner| {
            inner
                .jobs
                .get(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            if let Some((runner_id, capacity)) = runner_capacity {
                let active = inner.jobs.values().filter(|candidate| {
                    candidate.job.id != job_id
                        && matches!(candidate.job.status, JobStatus::Queued | JobStatus::Running)
                        && candidate.local_child.is_some()
                        && candidate
                            .local_runner
                            .as_ref()
                            .is_some_and(|runner| runner.runner_id == runner_id)
                });
                if active.count() >= capacity {
                    return Ok(false);
                }
            }
            let stored = inner.jobs.get_mut(&job_id).expect("job exists");
            if stored.job.status != JobStatus::Queued {
                return Err(Error::validation_invalid_argument(
                    "status",
                    "local child reservation requires a queued job",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            stored.local_child = Some(LocalChildExecution {
                reservation_id: reservation_id.clone(),
                reservation_expires_at_ms: Some(reservation_expires_at_ms),
                process: None,
            });
            let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
            stored.events.push(JobEvent {
                sequence,
                job_id,
                kind: JobEventKind::Progress,
                timestamp_ms: now,
                message: Some("local runner child reserved before spawn".to_string()),
                data: Some(serde_json::json!({
                    "phase": "child_reserved",
                    "reservation_id": reservation_id,
                    "reservation_expires_at_ms": reservation_expires_at_ms,
                })),
            });
            stored.job.event_count = stored.events.len();
            Ok(true)
        })
    }

    /// Terminalize expired pre-spawn reservations. A PID-bound child has
    /// atomically claimed the reservation and is intentionally left to normal
    /// child liveness recovery, even when the original admission deadline has
    /// passed.
    pub(crate) fn reconcile_expired_local_child_reservations(&self) -> Result<Vec<Uuid>> {
        self.reconcile_expired_local_child_reservations_at(timestamp_ms())
    }

    pub(crate) fn reconcile_expired_local_child_reservations_at(
        &self,
        now: u64,
    ) -> Result<Vec<Uuid>> {
        self.durable_transaction(|inner| {
            let expired = inner
                .jobs
                .values()
                .filter(|stored| {
                    stored.job.status == JobStatus::Queued
                        && stored.local_child.as_ref().is_some_and(|child| {
                            child.process.is_none()
                                && child
                                    .reservation_expires_at_ms
                                    .is_some_and(|expires_at| expires_at <= now)
                        })
                })
                .map(|stored| stored.job.id)
                .collect::<Vec<_>>();

            for job_id in &expired {
                let stored = inner.jobs.get_mut(job_id).expect("expired job exists");
                let child = stored
                    .local_child
                    .as_ref()
                    .expect("expired reservation exists");
                let reason = "local child reservation lease expired before spawn";
                stored.job.status = JobStatus::Failed;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = Some(reason.to_string());
                let terminal_result = serde_json::json!({
                    "status": JobStatus::Failed,
                    "reason": "local_child_reservation_expired",
                    "retryable": true,
                    "reservation_id": child.reservation_id,
                    "reservation_expires_at_ms": child.reservation_expires_at_ms,
                });
                for (kind, message, data) in [
                    (
                        JobEventKind::Error,
                        reason.to_string(),
                        terminal_result.clone(),
                    ),
                    (
                        JobEventKind::Result,
                        "retryable terminal reservation failure".to_string(),
                        terminal_result.clone(),
                    ),
                    (
                        JobEventKind::Status,
                        "job marked failed after local child reservation lease expiry".to_string(),
                        terminal_result,
                    ),
                ] {
                    let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                    stored.events.push(JobEvent {
                        sequence,
                        job_id: *job_id,
                        kind,
                        timestamp_ms: now,
                        message: Some(message),
                        data: Some(data),
                    });
                }
                apply_event_retention(&mut stored.events, self.event_retention_limit());
                stored.job.event_count = stored.events.len();
            }
            Ok(expired)
        })
    }

    /// Explicit, per-job legacy recovery. The supplied PID/start ticks must
    /// prove the recorded process is gone or has been reused before this can
    /// attach the recovered identity and terminalize the interrupted job.
    pub fn recover_missing_child_identity_with_linux_evidence(
        &self,
        expected_lease_id: &str,
        job_id: Uuid,
        pid: u32,
        expected_starttime_ticks: u64,
    ) -> Result<Job> {
        let existing = self.get(job_id)?;
        if existing.daemon_lease_id.as_deref() != Some(expected_lease_id) {
            return Err(Error::validation_invalid_argument(
                "lease_id",
                "job is not owned by the expected daemon lease",
                Some(job_id.to_string()),
                None,
            ));
        }
        if existing.status.is_terminal() {
            let exact = self.events(job_id)?.iter().any(|event| {
                event.data.as_ref().is_some_and(|data| {
                    data["reason"] == "operator_legacy_child_identity_recovery"
                        && data["expected_lease_id"] == expected_lease_id
                        && data["process"]["root_pid"] == pid
                        && data["process"]["linux_starttime_ticks"] == expected_starttime_ticks
                })
            });
            return if exact {
                Ok(existing)
            } else {
                Err(Error::validation_invalid_argument(
                    "job_id",
                    "legacy recovery replay evidence conflicts with the recorded terminal recovery",
                    Some(job_id.to_string()),
                    None,
                ))
            };
        }
        match crate::process::linux_process_starttime_ticks(pid) {
            Ok(Some(actual)) if actual == expected_starttime_ticks => {
                return Err(Error::validation_invalid_argument(
                    "child_pid",
                    "operator-supplied child identity is still live; refusing recovery",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            Ok(_) => {}
            Err(evidence) => {
                return Err(Error::validation_invalid_argument(
                    "child_starttime_ticks",
                    format!("cannot verify Linux child identity: {evidence}"),
                    Some(job_id.to_string()),
                    Some(vec![
                        "Run this recovery on the Linux host that owned the child process."
                            .to_string(),
                    ]),
                ));
            }
        }
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            if !matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                || stored.local_child.is_some()
            {
                return Err(Error::validation_invalid_argument(
                "job_id",
                "legacy recovery requires one active job with no persisted local child identity",
                Some(job_id.to_string()),
                None,
            ));
            }
            let now = timestamp_ms();
            stored.local_child = Some(LocalChildExecution {
                reservation_id: format!("operator-recovery-{job_id}"),
                reservation_expires_at_ms: None,
                process: Some(LocalChildProcessIdentity {
                    pid,
                    process_group_id: None,
                    discriminator: LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks {
                        ticks: expected_starttime_ticks,
                    },
                }),
            });
            stored.job.status = JobStatus::Failed;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason =
                Some("operator-proven legacy child identity was absent or reused".to_string());
            let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
            stored.events.push(JobEvent {
            sequence,
            job_id,
            kind: JobEventKind::Status,
            timestamp_ms: now,
            message: Some(
                "job marked failed from operator-supplied legacy child evidence".to_string(),
            ),
            data: Some(serde_json::json!({
                "status": JobStatus::Failed,
                "reason": "operator_legacy_child_identity_recovery",
                "expected_lease_id": expected_lease_id,
                "process": { "root_pid": pid, "linux_starttime_ticks": expected_starttime_ticks },
            })),
        });
            stored.job.event_count = stored.events.len();
            Ok(inner
                .jobs
                .get(&job_id)
                .expect("recovered job exists")
                .job
                .clone())
        })
    }

    pub(crate) fn start_with_reserved_child_identity(
        &self,
        job_id: Uuid,
        pid: u32,
        process_group_id: Option<u32>,
        discriminator: LocalChildStartDiscriminator,
    ) -> Result<Job> {
        self.durable_transaction(|inner| {
        let started = {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            validate_transition(stored.job.status, JobStatus::Running)?;
            let local_child = stored.local_child.as_mut().ok_or_else(|| {
                Error::internal_unexpected("local child spawned without a durable reservation")
            })?;
            local_child.process = Some(LocalChildProcessIdentity {
                pid,
                process_group_id,
                discriminator: discriminator.clone(),
            });
            let now = timestamp_ms();
            stored.job.status = JobStatus::Running;
            stored.job.started_at_ms = Some(now);
            stored.job.updated_at_ms = now;
            let sequence = self.next_event_sequence.fetch_add(2, Ordering::SeqCst) + 1;
            stored.events.push(JobEvent {
                sequence,
                job_id,
                kind: JobEventKind::Progress,
                timestamp_ms: now,
                message: Some("runner child identity persisted".to_string()),
                data: Some(serde_json::json!({ "phase": "spawned", "process": { "root_pid": pid, "process_group_id": process_group_id, "start_discriminator": discriminator } })),
            });
            stored.events.push(JobEvent {
                sequence: sequence + 1,
                job_id,
                kind: JobEventKind::Status,
                timestamp_ms: now,
                message: Some("job started".to_string()),
                data: Some(serde_json::json!({ "status": JobStatus::Running })),
            });
            apply_event_retention(&mut stored.events, self.event_retention_limit());
            stored.job.event_count = stored.events.len();
            stored.job.clone()
        };
        Ok(started)
        })
    }

    pub(super) fn ensure_transition(&self, job_id: Uuid, next_status: JobStatus) -> Result<()> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        validate_transition(stored.job.status, next_status)
    }

    pub(super) fn event_retention_limit(&self) -> usize {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.event_retention_limit)
            .unwrap_or(usize::MAX)
    }
}

impl JobStore {
    /// Append durable evidence after a terminal outcome without changing that
    /// outcome. This is intentionally narrower than `append_event`: terminal
    /// jobs otherwise reject mutable progress and stream events.
    fn append_terminal_evidence(
        &self,
        job_id: Uuid,
        kind: JobEventKind,
        message: Option<String>,
        data: Option<Value>,
    ) -> Result<JobEvent> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            if !stored.job.status.is_terminal() {
                return Err(Error::validation_invalid_argument(
                    "status",
                    "terminal evidence requires a terminal job",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            let event = JobEvent {
                sequence: self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1,
                job_id,
                kind,
                timestamp_ms: timestamp_ms(),
                message,
                data,
            };
            stored.events.push(event.clone());
            apply_event_retention(&mut stored.events, self.event_retention_limit());
            stored.job.event_count = stored.events.len();
            stored.job.updated_at_ms = event.timestamp_ms;
            Ok(event)
        })
    }

    pub(super) fn append_event_already_locked(
        &self,
        inner: &mut JobStoreInner,
        job_id: Uuid,
        kind: JobEventKind,
        message: Option<String>,
        data: Option<Value>,
    ) -> Result<JobEvent> {
        let stored = inner
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if kind != JobEventKind::Status && stored.job.status.is_terminal() {
            return Err(Error::validation_invalid_argument(
                "status",
                format!("cannot append {:?} event to terminal job", kind),
                Some(job_id.to_string()),
                None,
            ));
        }
        let event = JobEvent {
            sequence: self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1,
            job_id,
            kind,
            timestamp_ms: timestamp_ms(),
            message,
            data,
        };
        stored.events.push(event.clone());
        apply_event_retention(&mut stored.events, self.event_retention_limit());
        stored.job.event_count = stored.events.len();
        stored.job.updated_at_ms = event.timestamp_ms;
        Ok(event)
    }
}

fn controller_submission_fingerprint(state: &ControllerJobState) -> String {
    let mut hasher = Sha256::new();
    hasher.update(state.job_type.as_bytes());
    hasher.update(state.version.to_be_bytes());
    hasher.update(state.request_digest.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn controller_idempotency_conflict(idempotency_key: &str) -> Error {
    Error::validation_invalid_argument(
        "idempotency_key",
        "controller idempotency key is already bound to a different type, version, or request",
        Some(idempotency_key.to_string()),
        None,
    )
}

enum DeadLeaseJobDisposition {
    RecoveredOuterResult(JobStatus, i64),
    RecoveredLinkedRun(RecoveredTerminalJob),
    ProtectedLive,
    PreservedRemote,
    ProtectedUnsupported(String),
    TerminalizeDead,
}

enum LocalChildLiveness {
    Live,
    Dead,
    Unsupported(String),
}

fn local_child_liveness(child: &LocalChildExecution) -> LocalChildLiveness {
    if let Some(process) = &child.process {
        let root_liveness = match &process.discriminator {
            LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks { ticks } => {
                match crate::process::linux_process_starttime_ticks(process.pid) {
                    Ok(Some(actual)) if actual == *ticks => return LocalChildLiveness::Live,
                    Ok(_) => LocalChildLiveness::Dead,
                    Err(evidence) => return LocalChildLiveness::Unsupported(evidence),
                }
            }
            LocalChildStartDiscriminator::Unsupported { evidence } => {
                if crate::process::pid_is_running(process.pid) {
                    return LocalChildLiveness::Unsupported(format!(
                        "{evidence}; PID {} still exists and Homeboy cannot distinguish PID reuse on this platform",
                        process.pid
                    ));
                } else {
                    LocalChildLiveness::Dead
                }
            }
        };
        if matches!(root_liveness, LocalChildLiveness::Dead) {
            if let Some(pgid) = process.process_group_id {
                return match crate::process::isolated_process_group_is_running(pgid) {
                    Ok(true) => LocalChildLiveness::Live,
                    Ok(false) => LocalChildLiveness::Dead,
                    Err(evidence) => LocalChildLiveness::Unsupported(evidence),
                };
            }
        }
        return root_liveness;
    }
    LocalChildLiveness::Unsupported(format!(
        "durable spawn reservation `{}` has no persisted PID; Homeboy will not infer child ownership from ambient processes",
        child.reservation_id
    ))
}

#[derive(Clone)]
pub struct RecoveredTerminalJob {
    status: JobStatus,
    terminal_result: Value,
    run_id: String,
    artifacts: Vec<JobArtifactMetadata>,
}

impl RecoveredTerminalJob {
    /// Construct a recovered terminal job. Used by the agent-task terminal
    /// recovery provider to build this core type from a durable run's result.
    pub fn new(
        status: JobStatus,
        terminal_result: Value,
        run_id: String,
        artifacts: Vec<JobArtifactMetadata>,
    ) -> Self {
        Self {
            status,
            terminal_result,
            run_id,
            artifacts,
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) enum LinkedDurableRunResolution {
    None,
    Terminal(RecoveredTerminalJob),
    Active(String),
    Unresolved(String),
}

#[cfg(test)]
impl RecoveredTerminalJob {
    pub(super) fn test_result(
        status: JobStatus,
        run_id: &str,
        terminal_result: Value,
        artifacts: Vec<JobArtifactMetadata>,
    ) -> Self {
        Self {
            status,
            terminal_result,
            run_id: run_id.to_string(),
            artifacts,
        }
    }
}

/// The controller-minted `durable_run_id` a stored job was enqueued for, from
/// whichever runner-lifecycle carries it (remote-runner request, local-runner
/// direct-daemon offload, or a driver-declared controller-job linkage).
fn stored_job_durable_run_id(stored: &StoredJob) -> Option<String> {
    stored
        .remote_runner
        .as_ref()
        .and_then(|remote| remote.request().ok())
        .and_then(|request| request.lifecycle)
        .as_ref()
        .or_else(|| {
            stored
                .local_runner
                .as_ref()
                .and_then(|local| local.lifecycle.as_ref())
        })
        .and_then(|lifecycle| lifecycle.durable_run_id.clone())
        .or_else(|| {
            stored
                .controller_job
                .as_ref()
                .and_then(|controller| controller.linked_durable_run_id.as_deref())
                .map(str::trim)
                .map(str::to_string)
        })
        .filter(|run_id| !run_id.trim().is_empty())
}

/// A remote runner workload records its agent-task run ID in a typed execution
/// envelope, and a controller job declares its linked durable run at
/// admission. That durable run is authoritative once the owning execution
/// cannot be observed in process.
fn recovered_terminal_agent_task_result(stored: &StoredJob) -> Option<RecoveredTerminalJob> {
    // Extract the durable agent-task run id from the (opaque) workload or the
    // driver-declared controller linkage; the agent-task terminal-recovery
    // hook resolves it into a recovered job so the job store does not depend
    // on the agent-task subsystem.
    let run_id = stored
        .remote_runner
        .as_ref()
        .and_then(|remote| remote.request().ok())
        .and_then(|request| request.lab_runner_workload)
        .as_ref()
        .and_then(|workload| workload.agent_task.as_ref())
        .map(|agent_task| agent_task.run_id.trim().to_string())
        .or_else(|| {
            stored
                .controller_job
                .as_ref()
                .and_then(|controller| controller.linked_durable_run_id.as_deref())
                .map(str::trim)
                .map(str::to_string)
        })
        .filter(|run_id| !run_id.is_empty())?;
    super::agent_task_terminal_recovery::recovered_terminal_agent_task_job(&run_id)
}

/// Durable terminal evidence for one active job: either its own event log
/// already recorded a terminal result, or its linked durable run terminalized.
/// Both prove no workload remains without inspecting or altering live
/// children, so replacement gates may reconcile on them without an operator
/// attestation.
fn recovered_terminal_agent_task_evidence(stored: &StoredJob) -> Option<RecoveredTerminalJob> {
    if let Some((status, exit_code)) = recovered_terminal_from_result(&stored.events) {
        let terminal_result = stored
            .events
            .iter()
            .rev()
            .find(|event| event.kind == JobEventKind::Result)
            .and_then(|event| event.data.clone())
            .unwrap_or_else(|| serde_json::json!({ "status": status, "exit_code": exit_code }));
        return Some(RecoveredTerminalJob::new(
            status,
            terminal_result,
            stored_job_durable_run_id(stored).unwrap_or_else(|| stored.job.id.to_string()),
            Vec::new(),
        ));
    }
    recovered_terminal_agent_task_result(stored)
}

impl JobHandle {
    pub fn job_id(&self) -> Uuid {
        self.job_id
    }

    pub fn is_cancelled(&self) -> bool {
        self.store
            .get(self.job_id)
            .map(|job| job.status == JobStatus::Cancelled)
            .unwrap_or(true)
            || self.store.controller_cancellation_requested(self.job_id)
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.store
            .get(self.job_id)
            .is_ok_and(|job| job.status.is_terminal())
    }

    pub(crate) fn record_controller_prepared(&self, prepared: Value) -> Result<()> {
        self.store.record_controller_prepared(self.job_id, prepared)
    }

    pub(crate) fn complete_controller_cancellation(&self) -> Result<Job> {
        self.store.complete_controller_cancellation(self.job_id)
    }

    pub(crate) fn complete_controller_success(&self, result: Value) -> Result<Job> {
        self.store.complete_controller_success(self.job_id, result)
    }

    pub(crate) fn fail_controller_cancellation(&self, message: String, data: Value) -> Result<Job> {
        self.store
            .fail_controller_cancellation(self.job_id, message, data)
    }

    pub(crate) fn fail_controller_error(&self, message: String, data: Value) -> Result<Job> {
        self.store.fail_controller_error(self.job_id, message, data)
    }

    pub(crate) fn local_child_reservation_id(&self) -> Result<String> {
        let inner = self.store.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .get(&self.job_id)
            .and_then(|stored| stored.local_child.as_ref())
            .map(|child| child.reservation_id.clone())
            .ok_or_else(|| Error::internal_unexpected("local child reservation is missing"))
    }

    pub(crate) fn accepted_local_child_execution_context(
        &self,
        reservation_id: &str,
        runner_id: &str,
        context_id: &str,
    ) -> Result<Value> {
        let inner = self.store.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&self.job_id)
            .ok_or_else(|| job_not_found(self.job_id))?;
        let local_runner = stored.local_runner.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner_execution_context",
                "daemon job has no local runner authority",
                Some(self.job_id.to_string()),
                None,
            )
        })?;
        let local_child = stored.local_child.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner_execution_context",
                "daemon job has no local child reservation",
                Some(self.job_id.to_string()),
                None,
            )
        })?;
        if stored.job.status != JobStatus::Running
            || local_child.process.is_none()
            || local_child.reservation_id != reservation_id
            || local_runner.runner_id != runner_id
        {
            return Err(Error::validation_invalid_argument(
                "runner_execution_context",
                "daemon job does not match the running local child authority",
                Some(self.job_id.to_string()),
                None,
            ));
        }
        let evidence = stored
            .events
            .iter()
            .rev()
            .filter_map(|event| event.data.as_ref())
            .filter(|data| {
                data.get("phase").and_then(Value::as_str)
                    == Some("runner_job_execution_context_verified")
            })
            .filter_map(|data| data.get("execution_context"))
            .find(|evidence| {
                evidence
                    .get("context")
                    .and_then(|context| context.get("id"))
                    .and_then(Value::as_str)
                    == Some(context_id)
            })
            .cloned()
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runner_execution_context",
                    "daemon job has no matching verified execution context",
                    Some(context_id.to_string()),
                    None,
                )
            })?;
        if evidence
            .get("context")
            .and_then(|context| context.get("controller_run_id"))
            .and_then(Value::as_str)
            != stored_job_durable_run_id(stored).as_deref()
        {
            return Err(Error::validation_invalid_argument(
                "runner_execution_context",
                "daemon job execution context does not match its durable run identity",
                Some(self.job_id.to_string()),
                None,
            ));
        }
        Ok(evidence)
    }

    pub(crate) fn start_with_reserved_child_identity(
        &self,
        pid: u32,
        process_group_id: Option<u32>,
        discriminator: LocalChildStartDiscriminator,
    ) -> Result<Job> {
        self.store.start_with_reserved_child_identity(
            self.job_id,
            pid,
            process_group_id,
            discriminator,
        )
    }

    pub(crate) fn stdout(&self, message: impl Into<String>) -> Result<JobEvent> {
        self.store.append_event(
            self.job_id,
            JobEventKind::Stdout,
            Some(message.into()),
            None,
        )
    }

    pub(crate) fn stderr(&self, message: impl Into<String>) -> Result<JobEvent> {
        self.store.append_event(
            self.job_id,
            JobEventKind::Stderr,
            Some(message.into()),
            None,
        )
    }

    pub fn progress(&self, data: Value) -> Result<JobEvent> {
        self.store
            .append_event(self.job_id, JobEventKind::Progress, None, Some(data))
    }

    pub fn result(&self, data: Value) -> Result<JobEvent> {
        self.store
            .append_event(self.job_id, JobEventKind::Result, None, Some(data))
    }
}

#[cfg(test)]
mod local_child_tests {
    use super::*;

    #[test]
    fn local_child_reservation_persists_before_running_visibility() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("jobs.json");
        let store = JobStore::open(&path).expect("open durable store");
        let job = store.create("runner.exec");

        store
            .reserve_local_child_at_with_runner_capacity(job.id, timestamp_ms(), None)
            .map(|_| ())
            .expect("reserve child");
        let queued = JobStore::open_without_reconciliation(&path).expect("read reservation");
        assert_eq!(
            queued.get(job.id).expect("queued job").status,
            JobStatus::Queued
        );
        assert!(queued
            .inner
            .lock()
            .expect("store")
            .jobs
            .get(&job.id)
            .and_then(|stored| stored.local_child.as_ref())
            .is_some());

        store
            .start_with_reserved_child_identity(
                job.id,
                4242,
                None,
                LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks { ticks: 1 },
            )
            .expect("bind child identity");
        let running = JobStore::open_without_reconciliation(&path).expect("read running");
        assert_eq!(
            running.get(job.id).expect("running job").status,
            JobStatus::Running
        );
        assert!(
            running
                .inner
                .lock()
                .expect("store")
                .jobs
                .get(&job.id)
                .and_then(|stored| stored.local_child.as_ref())
                .and_then(|child| child.process.as_ref())
                .expect("persisted child identity")
                .process_group_id
                .is_none(),
            "records serialized before process-group identity remain readable"
        );
    }

    #[test]
    fn unsupported_identity_with_a_live_pid_blocks_once_without_duplicate_diagnostics() {
        let store = JobStore::default().with_daemon_lease("dead-lease".to_string());
        let job = store.create("runner.exec");
        store
            .reserve_local_child_at_with_runner_capacity(job.id, timestamp_ms(), None)
            .map(|_| ())
            .expect("reserve child");
        store
            .start_with_reserved_child_identity(
                job.id,
                std::process::id(),
                None,
                LocalChildStartDiscriminator::Unsupported {
                    evidence: "fixture unsupported platform discriminator".to_string(),
                },
            )
            .expect("persist unsupported identity");

        let first = store
            .reconcile_dead_daemon_lease_jobs("dead-lease")
            .expect("live PID blocks recovery");
        let event_count = store.events(job.id).expect("events").len();
        let second = store
            .reconcile_dead_daemon_lease_jobs("dead-lease")
            .expect("repeated recovery stays blocked");

        assert_eq!(first.protected_job_ids, vec![job.id]);
        assert_eq!(second.protected_job_ids, vec![job.id]);
        assert_eq!(store.events(job.id).expect("events").len(), event_count);
        assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
    }

    #[test]
    fn unsupported_identity_with_an_absent_pid_terminalizes() {
        let store = JobStore::default().with_daemon_lease("dead-lease".to_string());
        let job = store.create("runner.exec");
        store
            .reserve_local_child_at_with_runner_capacity(job.id, timestamp_ms(), None)
            .map(|_| ())
            .expect("reserve child");
        store
            .start_with_reserved_child_identity(
                job.id,
                u32::MAX,
                None,
                LocalChildStartDiscriminator::Unsupported {
                    evidence: "fixture unsupported platform discriminator".to_string(),
                },
            )
            .expect("persist unsupported identity");

        let diagnostics = store
            .reconcile_dead_daemon_lease_jobs("dead-lease")
            .expect("absent PID is safe proof of death");
        assert!(diagnostics.protected_job_ids.is_empty());
        assert_eq!(store.get(job.id).expect("job").status, JobStatus::Failed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pid_reuse_mismatch_does_not_protect_the_new_process() {
        let store = JobStore::default().with_daemon_lease("dead-lease".to_string());
        let job = store.create("runner.exec");
        store
            .reserve_local_child_at_with_runner_capacity(job.id, timestamp_ms(), None)
            .map(|_| ())
            .expect("reserve child");
        let actual = crate::process::linux_process_starttime_ticks(std::process::id())
            .expect("read current start ticks")
            .expect("current process exists");
        store
            .start_with_reserved_child_identity(
                job.id,
                std::process::id(),
                None,
                LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks {
                    ticks: actual.saturating_add(1),
                },
            )
            .expect("record mismatched identity");

        store
            .reconcile_dead_daemon_lease_jobs("dead-lease")
            .expect("reconcile PID reuse mismatch");
        assert_eq!(store.get(job.id).expect("job").status, JobStatus::Failed);
        assert!(crate::process::pid_is_running(std::process::id()));
    }
}
