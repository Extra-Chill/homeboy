use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[cfg(test)]
use super::persistence::reconcile_stale_jobs;
use super::persistence::{
    apply_event_retention, job_not_found, read_durable_store, recovered_terminal_from_result,
    stale_after_restart_classification, timestamp_ms, validate_transition, write_durable_store,
    DEFAULT_EVENT_RETENTION_LIMIT,
};
use super::remote_runner;
use super::types::{
    DaemonLeaseJobDiagnostics, DaemonMissingLeaseJobDiagnostics, Job, JobEvent, JobEventKind,
    JobStatus,
};
use crate::core::error::{Error, Result};
use crate::core::runner_execution_envelope::PathMaterializationPlan;
use crate::core::source_snapshot::SourceSnapshot;

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
}

#[derive(Debug, Default)]
pub(super) struct JobStoreInner {
    pub(super) jobs: HashMap<Uuid, StoredJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredJob {
    pub(super) job: Job,
    pub(super) events: Vec<JobEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) remote_runner: Option<remote_runner::StoredRemoteRunnerJob>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct DurableJobStore {
    pub(super) jobs: Vec<StoredJob>,
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
        Self::open_with_event_retention(path, DEFAULT_EVENT_RETENTION_LIMIT)
    }

    #[cfg(test)]
    pub(crate) fn open_with_event_retention(
        path: impl Into<PathBuf>,
        event_retention_limit: usize,
    ) -> Result<Self> {
        let path = path.into();
        let mut durable = read_durable_store(&path)?;
        let event_retention_limit = event_retention_limit.max(1);
        let next_sequence = reconcile_stale_jobs(&mut durable, event_retention_limit);
        let store = Self {
            inner: Arc::new(Mutex::new(JobStoreInner {
                jobs: durable
                    .jobs
                    .into_iter()
                    .map(|stored| (stored.job.id, stored))
                    .collect(),
            })),
            next_event_sequence: Arc::new(AtomicU64::new(next_sequence)),
            persistence: Some(Arc::new(JobStorePersistence {
                path,
                event_retention_limit,
            })),
            daemon_lease_id: None,
        };

        store.persist()?;
        Ok(store)
    }

    /// Open durable jobs without treating active records as an implicit daemon
    /// restart. Daemon lifecycle recovery must select ownership explicitly.
    pub(crate) fn open_without_reconciliation(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let durable = read_durable_store(&path)?;
        let next_sequence = durable
            .jobs
            .iter()
            .flat_map(|stored| stored.events.iter().map(|event| event.sequence))
            .max()
            .unwrap_or(0);
        Ok(Self {
            inner: Arc::new(Mutex::new(JobStoreInner {
                jobs: durable
                    .jobs
                    .into_iter()
                    .map(|stored| (stored.job.id, stored))
                    .collect(),
            })),
            next_event_sequence: Arc::new(AtomicU64::new(next_sequence)),
            persistence: Some(Arc::new(JobStorePersistence {
                path,
                event_retention_limit: DEFAULT_EVENT_RETENTION_LIMIT,
            })),
            daemon_lease_id: None,
        })
    }

    pub(crate) fn with_daemon_lease(mut self, daemon_lease_id: String) -> Self {
        self.daemon_lease_id = Some(daemon_lease_id);
        self
    }

    /// Snapshot-less job creation convenience. Production code creates jobs via
    /// [`JobStore::run_background_with_source_snapshot`] →
    /// [`JobStore::create_with_source_snapshot`]; this shorthand is only used by
    /// the store's unit tests.
    #[cfg(test)]
    pub(crate) fn create(&self, operation: impl Into<String>) -> Job {
        self.create_with_source_snapshot(operation, None)
    }

