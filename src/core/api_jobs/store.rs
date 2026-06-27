use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::persistence::{
    apply_event_retention, job_not_found, read_durable_store, reconcile_stale_jobs, timestamp_ms,
    validate_transition, write_durable_store, DEFAULT_EVENT_RETENTION_LIMIT,
};
use super::remote_runner;
use super::types::{Job, JobEvent, JobEventKind, JobStatus};
use crate::core::error::{Error, Result};
use crate::core::source_snapshot::SourceSnapshot;

#[derive(Debug, Clone, Default)]
pub struct JobStore {
    pub(super) inner: Arc<Mutex<JobStoreInner>>,
    pub(super) next_event_sequence: Arc<AtomicU64>,
    pub(super) persistence: Option<Arc<JobStorePersistence>>,
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
    pub(crate) fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_event_retention(path, DEFAULT_EVENT_RETENTION_LIMIT)
    }

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
        };

        store.persist()?;
        Ok(store)
    }

    /// Snapshot-less job creation convenience. Production code creates jobs via
    /// [`JobStore::run_background_with_source_snapshot`] →
    /// [`JobStore::create_with_source_snapshot`]; this shorthand is only used by
    /// the store's unit tests.
    #[cfg(test)]
    pub(crate) fn create(&self, operation: impl Into<String>) -> Job {
        self.create_with_source_snapshot(operation, None)
    }

    pub(crate) fn create_with_source_snapshot(
        &self,
        operation: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
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
            stale_reason: None,
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

        self.append_status_event(job.id, JobStatus::Queued, "job queued")
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
        let job = self.create_with_source_snapshot(operation, source_snapshot);
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
        self.append_event(
            job_id,
            JobEventKind::Status,
            Some(message.into()),
            Some(serde_json::json!({ "status": status })),
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
