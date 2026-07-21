use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::persistence::{
    apply_event_retention, compact_terminal_jobs, job_not_found, timestamp_ms, validate_transition,
    write_durable_store, JobStoreCompactionEvidence, DEFAULT_EVENT_RETENTION_LIMIT,
    DEFAULT_TERMINAL_JOB_RETENTION_BYTES, DEFAULT_TERMINAL_JOB_RETENTION_LIMIT,
};
#[cfg(test)]
use super::persistence::{read_durable_store, reconcile_stale_jobs};
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
}

#[derive(Debug, Clone, Default)]
pub struct JobStore {
    pub(super) inner: Arc<Mutex<JobStoreInner>>,
    pub(super) next_event_sequence: Arc<AtomicU64>,
    pub(super) persistence: Option<Arc<JobStorePersistence>>,
    pub(super) daemon_lease_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct JobStorePersistence {
    pub(super) path: PathBuf,
    pub(super) event_retention_limit: usize,
    pub(super) terminal_job_retention_limit: usize,
    pub(super) terminal_job_retention_bytes: usize,
}

#[derive(Debug, Default)]
pub(super) struct JobStoreInner {
    pub(super) jobs: HashMap<Uuid, StoredJob>,
    pub(super) submission_keys: HashMap<String, RemoteRunnerSubmission>,
    pub(super) expired_submission_keys: HashMap<String, RemoteRunnerSubmission>,
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
pub(super) struct StoredJob {
    pub(super) job: Job,
    pub(super) events: Vec<JobEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) admission_idempotency_key: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) compaction: Option<JobStoreCompactionEvidence>,
}

#[derive(Debug)]
pub struct JobRunner {
    pub job_id: Uuid,
    pub handle: JoinHandle<()>,
}

#[derive(Debug, Clone)]
pub struct JobHandle {
    store: JobStore,
    job_id: Uuid,
}

