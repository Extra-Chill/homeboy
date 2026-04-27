#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::core::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobEventKind {
    Status,
    Stdout,
    Stderr,
    Progress,
    Result,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub operation: String,
    pub status: JobStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub sequence: u64,
    pub job_id: Uuid,
    pub kind: JobEventKind,
    pub timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct JobStore {
    inner: Arc<Mutex<JobStoreInner>>,
    next_event_sequence: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct JobStoreInner {
    jobs: HashMap<Uuid, StoredJob>,
}

#[derive(Debug, Clone)]
struct StoredJob {
    job: Job,
    events: Vec<JobEvent>,
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
    pub(crate) fn create(&self, operation: impl Into<String>) -> Job {
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
        };

        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        inner.jobs.insert(
            job.id,
            StoredJob {
                job: job.clone(),
                events: Vec::new(),
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
        stored.job.event_count = stored.events.len();
        stored.job.updated_at_ms = event.timestamp_ms;

        Ok(event)
    }

    pub(crate) fn run_background<T, F>(&self, operation: impl Into<String>, run: F) -> JobRunner
    where
        T: Serialize + Send + 'static,
        F: FnOnce(JobHandle) -> Result<T> + Send + 'static,
    {
        let job = self.create(operation);
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

    fn transition(
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

        self.append_status_event(job_id, next_status, message)?;
        self.get(job_id)
    }

    fn ensure_transition(&self, job_id: Uuid, next_status: JobStatus) -> Result<()> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        validate_transition(stored.job.status, next_status)
    }

    fn append_status_event(
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
}

impl JobHandle {
    pub(crate) fn job_id(&self) -> Uuid {
        self.job_id
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
}

fn validate_transition(current: JobStatus, next: JobStatus) -> Result<()> {
    let allowed = matches!(
        (current, next),
        (JobStatus::Queued, JobStatus::Running)
            | (JobStatus::Queued, JobStatus::Cancelled)
            | (JobStatus::Running, JobStatus::Succeeded)
            | (JobStatus::Running, JobStatus::Failed)
            | (JobStatus::Running, JobStatus::Cancelled)
    );

    if allowed {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "status",
            format!("cannot transition job from {:?} to {:?}", current, next),
            None,
            None,
        ))
    }
}

fn job_not_found(job_id: Uuid) -> Error {
    Error::validation_invalid_argument("job_id", "job not found", Some(job_id.to_string()), None)
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_create() {
        let store = JobStore::default();
        let job = store.create("audit");

        assert_eq!(job.operation, "audit");
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.event_count, 1);
    }

    #[test]
    fn test_get() {
        let store = JobStore::default();
        let job = store.create("audit");

        assert_eq!(store.get(job.id).expect("job is readable").id, job.id);
    }

    #[test]
    fn test_list() {
        let store = JobStore::default();
        let first = store.create("audit");
        let second = store.create("lint");

        let mut jobs = store.list();
        jobs.sort_by(|a, b| a.operation.cmp(&b.operation));
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, first.id);
        assert_eq!(jobs[1].id, second.id);
    }

    #[test]
    fn test_start() {
        let store = JobStore::default();
        let job = store.create("audit");

        let running = store.start(job.id).expect("job starts");
        assert_eq!(running.status, JobStatus::Running);
        assert!(running.started_at_ms.is_some());
    }

    #[test]
    fn test_append_event() {
        let store = JobStore::default();
        let job = store.create("audit");
        store.start(job.id).expect("job starts");

        let event = store
            .append_event(
                job.id,
                JobEventKind::Stdout,
                Some("running audit".to_string()),
                None,
            )
            .expect("stdout event appends");

        assert_eq!(event.kind, JobEventKind::Stdout);
        assert_eq!(event.message.as_deref(), Some("running audit"));
    }

    #[test]
    fn test_complete() {
        let store = JobStore::default();
        let job = store.create("audit");
        store.start(job.id).expect("job starts");

        let completed = store
            .complete(job.id, Some(json!({ "findings": 0 })))
            .expect("job completes");
        assert_eq!(completed.status, JobStatus::Succeeded);
        assert!(completed.finished_at_ms.is_some());
    }

    #[test]
    fn test_fail() {
        let store = JobStore::default();
        let job = store.create("lint");
        store.start(job.id).expect("job starts");

        let failed = store.fail(job.id, "lint failed").expect("job fails");
        assert_eq!(failed.status, JobStatus::Failed);
        assert!(store
            .events(job.id)
            .expect("events are readable")
            .iter()
            .any(|event| event.kind == JobEventKind::Error));
    }

    #[test]
    fn test_cancel() {
        let store = JobStore::default();
        let job = store.create("bench");

        let cancelled = store.cancel(job.id, "user requested").expect("job cancels");
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(cancelled.started_at_ms.is_none());
        assert!(cancelled.finished_at_ms.is_some());
    }

    #[test]
    fn test_job_id() {
        let store = JobStore::default();
        let runner = store.run_background("test", |job| Ok(job.job_id().to_string()));

        runner.handle.join().expect("worker thread exits cleanly");
        assert_eq!(
            store
                .events(runner.job_id)
                .expect("events are readable")
                .iter()
                .find(|event| event.kind == JobEventKind::Result)
                .and_then(|event| event.data.as_ref()),
            Some(&json!(runner.job_id.to_string()))
        );
    }

    #[test]
    fn test_stdout() {
        let store = JobStore::default();
        let runner = store.run_background("test", |job| {
            job.stdout("stdout line")?;
            Ok(json!(true))
        });

        runner.handle.join().expect("worker thread exits cleanly");
        assert!(store
            .events(runner.job_id)
            .expect("events are readable")
            .iter()
            .any(|event| event.kind == JobEventKind::Stdout));
    }

    #[test]
    fn test_stderr() {
        let store = JobStore::default();
        let runner = store.run_background("test", |job| {
            job.stderr("stderr line")?;
            Ok(json!(true))
        });

        runner.handle.join().expect("worker thread exits cleanly");
        assert!(store
            .events(runner.job_id)
            .expect("events are readable")
            .iter()
            .any(|event| event.kind == JobEventKind::Stderr));
    }

    #[test]
    fn test_progress() {
        let store = JobStore::default();
        let runner = store.run_background("test", |job| {
            job.progress(json!({ "current": 1, "total": 1 }))?;
            Ok(json!(true))
        });

        runner.handle.join().expect("worker thread exits cleanly");
        assert!(store
            .events(runner.job_id)
            .expect("events are readable")
            .iter()
            .any(|event| event.kind == JobEventKind::Progress));
    }

    #[test]
    fn job_lifecycle_records_status_events_in_order() {
        let store = JobStore::default();
        let job = store.create("audit");

        store.start(job.id).expect("job starts");
        store
            .append_event(
                job.id,
                JobEventKind::Stdout,
                Some("running audit".to_string()),
                None,
            )
            .expect("stdout event appends");
        store
            .append_event(
                job.id,
                JobEventKind::Progress,
                None,
                Some(json!({ "current": 1, "total": 2 })),
            )
            .expect("progress event appends");

        store
            .complete(job.id, Some(json!({ "findings": 0 })))
            .expect("job completes");

        let events = store.events(job.id).expect("events are readable");
        let kinds: Vec<JobEventKind> = events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                JobEventKind::Status,
                JobEventKind::Status,
                JobEventKind::Stdout,
                JobEventKind::Progress,
                JobEventKind::Result,
                JobEventKind::Status,
            ]
        );
        assert!(events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence));
        assert_eq!(
            events.last().unwrap().data,
            Some(json!({ "status": "succeeded" }))
        );
    }

    #[test]
    fn invalid_status_transitions_are_rejected() {
        let store = JobStore::default();
        let job = store.create("lint");

        let err = store
            .complete(job.id, None)
            .expect_err("queued job cannot complete before running");
        assert!(err.to_string().contains("Queued to Succeeded"));
        assert_eq!(
            store.events(job.id).expect("events are readable").len(),
            1,
            "failed transition must not append result or status events"
        );

        store.start(job.id).expect("job starts");
        store.fail(job.id, "lint failed").expect("job fails");

        let err = store
            .cancel(job.id, "too late")
            .expect_err("terminal job cannot be cancelled");
        assert!(err.to_string().contains("Failed to Cancelled"));

        let err = store
            .append_event(
                job.id,
                JobEventKind::Stdout,
                Some("too late".to_string()),
                None,
            )
            .expect_err("terminal job cannot receive more output");
        assert!(err.to_string().contains("terminal job"));
    }

    #[test]
    fn background_runner_captures_result_and_handle_events() {
        let store = JobStore::default();
        let runner = store.run_background("rig-check", |job| {
            job.stdout("checking services")?;
            job.progress(json!({ "checked": 1, "total": 1 }))?;
            Ok(json!({ "ok": true, "job_id": job.job_id().to_string() }))
        });

        runner.handle.join().expect("worker thread exits cleanly");

        let job = store.get(runner.job_id).expect("job is readable");
        assert_eq!(job.status, JobStatus::Succeeded);

        let events = store.events(runner.job_id).expect("events are readable");
        assert_eq!(events[0].kind, JobEventKind::Status);
        assert!(events
            .iter()
            .any(|event| event.kind == JobEventKind::Stdout));
        assert!(events
            .iter()
            .any(|event| event.kind == JobEventKind::Progress));
        assert!(events
            .iter()
            .any(|event| event.kind == JobEventKind::Result));
        assert_eq!(
            events.last().unwrap().data,
            Some(json!({ "status": "succeeded" }))
        );
    }

    #[test]
    fn background_runner_captures_errors_as_failed_jobs() {
        let store = JobStore::default();
        let runner = store.run_background::<serde_json::Value, _>("test", |_job| {
            Err(Error::validation_invalid_argument(
                "fixture", "boom", None, None,
            ))
        });

        runner.handle.join().expect("worker thread exits cleanly");

        let job = store.get(runner.job_id).expect("job is readable");
        assert_eq!(job.status, JobStatus::Failed);

        let events = store.events(runner.job_id).expect("events are readable");
        assert!(events.iter().any(|event| {
            event.kind == JobEventKind::Error
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("boom"))
        }));
        assert_eq!(
            events.last().unwrap().data,
            Some(json!({ "status": "failed" }))
        );
    }
}