    #[cfg(test)]
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
        let now = timestamp_ms();
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
        };

        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        inner.jobs.insert(
            job.id,
            StoredJob {
                job: job.clone(),
                events: Vec::new(),
                remote_runner: None,
            },
        );
        drop(inner);

        if let Some(metadata) = metadata {
            self.append_status_event_with_data(job.id, JobStatus::Queued, "job queued", metadata)
        } else {
            self.append_status_event(job.id, JobStatus::Queued, "job queued")
        }
        .expect("newly-created job must accept queued status event");
        self.get(job.id)
            .expect("newly-created job must be readable after insert")
    }

    pub(crate) fn get(&self, job_id: Uuid) -> Result<Job> {
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

    pub fn daemon_lease_job_diagnostics(
        &self,
        expected_lease_id: &str,
    ) -> DaemonLeaseJobDiagnostics {
        let mut diagnostics = DaemonLeaseJobDiagnostics {
            expected_lease_id: expected_lease_id.to_string(),
            ..DaemonLeaseJobDiagnostics::default()
        };
        let inner = self.inner.lock().expect("job store mutex poisoned");
        for stored in inner.jobs.values() {
            if !matches!(stored.job.status, JobStatus::Queued | JobStatus::Running) {
                continue;
            }
            match stored.job.daemon_lease_id.as_deref() {
                Some(lease_id) if lease_id == expected_lease_id => {
                    diagnostics.matching_job_ids.push(stored.job.id)
                }
                Some(_) => diagnostics.other_lease_job_ids.push(stored.job.id),
                None => diagnostics.unowned_job_ids.push(stored.job.id),
            }
        }
        diagnostics.matching_job_ids.sort();
        diagnostics.other_lease_job_ids.sort();
        diagnostics.unowned_job_ids.sort();
        diagnostics
    }

    pub fn reconcile_dead_daemon_lease_jobs(
        &self,
        expected_lease_id: &str,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        let diagnostics = self.daemon_lease_job_diagnostics(expected_lease_id);
        if diagnostics.unowned_count() > 0 {
            return Err(Error::validation_invalid_argument(
                "expected-lease-id",
                format!(
                    "refusing automatic dead-daemon recovery: {} legacy unowned active job(s) {}; inspect them with `homeboy daemon status` and reconcile after operator review",
                    diagnostics.unowned_count(),
                    diagnostics.unowned_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                ),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }

        let now = timestamp_ms();
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        for job_id in &diagnostics.matching_job_ids {
            let stored = inner.jobs.get_mut(job_id).expect("diagnosed job exists");
            if let Some((status, exit_code)) = recovered_terminal_from_result(&stored.events) {
                stored.job.status = status;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = None;
                let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                stored.events.push(JobEvent {
                    sequence,
                    job_id: *job_id,
                    kind: JobEventKind::Status,
                    timestamp_ms: now,
                    message: Some(
                        "job terminal status recovered from recorded result after dead daemon lease"
                            .to_string(),
                    ),
                    data: Some(serde_json::json!({
                        "status": status,
                        "reason": "recovered_after_dead_daemon_lease",
                        "exit_code": exit_code,
                        "daemon_lease_id": expected_lease_id,
                    })),
                });
                apply_event_retention(&mut stored.events, self.event_retention_limit());
                stored.job.event_count = stored.events.len();
                continue;
            }
            let reason = "daemon lease owner process was not running".to_string();
            stored.job.status = JobStatus::Failed;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason = Some(reason.clone());
            let classification = stale_after_restart_classification(stored);
            for (kind, message, data) in [
                (
                    JobEventKind::Error,
                    reason,
                    serde_json::json!({
                        "reason": "dead_daemon_lease",
                        "classification": classification,
                        "daemon_lease_id": expected_lease_id,
                    }),
                ),
                (
                    JobEventKind::Status,
                    "job marked failed after dead daemon lease".to_string(),
                    serde_json::json!({
                        "status": JobStatus::Failed,
                        "reason": "dead_daemon_lease",
                        "daemon_lease_id": expected_lease_id,
                    }),
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
        self.persist()?;
        Ok(diagnostics)
    }

    /// Explicitly terminalize an unowned store after the daemon lifecycle has
    /// independently established that no daemon can still own it.
    pub fn reconcile_leaseless_orphan_jobs(&self) -> Result<Vec<Uuid>> {
        let mut job_ids: Vec<Uuid> = self
            .inner
            .lock()
            .expect("job store mutex poisoned")
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .map(|stored| stored.job.id)
            .collect();
        job_ids.sort();
        if let Some(job) = job_ids
            .iter()
            .find_map(|id| self.get(*id).ok())
            .filter(|job| job.daemon_lease_id.is_some())
        {
            return Err(Error::validation_invalid_argument(
                "job_store",
                format!(
                    "refusing lease-less reconciliation: active job {} has daemon lease evidence",
                    job.id
                ),
                None,
            ));
        }

        let now = timestamp_ms();
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        for job_id in &job_ids {
            let stored = inner.jobs.get_mut(job_id).expect("diagnosed job exists");
            if let Some((status, exit_code)) = recovered_terminal_from_result(&stored.events) {
                stored.job.status = status;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = None;
                let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                stored.events.push(JobEvent { sequence, job_id: *job_id, kind: JobEventKind::Status, timestamp_ms: now, message: Some("job terminal status recovered from retained result after lease-less control-plane loss".to_string()), data: Some(serde_json::json!({"status": status, "reason": "leaseless_orphan_reconciliation", "exit_code": exit_code})) });
            } else {
                let reason =
                    "control plane lost before the job reached a terminal status".to_string();
                stored.job.status = JobStatus::Failed;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = Some(reason.clone());
                let classification = stale_after_restart_classification(stored);
                for (kind, message, data) in [
                    (
                        JobEventKind::Error,
                        reason,
                        serde_json::json!({"reason":"leaseless_orphan_reconciliation", "classification": classification, "retry_guidance":"Inspect retained job events, then retry eligible work through its original command or workflow."}),
                    ),
                    (
                        JobEventKind::Status,
                        "job marked failed after lease-less control-plane loss".to_string(),
                        serde_json::json!({"status":JobStatus::Failed, "reason":"leaseless_orphan_reconciliation"}),
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
        self.persist()?;
        Ok(job_ids)
    }

    pub fn reconcile_missing_daemon_lease_jobs(
        &self,
        state_identity: &str,
    ) -> Result<DaemonMissingLeaseJobDiagnostics> {
        let terminalized_job_ids = self.reconcile_leaseless_orphan_jobs()?;
        Ok(DaemonMissingLeaseJobDiagnostics {
            terminalized_job_ids,
            durable_run_ids: Vec::new(),
            owned_job_ids: Vec::new(),
        })
    }

    pub(crate) fn active_runner_jobs(&self) -> Vec<super::types::ActiveRunnerJobSummary> {
        let now = timestamp_ms();
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut jobs: Vec<super::types::ActiveRunnerJobSummary> = inner
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .filter_map(|stored| {
                let request = stored.remote_runner.as_ref()?.request.clone();
                Some(super::summary::active_runner_job_summary(
                    &stored.job,
                    &request,
                    now,
                ))
            })
            .collect();
        jobs.sort_by_key(|job| (job.started_at_ms, job.job_id.clone()));
        jobs
    }

    pub(crate) fn stale_runner_jobs(&self) -> Vec<super::types::ActiveRunnerJobSummary> {
        let now = timestamp_ms();
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut jobs: Vec<super::types::ActiveRunnerJobSummary> = inner
            .jobs
            .values()
            .filter(|stored| {
                stored.job.status == JobStatus::Failed && stored.job.stale_reason.is_some()
            })
            .filter_map(|stored| {
                let request = stored.remote_runner.as_ref()?.request.clone();
                Some(super::summary::active_runner_job_summary(
                    &stored.job,
                    &request,
                    now,
                ))
            })
            .collect();
        jobs.sort_by_key(|job| (job.updated_at_ms, job.job_id.clone()));
        jobs
    }

    pub(crate) fn events(&self, job_id: Uuid) -> Result<Vec<JobEvent>> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        Ok(stored.events.clone())
    }

    pub(crate) fn start(&self, job_id: Uuid) -> Result<Job> {
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
        self.ensure_transition(job_id, JobStatus::Failed)?;
        let error = error.into();
        self.append_event(job_id, JobEventKind::Error, Some(error.clone()), None)?;
        self.transition(job_id, JobStatus::Failed, error)
    }

    pub(crate) fn cancel(&self, job_id: Uuid, reason: impl Into<String>) -> Result<Job> {
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
            if worker_store.start(job_id).is_err() {
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

    fn event_retention_limit(&self) -> usize {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.event_retention_limit)
            .unwrap_or(usize::MAX)
    }

    pub(super) fn persist(&self) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };

        let durable = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            DurableJobStore {
                jobs: inner.jobs.values().cloned().collect(),
            }
        };

        write_durable_store(&persistence.path, &durable)
    }
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