impl JobStore {
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
        Ok(durable
            .jobs
            .into_iter()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .count())
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
        };

        store.persist()?;
        Ok(store)
    }

    /// Open durable jobs without treating active records as an implicit daemon
    /// restart. Daemon lifecycle recovery must select ownership explicitly.
    pub fn open_without_reconciliation(path: impl Into<PathBuf>) -> Result<Self> {
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
        let mut durable: DurableJobStore = serde_json::from_slice(raw)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))?;
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
        };
        store.persist()?;
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
        self.create_or_reuse_active_local_runner_job(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            local_runner,
            None,
        )
        .0
    }

    /// Create or renew a caller-owned admission under one store lock. Replays
    /// renew only the live reservation; terminal identities are never revived.
    pub(crate) fn create_or_renew_admission_at(
        &self,
        metadata: Value,
        idempotency_key: &str,
        now: u64,
    ) -> Result<AdmissionReservation> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        if let Some(stored) = inner
            .jobs
            .values_mut()
            .filter(|stored| stored.admission_idempotency_key.as_deref() == Some(idempotency_key))
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
            };
            drop(inner);
            self.persist()?;
            return Ok(reservation);
        }
        drop(inner);
        self.create_admission_inner(metadata, Some(idempotency_key.to_string()), now)
    }

    pub(crate) fn create_admission_at(
        &self,
        metadata: Value,
        now: u64,
    ) -> Result<AdmissionReservation> {
        self.create_admission_inner(metadata, None, now)
    }

    /// Legacy, tokenless admission used only by pre-lease protocol clients
    /// during a rolling daemon upgrade.
    pub(crate) fn create_or_reuse_active_admission(
        &self,
        metadata: Value,
        idempotency_key: &str,
    ) -> (Job, bool) {
        self.create_or_reuse_active_local_runner_job(
            "runner.admission",
            None,
            Some(metadata),
            None,
            None,
            Some(idempotency_key.to_string()),
        )
    }

    fn create_admission_inner(
        &self,
        metadata: Value,
        idempotency_key: Option<String>,
        now: u64,
    ) -> Result<AdmissionReservation> {
        let (job, created) = self.create_or_reuse_active_local_runner_job(
            "runner.admission",
            None,
            Some(metadata),
            None,
            None,
            idempotency_key.clone(),
        );
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
            };
            drop(inner);
            self.persist()?;
            return Ok(reservation);
        }
        let lease = AdmissionLease {
            token: Uuid::new_v4().to_string(),
            expires_at_ms: now.saturating_add(ADMISSION_RESERVATION_LEASE_MS),
            renewals: 0,
        };
        stored.admission_lease = Some(lease.clone());
        drop(inner);
        self.persist()?;
        Ok(AdmissionReservation {
            job,
            token: lease.token,
            expires_at_ms: lease.expires_at_ms,
            created,
        })
    }

    pub(crate) fn renew_admission_at(
        &self,
        job_id: Uuid,
        token: &str,
        now: u64,
    ) -> Result<AdmissionReservation> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        let (token, expires_at_ms) = {
            let lease = Self::admission_lease_for_live_job(stored, token, now)?;
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
        };
        drop(inner);
        self.persist()?;
        Ok(reservation)
    }

    pub(crate) fn release_admission_at(&self, job_id: Uuid, token: &str, now: u64) -> Result<Job> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
        drop(inner);
        self.persist()?;
        Ok(job)
    }

    pub(crate) fn admission_is_leased(&self, job_id: Uuid) -> Result<bool> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        Ok(stored.admission_lease.is_some())
    }

    pub(crate) fn reconcile_expired_admissions_at(&self, now: u64) -> Result<Vec<Uuid>> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
        drop(inner);
        if !expired.is_empty() {
            self.persist()?;
        }
        Ok(expired)
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
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
        local_runner: Option<LocalRunnerJob>,
        admission_idempotency_key: Option<String>,
    ) -> (Job, bool) {
        let now = timestamp_ms();
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
            operation: operation.into(),
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

        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
                return (existing, false);
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
                return (existing, false);
            }
        }
        let consumes_admission = local_runner.is_some();
        inner.jobs.insert(
            job.id,
            StoredJob {
                job: job.clone(),
                events: Vec::new(),
                admission_idempotency_key: admission_idempotency_key.clone(),
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
        drop(inner);

        if let Some(metadata) = metadata {
            self.append_status_event_with_data(job.id, JobStatus::Queued, "job queued", metadata)
        } else {
            self.append_status_event(job.id, JobStatus::Queued, "job queued")
        }
        .expect("newly-created job must accept queued status event");
        (
            self.get(job.id)
                .expect("newly-created job must be readable after insert"),
            true,
        )
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
        self.transition(job_id, JobStatus::Cancelled, reason.into())
    }

    pub(crate) fn append_event(
        &self,
        job_id: Uuid,
        kind: JobEventKind,
        message: Option<String>,
        data: Option<Value>,
    ) -> Result<JobEvent> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
        drop(inner);

        self.persist()?;

        Ok(event)
    }

    pub(crate) fn run_background<T, F>(&self, operation: impl Into<String>, run: F) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        self.run_background_with_source_snapshot(operation, None, run)
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

    /// Reserve a local child while the job is still queued. The reservation is
    /// durable before spawn; only binding a PID plus start ticks exposes Running.
    pub(crate) fn run_local_child_background_with_source_snapshot_metadata_and_path_materialization_plan<
        T,
        F,
    >(
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
        self.run_local_child_background_with_source_snapshot_metadata_path_materialization_and_local_runner(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            None,
            run,
        )
    }

    pub(crate) fn run_local_child_background_with_source_snapshot_metadata_path_materialization_and_local_runner<
        T,
        F,
    >(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
        local_runner: Option<LocalRunnerJob>,
        run: F,
    ) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        let job = self.create_with_source_snapshot_metadata_path_materialization_and_local_runner(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            local_runner,
        );
        self.reserve_local_child(job.id)
            .expect("new local child reservation must persist");
        let job_id = job.id;
        let handle_store = self.clone();
        let worker_store = self.clone();
        let handle = thread::spawn(move || {
            let job_handle = JobHandle {
                store: handle_store,
                job_id,
            };
            let _ = job_handle.progress(serde_json::json!({
                "phase": "local_child_worker_started",
            }));
            match run(job_handle) {
                Ok(output) => {
                    let _ = worker_store.complete(job_id, serde_json::to_value(output).ok());
                }
                Err(error) => {
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
        JobRunner { job_id, handle }
    }

    pub(crate) fn run_capacity_queued_local_child_background_with_source_snapshot_metadata_path_materialization_and_local_runner<
        T,
        F,
    >(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Option<Value>,
        path_materialization_plan: Option<PathMaterializationPlan>,
        local_runner: LocalRunnerJob,
        admission_idempotency_key: Option<String>,
        capacity: usize,
        run: F,
    ) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        let (job, created) = self.create_or_reuse_active_local_runner_job(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            Some(local_runner.clone()),
            admission_idempotency_key,
        );
        let job_id = job.id;
        // An idempotent resubmission reused an already-enqueued job that already
        // has its own worker. Do not spawn a second worker for it — return a
        // handle to a thread that completes immediately so the caller's
        // `JobRunner` contract is preserved.
        if !created {
            let handle = thread::spawn(|| {});
            return JobRunner { job_id, handle };
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
                    &local_runner.runner_id,
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
        JobRunner { job_id, handle }
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
        {
            let mut inner = self.inner.lock().expect("job store mutex poisoned");
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            // Cancellation may be retried after the daemon has already recorded it.
            // It is a no-op so callers receive the authoritative terminal job without
            // adding a duplicate cancellation event.
            if stored.job.status == JobStatus::Cancelled && next_status == JobStatus::Cancelled {
                return Ok(stored.job.clone());
            }
            validate_transition(stored.job.status, next_status)?;

            let now = timestamp_ms();
            stored.job.status = next_status;
            stored.job.updated_at_ms = now;
            if next_status == JobStatus::Running {
                stored.job.started_at_ms = Some(now);
            }
            if next_status.is_terminal() {
                stored.job.finished_at_ms = Some(now);
            }
        }

        self.persist()?;

        self.append_status_event(job_id, next_status, message)?;
        self.get(job_id)
    }

    pub(crate) fn reserve_local_child(&self, job_id: Uuid) -> Result<()> {
        self.reserve_local_child_at(job_id, timestamp_ms())
    }

    pub(crate) fn reserve_local_child_at(&self, job_id: Uuid, now: u64) -> Result<()> {
        self.reserve_local_child_at_with_runner_capacity(job_id, now, None)
            .map(|_| ())
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

    fn reserve_local_child_at_with_runner_capacity(
        &self,
        job_id: Uuid,
        now: u64,
        runner_capacity: Option<(&str, usize)>,
    ) -> Result<bool> {
        let reservation_id = Uuid::new_v4().to_string();
        let reservation_expires_at_ms = now.saturating_add(LOCAL_CHILD_RESERVATION_LEASE_MS);
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let prior = inner
            .jobs
            .get(&job_id)
            .cloned()
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
        if let Some(persistence) = &self.persistence {
            let mut durable = DurableJobStore {
                jobs: inner.jobs.values().cloned().collect(),
                submission_keys: inner.submission_keys.clone(),
                expired_submission_keys: inner.expired_submission_keys.clone(),
                compaction: inner.compaction.clone(),
            };
            compact_terminal_jobs(
                &mut durable,
                persistence.event_retention_limit,
                persistence.terminal_job_retention_limit,
                persistence.terminal_job_retention_bytes,
            );
            if let Err(error) = write_durable_store(&persistence.path, &durable) {
                inner.jobs.insert(job_id, prior);
                return Err(error);
            }
            inner.jobs = durable
                .jobs
                .iter()
                .cloned()
                .map(|stored| (stored.job.id, stored))
                .collect();
            inner.compaction = durable.compaction;
        }
        Ok(true)
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
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
        drop(inner);
        if !expired.is_empty() {
            self.persist()?;
        }
        Ok(expired)
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
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
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
        drop(inner);
        self.persist()?;
        self.get(job_id)
    }

    pub(crate) fn start_with_reserved_child_identity(
        &self,
        job_id: Uuid,
        pid: u32,
        process_group_id: Option<u32>,
        discriminator: LocalChildStartDiscriminator,
    ) -> Result<Job> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let (prior, started) = {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            // Retain the unclaimed reservation so a failed durable write never
            // leaves queued visibility paired with an uncommitted child PID.
            let prior = stored.clone();
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
            (prior, stored.job.clone())
        };

        if let Some(persistence) = &self.persistence {
            let mut durable = DurableJobStore {
                jobs: inner.jobs.values().cloned().collect(),
                submission_keys: inner.submission_keys.clone(),
                expired_submission_keys: inner.expired_submission_keys.clone(),
                compaction: inner.compaction.clone(),
            };
            compact_terminal_jobs(
                &mut durable,
                persistence.event_retention_limit,
                persistence.terminal_job_retention_limit,
                persistence.terminal_job_retention_bytes,
            );
            if let Err(error) = write_durable_store(&persistence.path, &durable) {
                *inner.jobs.get_mut(&job_id).expect("job exists") = prior;
                return Err(error);
            }
            inner.jobs = durable
                .jobs
                .iter()
                .cloned()
                .map(|stored| (stored.job.id, stored))
                .collect();
            inner.compaction = durable.compaction;
        }
        Ok(started)
    }

    pub(super) fn ensure_transition(&self, job_id: Uuid, next_status: JobStatus) -> Result<()> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        validate_transition(stored.job.status, next_status)
    }

    pub(super) fn append_status_event(
        &self,
        job_id: Uuid,
        status: JobStatus,
        message: impl Into<String>,
    ) -> Result<JobEvent> {
        self.append_status_event_with_data(
            job_id,
            status,
            message,
            serde_json::json!({ "status": status }),
        )
    }

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

    pub(super) fn event_retention_limit(&self) -> usize {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.event_retention_limit)
            .unwrap_or(usize::MAX)
    }

    fn terminal_job_retention_limit(&self) -> usize {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.terminal_job_retention_limit)
            .unwrap_or(usize::MAX)
    }

    fn terminal_job_retention_bytes(&self) -> usize {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.terminal_job_retention_bytes)
            .unwrap_or(usize::MAX)
    }

    pub(super) fn persist(&self) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };

        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let mut durable = DurableJobStore {
            jobs: inner.jobs.values().cloned().collect(),
            submission_keys: inner.submission_keys.clone(),
            expired_submission_keys: inner.expired_submission_keys.clone(),
            compaction: inner.compaction.clone(),
        };
        compact_terminal_jobs(
            &mut durable,
            self.event_retention_limit(),
            self.terminal_job_retention_limit(),
            self.terminal_job_retention_bytes(),
        );
        write_durable_store(&persistence.path, &durable)?;
        inner.jobs = durable
            .jobs
            .iter()
            .cloned()
            .map(|stored| (stored.job.id, stored))
            .collect();
        inner.submission_keys = durable.submission_keys;
        inner.expired_submission_keys = durable.expired_submission_keys;
        inner.compaction = durable.compaction;
        Ok(())
    }
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
pub(super) enum LinkedDurableRunResolution {
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
/// whichever runner-lifecycle carries it (remote-runner request or local-runner
/// direct-daemon offload).
fn stored_job_durable_run_id(stored: &StoredJob) -> Option<String> {
    stored
        .remote_runner
        .as_ref()
        .and_then(|remote| remote.request.lifecycle.as_ref())
        .or_else(|| {
            stored
                .local_runner
                .as_ref()
                .and_then(|local| local.lifecycle.as_ref())
        })
        .and_then(|lifecycle| lifecycle.durable_run_id.clone())
        .filter(|run_id| !run_id.trim().is_empty())
}

/// A remote runner workload records its agent-task run ID in a typed execution
/// envelope. That durable run is authoritative after the runner child exits.
fn recovered_terminal_agent_task_result(stored: &StoredJob) -> Option<RecoveredTerminalJob> {
    // Extract the durable agent-task run id from the (opaque) workload; the
    // agent-task terminal-recovery hook resolves it into a recovered job so the
    // job store does not depend on the agent-task subsystem.
    let run_id = stored
        .remote_runner
        .as_ref()?
        .request
        .lab_runner_workload
        .as_ref()?
        .agent_task
        .as_ref()?
        .run_id
        .trim()
        .to_string();
    if run_id.is_empty() {
        return None;
    }
    super::agent_task_terminal_recovery::recovered_terminal_agent_task_job(&run_id)
}

impl JobHandle {
    pub(crate) fn job_id(&self) -> Uuid {
        self.job_id
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.store
            .get(self.job_id)
            .map(|job| job.status == JobStatus::Cancelled)
            .unwrap_or(true)
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

    pub(crate) fn progress(&self, data: Value) -> Result<JobEvent> {
        self.store
            .append_event(self.job_id, JobEventKind::Progress, None, Some(data))
    }

    pub(crate) fn result(&self, data: Value) -> Result<JobEvent> {
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

        store.reserve_local_child(job.id).expect("reserve child");
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
        store.reserve_local_child(job.id).expect("reserve child");
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
        store.reserve_local_child(job.id).expect("reserve child");
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
        store.reserve_local_child(job.id).expect("reserve child");
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
