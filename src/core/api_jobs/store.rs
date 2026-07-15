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
    apply_event_retention, job_not_found, recovered_terminal_from_result,
    stale_after_restart_classification, timestamp_ms, validate_transition, write_durable_store,
    DEFAULT_EVENT_RETENTION_LIMIT,
};
#[cfg(test)]
use super::persistence::{read_durable_store, reconcile_stale_jobs};
use super::remote_runner;
use super::remote_runner::JobArtifactMetadata;
use super::types::{
    DaemonLeaseJobDiagnostics, Job, JobEvent, JobEventKind, JobStatus, LeaselessOrphanAffectedJob,
    LeaselessOrphanJobDiagnostics,
};
use crate::core::agent_task_scheduler::AgentTaskAggregateStatus;
use crate::core::agent_task_service;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) local_child: Option<LocalChildExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LocalChildExecution {
    reservation_id: String,
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

    pub(crate) fn open_without_reconciliation_from_bytes(
        path: impl Into<PathBuf>,
        raw: &[u8],
    ) -> Result<Self> {
        let path = path.into();
        let durable: DurableJobStore = serde_json::from_slice(raw)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))?;
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
                local_child: None,
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
                // The empty lease selector is private to lease-less recovery.
                // It routes pre-lease records through this exact typed engine.
                None if expected_lease_id.is_empty() => {
                    diagnostics.matching_job_ids.push(stored.job.id)
                }
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
        // Remote claims are broker-owned, not daemon-child processes. Expire
        // them through the broker lifecycle before considering daemon recovery.
        self.reconcile_expired_remote_runner_claims(timestamp_ms())?;
        self.reconcile_dead_daemon_lease_jobs_with_local_child_liveness(
            expected_lease_id,
            local_child_liveness,
            recovered_terminal_agent_task_result,
        )
    }

    #[cfg(test)]
    pub(super) fn reconcile_dead_daemon_lease_jobs_with_child_liveness(
        &self,
        expected_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
            expected_lease_id,
            pid_is_alive,
            recovered_terminal_agent_task_result,
        )
    }

    #[cfg(test)]
    pub(super) fn reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
        &self,
        expected_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
        terminal_child_result: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_local_child_liveness(
            expected_lease_id,
            |child| match child.process.as_ref() {
                Some(process) if pid_is_alive(process.pid) => LocalChildLiveness::Live,
                Some(_) => LocalChildLiveness::Dead,
                None => LocalChildLiveness::Unsupported(
                    "test liveness probe cannot establish a reserved child identity".to_string(),
                ),
            },
            terminal_child_result,
        )
    }

    fn reconcile_dead_daemon_lease_jobs_with_local_child_liveness(
        &self,
        expected_lease_id: &str,
        inspect_local_child: impl Fn(&LocalChildExecution) -> LocalChildLiveness,
        terminal_child_result: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
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

        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let mut diagnostics = diagnostics;
        let mut dispositions = Vec::with_capacity(diagnostics.matching_job_ids.len());
        let mut ambiguous_job_ids = Vec::new();
        let reconciliation_now = timestamp_ms();
        for job_id in &diagnostics.matching_job_ids {
            let stored = inner.jobs.get(job_id).expect("diagnosed job exists");
            let disposition =
                if let Some((status, exit_code)) = recovered_terminal_from_result(&stored.events) {
                    DeadLeaseJobDisposition::RecoveredOuterResult(status, exit_code)
                } else if stored.remote_runner.is_some() {
                    if stored.job.status == JobStatus::Queued
                        || stored
                            .job
                            .claim_expires_at_ms
                            .is_some_and(|expires| expires > reconciliation_now)
                    {
                        diagnostics.preserved_remote_job_ids.push(*job_id);
                        DeadLeaseJobDisposition::PreservedRemote
                    } else if let Some(recovered) = terminal_child_result(stored) {
                        DeadLeaseJobDisposition::RecoveredLinkedRun(recovered)
                    } else {
                        DeadLeaseJobDisposition::TerminalizeDead
                    }
                } else if let Some(local_child) = stored.local_child.as_ref() {
                    match inspect_local_child(local_child) {
                        LocalChildLiveness::Live => {
                            diagnostics.protected_job_ids.push(*job_id);
                            DeadLeaseJobDisposition::ProtectedLive
                        }
                        LocalChildLiveness::Dead => {
                            if let Some(recovered) = terminal_child_result(stored) {
                                DeadLeaseJobDisposition::RecoveredLinkedRun(recovered)
                            } else {
                                DeadLeaseJobDisposition::TerminalizeDead
                            }
                        }
                        LocalChildLiveness::Unsupported(evidence) => {
                            diagnostics.protected_job_ids.push(*job_id);
                            DeadLeaseJobDisposition::ProtectedUnsupported(evidence)
                        }
                    }
                } else {
                    if stored.job.status == JobStatus::Queued {
                        DeadLeaseJobDisposition::TerminalizeDead
                    } else {
                        ambiguous_job_ids.push(*job_id);
                        continue;
                    }
                };
            dispositions.push((*job_id, disposition));
        }
        if !ambiguous_job_ids.is_empty() {
            return Err(Error::validation_invalid_argument(
                "expected-lease-id",
                format!(
                    "refusing automatic dead-daemon recovery: active job(s) {} have no authoritative terminal result or recorded child PID; inspect durable lifecycle/process evidence with `homeboy daemon status` before retrying",
                    ambiguous_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                ),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }

        let now = timestamp_ms();
        for (job_id, disposition) in dispositions {
            let stored = inner.jobs.get_mut(&job_id).expect("diagnosed job exists");
            if let DeadLeaseJobDisposition::RecoveredOuterResult(status, exit_code) = disposition {
                stored.job.status = status;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = None;
                let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                stored.events.push(JobEvent {
                    sequence,
                    job_id,
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
            if let DeadLeaseJobDisposition::RecoveredLinkedRun(recovered) = disposition {
                stored.job.status = recovered.status;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = None;
                for artifact in recovered.artifacts {
                    if !stored
                        .job
                        .artifacts
                        .iter()
                        .any(|existing| existing.id == artifact.id)
                    {
                        stored.job.artifacts.push(artifact);
                    }
                }
                let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                stored.events.push(JobEvent {
                    sequence,
                    job_id,
                    kind: JobEventKind::Status,
                    timestamp_ms: now,
                    message: Some(format!(
                        "job terminal status recovered from linked durable run `{}` after dead daemon lease",
                        recovered.run_id
                    )),
                    data: Some(serde_json::json!({
                        "status": recovered.status,
                        "reason": "recovered_after_dead_daemon_lease",
                        "daemon_lease_id": expected_lease_id,
                        "child_terminal_result": recovered.terminal_result,
                    })),
                });
                apply_event_retention(&mut stored.events, self.event_retention_limit());
                stored.job.event_count = stored.events.len();
                continue;
            }
            if let DeadLeaseJobDisposition::ProtectedUnsupported(evidence) = disposition {
                let duplicate = stored.events.last().is_some_and(|event| {
                    event.data.as_ref().is_some_and(|data| {
                        data["reason"] == "local_child_identity_unsupported"
                            && data["evidence"] == evidence
                    })
                });
                if !duplicate {
                    let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                    stored.events.push(JobEvent {
                        sequence,
                        job_id,
                        kind: JobEventKind::Progress,
                        timestamp_ms: now,
                        message: Some("local child recovery deferred".to_string()),
                        data: Some(serde_json::json!({
                            "reason": "local_child_identity_unsupported",
                            "evidence": evidence,
                            "recovery": "Homeboy cannot reattach or collect this child result; it blocks replacement until exact process evidence is available.",
                        })),
                    });
                    stored.job.event_count = stored.events.len();
                }
                continue;
            }
            if matches!(disposition, DeadLeaseJobDisposition::ProtectedLive) {
                continue;
            }
            if matches!(disposition, DeadLeaseJobDisposition::PreservedRemote) {
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
                    job_id,
                    kind,
                    timestamp_ms: now,
                    message: Some(message),
                    data: Some(data),
                });
            }
            apply_event_retention(&mut stored.events, self.event_retention_limit());
            stored.job.event_count = stored.events.len();
        }
        diagnostics.protected_job_ids.sort();
        diagnostics.preserved_remote_job_ids.sort();
        drop(inner);
        self.persist()?;
        Ok(diagnostics)
    }

    /// Terminalize all active jobs after an operator has proved that no daemon
    /// owns a store whose current daemon lease is missing. Historical job leases
    /// remain evidence; they cannot prove a current owner or be adopted as one.
    pub fn reconcile_leaseless_orphan_jobs(&self) -> Result<LeaselessOrphanJobDiagnostics> {
        // Claims are broker-owned and must be expired before classifying local
        // child identity. The per-lease call below is the same typed disposition
        // engine used by dead-lease recovery.
        self.reconcile_expired_remote_runner_claims(timestamp_ms())?;
        let (historical_lease_ids, has_unowned_jobs) = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            let mut leases = Vec::new();
            let mut has_unowned = false;
            for stored in inner.jobs.values().filter(|stored| {
                matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
            }) {
                if let Some(lease_id) = &stored.job.daemon_lease_id {
                    leases.push(lease_id.clone());
                } else {
                    has_unowned = true;
                }
            }
            (leases, has_unowned)
        };

        let mut diagnostics = LeaselessOrphanJobDiagnostics {
            historical_lease_ids,
            ..LeaselessOrphanJobDiagnostics::default()
        };
        diagnostics.historical_lease_ids.sort();
        diagnostics.historical_lease_ids.dedup();
        let mut selectors = Vec::with_capacity(
            diagnostics.historical_lease_ids.len() + usize::from(has_unowned_jobs),
        );
        if has_unowned_jobs {
            selectors.push(String::new());
        }
        selectors.extend(diagnostics.historical_lease_ids.clone());
        for lease_id in selectors {
            let lease = self.reconcile_dead_daemon_lease_jobs(&lease_id)?;
            diagnostics
                .protected_job_ids
                .extend(lease.protected_job_ids.iter().copied());
            diagnostics
                .preserved_remote_job_ids
                .extend(lease.preserved_remote_job_ids.iter().copied());
            for job_id in lease.matching_job_ids {
                if lease.protected_job_ids.contains(&job_id)
                    || lease.preserved_remote_job_ids.contains(&job_id)
                {
                    continue;
                }
                diagnostics.reconciled_job_ids.push(job_id);
                diagnostics.affected_jobs.push(LeaselessOrphanAffectedJob {
                    job_id,
                    original_daemon_lease_id: (!lease_id.is_empty()).then(|| lease_id.clone()),
                });
            }
        }
        diagnostics.reconciled_job_ids.sort();
        diagnostics.reconciled_job_ids.dedup();
        diagnostics.affected_jobs.sort_by_key(|job| job.job_id);
        diagnostics.affected_jobs.dedup_by_key(|job| job.job_id);
        diagnostics.protected_job_ids.sort();
        diagnostics.protected_job_ids.dedup();
        diagnostics.preserved_remote_job_ids.sort();
        diagnostics.preserved_remote_job_ids.dedup();
        Ok(diagnostics)
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
        let job = self.create_with_source_snapshot_metadata_and_path_materialization_plan(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
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
            match run(job_handle) {
                Ok(output) => {
                    let _ = worker_store.complete(job_id, serde_json::to_value(output).ok());
                }
                Err(error) => {
                    let _ = worker_store.fail(job_id, error.to_string());
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
        let reservation_id = Uuid::new_v4().to_string();
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let prior = inner
            .jobs
            .get(&job_id)
            .cloned()
            .ok_or_else(|| job_not_found(job_id))?;
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
            process: None,
        });
        let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        stored.events.push(JobEvent {
            sequence,
            job_id,
            kind: JobEventKind::Progress,
            timestamp_ms: timestamp_ms(),
            message: Some("local runner child reserved before spawn".to_string()),
            data: Some(
                serde_json::json!({ "phase": "child_reserved", "reservation_id": reservation_id }),
            ),
        });
        stored.job.event_count = stored.events.len();
        if let Some(persistence) = &self.persistence {
            let durable = DurableJobStore {
                jobs: inner.jobs.values().cloned().collect(),
            };
            if let Err(error) = write_durable_store(&persistence.path, &durable) {
                inner.jobs.insert(job_id, prior);
                return Err(error);
            }
        }
        Ok(())
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
        match crate::core::process::linux_process_starttime_ticks(pid) {
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
            validate_transition(stored.job.status, JobStatus::Running)?;
            let local_child = stored.local_child.as_mut().ok_or_else(|| {
                Error::internal_unexpected("local child spawned without a durable reservation")
            })?;
            local_child.process = Some(LocalChildProcessIdentity {
                pid,
                process_group_id,
                discriminator: discriminator.clone(),
            });
            let prior = stored.clone();
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
            let durable = DurableJobStore {
                jobs: inner.jobs.values().cloned().collect(),
            };
            if let Err(error) = write_durable_store(&persistence.path, &durable) {
                *inner.jobs.get_mut(&job_id).expect("job exists") = prior;
                return Err(error);
            }
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
                match crate::core::process::linux_process_starttime_ticks(process.pid) {
                    Ok(Some(actual)) if actual == *ticks => return LocalChildLiveness::Live,
                    Ok(_) => LocalChildLiveness::Dead,
                    Err(evidence) => return LocalChildLiveness::Unsupported(evidence),
                }
            }
            LocalChildStartDiscriminator::Unsupported { evidence } => {
                if crate::core::process::pid_is_running(process.pid) {
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
                return match crate::core::process::isolated_process_group_is_running(pgid) {
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

pub(super) struct RecoveredTerminalJob {
    status: JobStatus,
    terminal_result: Value,
    run_id: String,
    artifacts: Vec<JobArtifactMetadata>,
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

/// A remote runner workload records its agent-task run ID in a typed execution
/// envelope. That durable run is authoritative after the runner child exits.
fn recovered_terminal_agent_task_result(stored: &StoredJob) -> Option<RecoveredTerminalJob> {
    let run_id = stored
        .remote_runner
        .as_ref()?
        .request
        .runner_workload
        .as_ref()?
        .agent_task
        .as_ref()?
        .run_id
        .trim()
        .to_string();
    if run_id.is_empty() {
        return None;
    }

    let result = agent_task_service::terminal_run_result(&run_id).ok()??;
    let status = match result.value.status {
        AgentTaskAggregateStatus::Succeeded => JobStatus::Succeeded,
        AgentTaskAggregateStatus::Cancelled => JobStatus::Cancelled,
        AgentTaskAggregateStatus::PartialFailure | AgentTaskAggregateStatus::Failed => {
            JobStatus::Failed
        }
    };
    let artifacts = result
        .value
        .artifact_bindings
        .iter()
        .map(|binding| JobArtifactMetadata {
            id: binding.artifact_id.clone(),
            name: binding.name.clone(),
            path: binding.path.clone(),
            url: binding.url.clone(),
            mime: None,
            size_bytes: None,
            sha256: binding.sha256.clone(),
            content_base64: None,
            metadata: Some(serde_json::json!({
                "kind": binding.kind,
                "task_id": binding.task_id,
                "durable_run_id": run_id,
            })),
        })
        .collect();
    Some(RecoveredTerminalJob {
        status,
        terminal_result: serde_json::json!({
            "kind": "agent_task_aggregate",
            "run_id": &run_id,
            "exit_code": result.exit_code,
            "aggregate": result.value,
        }),
        run_id,
        artifacts,
    })
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
        let actual = crate::core::process::linux_process_starttime_ticks(std::process::id())
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
        assert!(crate::core::process::pid_is_running(std::process::id()));
    }
}
