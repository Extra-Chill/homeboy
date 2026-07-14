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
    DaemonActiveJobRecoveryDisposition, DaemonActiveJobRecoveryEvidence, DaemonLeaseJobDiagnostics,
    DaemonLinkedDurableRunState, Job, JobEvent, JobEventKind, JobStatus,
    LeaselessOrphanAffectedJob, LeaselessOrphanJobDiagnostics,
};
use crate::core::agent_task_scheduler::AgentTaskAggregateStatus;
use crate::core::agent_task_service;
use crate::core::error::{Error, Result};
use crate::core::process::pid_is_running;
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

    /// Read active-job recovery evidence without reconciling or persisting jobs.
    pub fn active_daemon_job_recovery_evidence(
        &self,
        current_lease_id: Option<&str>,
        pid_is_alive: impl Fn(u32) -> bool,
    ) -> Vec<DaemonActiveJobRecoveryEvidence> {
        self.active_daemon_job_recovery_evidence_with_linked_durable_run_resolver(
            current_lease_id,
            pid_is_alive,
            resolve_linked_durable_run,
        )
    }

    pub(super) fn active_daemon_job_recovery_evidence_with_linked_durable_run_resolver(
        &self,
        current_lease_id: Option<&str>,
        pid_is_alive: impl Fn(u32) -> bool,
        resolve_linked: impl Fn(&StoredJob) -> LinkedDurableRunResolution,
    ) -> Vec<DaemonActiveJobRecoveryEvidence> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut evidence = inner
            .jobs
            .values()
            .filter_map(|stored| {
                let job = &stored.job;
                if !matches!(job.status, JobStatus::Queued | JobStatus::Running) {
                    return None;
                }
                let terminal_evidence =
                    recovered_terminal_from_result(&stored.events).map(|(status, _)| status);
                let linked = resolve_linked(stored);
                let (
                    linked_durable_run_id,
                    linked_durable_run_state,
                    linked_durable_run_terminal_status,
                ) = match &linked {
                    LinkedDurableRunResolution::None => (None, None, None),
                    LinkedDurableRunResolution::Terminal(result) => (
                        Some(result.run_id.clone()),
                        Some(DaemonLinkedDurableRunState::Terminal),
                        Some(result.status),
                    ),
                    LinkedDurableRunResolution::Active(run_id) => (
                        Some(run_id.clone()),
                        Some(DaemonLinkedDurableRunState::Active),
                        None,
                    ),
                    LinkedDurableRunResolution::Unresolved(run_id) => (
                        Some(run_id.clone()),
                        Some(DaemonLinkedDurableRunState::Unresolved),
                        None,
                    ),
                };
                let child_pid = last_progress_child_pid(&stored.events);
                let child_started_at = last_progress_child_started_at(&stored.events);
                let child_is_live = child_pid.is_some_and(|pid| {
                    pid_is_alive(pid) && child_started_at.as_deref().is_none_or(|expected| {
                        crate::core::engine::invocation::InvocationChildRecord::process_started_at(
                            pid,
                        )
                        .as_deref()
                            == Some(expected)
                    })
                });
                let disposition = if matches!(
                    linked,
                    LinkedDurableRunResolution::Active(_)
                        | LinkedDurableRunResolution::Unresolved(_)
                ) {
                    DaemonActiveJobRecoveryDisposition::BlockingAmbiguous
                } else if terminal_evidence.is_some()
                    || linked_durable_run_terminal_status.is_some()
                {
                    DaemonActiveJobRecoveryDisposition::TerminalEvidence
                } else if child_is_live {
                    DaemonActiveJobRecoveryDisposition::ProtectedLive
                } else if child_pid.is_some() || child_started_at.is_some() {
                    DaemonActiveJobRecoveryDisposition::DeadChild
                } else if current_lease_id
                    .is_some_and(|lease| job.daemon_lease_id.as_deref() == Some(lease))
                {
                    DaemonActiveJobRecoveryDisposition::MissingChildIdentityRecoverable
                } else {
                    DaemonActiveJobRecoveryDisposition::BlockingAmbiguous
                };
                Some(DaemonActiveJobRecoveryEvidence {
                    job_id: job.id,
                    operation: job.operation.clone(),
                    status: job.status,
                    daemon_lease_id: job.daemon_lease_id.clone(),
                    created_at_ms: job.created_at_ms,
                    updated_at_ms: job.updated_at_ms,
                    started_at_ms: job.started_at_ms,
                    terminal_evidence,
                    child_pid,
                    child_started_at,
                    linked_durable_run_id,
                    linked_durable_run_state,
                    linked_durable_run_terminal_status,
                    disposition,
                })
            })
            .collect::<Vec<_>>();
        evidence.sort_by_key(|job| (job.created_at_ms, job.job_id));
        evidence
    }

    pub fn reconcile_dead_daemon_lease_jobs(
        &self,
        expected_lease_id: &str,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs(expected_lease_id, &[])
    }

    /// Reconcile an exact dead daemon lease after the operator has separately
    /// proved each listed no-PID child is gone.
    pub fn reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs(
        &self,
        expected_lease_id: &str,
        confirmed_no_pid_job_ids: &[Uuid],
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result_with_no_pid_recovery(
            expected_lease_id,
            pid_is_running,
            |_| None,
            resolve_linked_durable_run,
            confirmed_no_pid_job_ids,
            false,
        )
    }

    /// Explicit operator recovery for legacy records which predate child identity.
    /// Callers must first prove the exact daemon lease PID is dead and no owner remains.
    pub fn reconcile_dead_daemon_lease_jobs_allow_missing_child_identity(
        &self,
        expected_lease_id: &str,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result_with_no_pid_recovery(
            expected_lease_id,
            pid_is_running,
            |_| None,
            resolve_linked_durable_run,
            &[],
            true,
        )
    }

    #[cfg(test)]
    pub(super) fn reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs_and_linked_resolver(
        &self,
        expected_lease_id: &str,
        confirmed_no_pid_job_ids: &[Uuid],
        pid_is_alive: impl Fn(u32) -> bool,
        resolve_linked: impl Fn(&StoredJob) -> LinkedDurableRunResolution,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result_with_no_pid_recovery(
            expected_lease_id,
            pid_is_alive,
            |_| None,
            resolve_linked,
            confirmed_no_pid_job_ids,
            false,
        )
    }

    pub(super) fn reconcile_dead_daemon_lease_jobs_with_child_liveness(
        &self,
        expected_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result_with_no_pid_recovery(
            expected_lease_id,
            pid_is_alive,
            |_| None,
            resolve_linked_durable_run,
            &[],
            false,
        )
    }

    pub(super) fn reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
        &self,
        expected_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
        terminal_child_result: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result_with_no_pid_recovery(
            expected_lease_id,
            pid_is_alive,
            terminal_child_result,
            resolve_linked_durable_run,
            &[],
            false,
        )
    }

    fn reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result_with_no_pid_recovery(
        &self,
        expected_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
        terminal_child_result: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
        resolve_linked: impl Fn(&StoredJob) -> LinkedDurableRunResolution,
        confirmed_no_pid_job_ids: &[Uuid],
        allow_missing_child_identity: bool,
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
        let supplied_confirmation_count = confirmed_no_pid_job_ids.len();
        let confirmed_no_pid_job_ids = confirmed_no_pid_job_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if confirmed_no_pid_job_ids.len() != supplied_confirmation_count {
            return Err(Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                "each --confirm-untracked-child-dead job ID must be supplied once",
                Some(expected_lease_id.to_string()),
                None,
            ));
        }
        let mut dispositions = Vec::with_capacity(diagnostics.matching_job_ids.len());
        let mut ambiguous_job_ids = Vec::new();
        for job_id in &diagnostics.matching_job_ids {
            let stored = inner.jobs.get(job_id).expect("diagnosed job exists");
            let disposition = match resolve_linked(stored) {
                LinkedDurableRunResolution::Terminal(recovered) => {
                    DeadLeaseJobDisposition::RecoveredLinkedRun(recovered)
                }
                LinkedDurableRunResolution::Active(_) => {
                    diagnostics.protected_job_ids.push(*job_id);
                    DeadLeaseJobDisposition::ProtectedLive
                }
                LinkedDurableRunResolution::Unresolved(run_id) => {
                    return Err(Error::validation_invalid_argument(
                        "expected-lease-id",
                        format!("refusing dead-daemon recovery because linked durable run `{run_id}` for job `{job_id}` cannot be safely resolved"),
                        Some(expected_lease_id.to_string()),
                        None,
                    ));
                }
                LinkedDurableRunResolution::None => {
                    if let Some((status, exit_code)) =
                        recovered_terminal_from_result(&stored.events)
                    {
                        DeadLeaseJobDisposition::RecoveredOuterResult(status, exit_code)
                    } else if let Some(recovered) = terminal_child_result(stored) {
                        DeadLeaseJobDisposition::RecoveredLinkedRun(recovered)
                    } else if let Some(pid) = last_progress_child_pid(&stored.events) {
                        let child_is_live = pid_is_alive(pid)
                            && last_progress_child_started_at(&stored.events).is_none_or(
                                |expected| {
                                    crate::core::engine::invocation::InvocationChildRecord::process_started_at(pid)
                                        .as_deref()
                                        == Some(expected.as_str())
                                },
                            );
                        if child_is_live {
                            diagnostics.protected_job_ids.push(*job_id);
                            DeadLeaseJobDisposition::ProtectedLive
                        } else {
                            DeadLeaseJobDisposition::TerminalizeDead
                        }
                    } else {
                        if allow_missing_child_identity {
                            DeadLeaseJobDisposition::TerminalizeDead
                        } else {
                            ambiguous_job_ids.push(*job_id);
                            if confirmed_no_pid_job_ids.contains(job_id) {
                                DeadLeaseJobDisposition::TerminalizeOperatorConfirmedNoPid
                            } else {
                                continue;
                            }
                        }
                    }
                }
            };
            dispositions.push((*job_id, disposition));
        }
        let ambiguous_job_ids = ambiguous_job_ids
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if !confirmed_no_pid_job_ids.is_subset(&ambiguous_job_ids) {
            let invalid_job_ids = confirmed_no_pid_job_ids
                .difference(&ambiguous_job_ids)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                format!(
                    "refusing dead-daemon recovery: confirmed job(s) {invalid_job_ids} are not unresolved active jobs without a recorded child PID for lease `{expected_lease_id}`"
                ),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }
        let missing_confirmation_job_ids = ambiguous_job_ids
            .difference(&confirmed_no_pid_job_ids)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !missing_confirmation_job_ids.is_empty() {
            return Err(Error::validation_invalid_argument(
                "expected-lease-id",
                format!(
                    "refusing automatic dead-daemon recovery: active job(s) {} have no authoritative terminal result or recorded child PID; inspect durable lifecycle/process evidence with `homeboy daemon status` and repeat --confirm-untracked-child-dead <JOB_ID> for each independently proven-dead child before retrying",
                    missing_confirmation_job_ids.join(", "),
                ),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }
        if !diagnostics.protected_job_ids.is_empty() {
            diagnostics.protected_job_ids.sort();
            return Ok(diagnostics);
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
            if matches!(disposition, DeadLeaseJobDisposition::ProtectedLive) {
                continue;
            }
            let operator_confirmed_no_pid = matches!(
                disposition,
                DeadLeaseJobDisposition::TerminalizeOperatorConfirmedNoPid
            );
            let reason = if operator_confirmed_no_pid {
                "operator confirmed the untracked child process was dead after daemon lease loss"
                    .to_string()
            } else {
                "daemon lease owner process was not running".to_string()
            };
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
                        "reason": if operator_confirmed_no_pid { "operator_confirmed_untracked_child_dead_after_dead_daemon_lease" } else { "dead_daemon_lease" },
                        "classification": classification,
                        "daemon_lease_id": expected_lease_id,
                        "operator_confirmation": operator_confirmed_no_pid,
                    }),
                ),
                (
                    JobEventKind::Status,
                    "job marked failed after dead daemon lease".to_string(),
                    serde_json::json!({
                        "status": JobStatus::Failed,
                        "reason": if operator_confirmed_no_pid { "operator_confirmed_untracked_child_dead_after_dead_daemon_lease" } else { "dead_daemon_lease" },
                        "daemon_lease_id": expected_lease_id,
                        "operator_confirmation": operator_confirmed_no_pid,
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
        drop(inner);
        self.persist()?;
        Ok(diagnostics)
    }

    /// Terminalize all active jobs after an operator has proved that no daemon
    /// owns a store whose current daemon lease is missing. Historical job leases
    /// remain evidence; they cannot prove a current owner or be adopted as one.
    pub fn reconcile_leaseless_orphan_jobs(&self) -> Result<LeaselessOrphanJobDiagnostics> {
        self.reconcile_leaseless_orphan_jobs_with_child_liveness(pid_is_running)
    }

    fn reconcile_leaseless_orphan_jobs_with_child_liveness(
        &self,
        pid_is_alive: impl Fn(u32) -> bool,
    ) -> Result<LeaselessOrphanJobDiagnostics> {
        let mut diagnostics = LeaselessOrphanJobDiagnostics::default();
        {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            for stored in inner.jobs.values() {
                if !matches!(stored.job.status, JobStatus::Queued | JobStatus::Running) {
                    continue;
                }
                if last_progress_child_pid(&stored.events).is_some_and(&pid_is_alive) {
                    diagnostics.protected_job_ids.push(stored.job.id);
                    continue;
                }
                let original_daemon_lease_id = stored.job.daemon_lease_id.clone();
                if let Some(lease_id) = &original_daemon_lease_id {
                    diagnostics.historical_lease_ids.push(lease_id.clone());
                }
                diagnostics.reconciled_job_ids.push(stored.job.id);
                diagnostics.affected_jobs.push(LeaselessOrphanAffectedJob {
                    job_id: stored.job.id,
                    original_daemon_lease_id,
                });
            }
        }
        diagnostics.reconciled_job_ids.sort();
        diagnostics.affected_jobs.sort_by_key(|job| job.job_id);
        diagnostics.historical_lease_ids.sort();
        diagnostics.historical_lease_ids.dedup();
        diagnostics.protected_job_ids.sort();

        let now = timestamp_ms();
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        for job_id in &diagnostics.reconciled_job_ids {
            let stored = inner.jobs.get_mut(job_id).expect("diagnosed job exists");
            let original_daemon_lease_id = stored.job.daemon_lease_id.clone();
            stored.job.status = JobStatus::Failed;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason = Some("control plane lost before job completion".to_string());
            let classification = stale_after_restart_classification(stored);
            for (kind, message, data) in [
                (
                    JobEventKind::Error,
                    "job marked failed after missing-lease control-plane loss; retry through its original command".to_string(),
                    serde_json::json!({"reason": "leaseless_orphan_reconciliation", "classification": classification, "original_daemon_lease_id": original_daemon_lease_id, "retry_guidance": "Retry eligible work through its original command or workflow."}),
                ),
                (
                    JobEventKind::Status,
                    "job marked failed after missing-lease control-plane loss".to_string(),
                    serde_json::json!({"status": JobStatus::Failed, "reason": "leaseless_orphan_reconciliation", "original_daemon_lease_id": original_daemon_lease_id}),
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

    pub(crate) fn run_background_deferred_start_with_source_snapshot_metadata_and_path_materialization_plan<
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
        self.run_background_with_start_policy(
            operation,
            source_snapshot,
            metadata,
            path_materialization_plan,
            false,
            run,
        )
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

    pub(crate) fn start_with_child_identity(
        &self,
        job_id: Uuid,
        pid: u32,
        started_at: String,
    ) -> Result<Job> {
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let (prior, started) = {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            validate_transition(stored.job.status, JobStatus::Running)?;
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
                message: Some("runner child spawned".to_string()),
                data: Some(serde_json::json!({ "phase": "spawned", "process": { "root_pid": pid, "started_at": started_at } })),
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
    TerminalizeDead,
    TerminalizeOperatorConfirmedNoPid,
}

#[derive(Clone)]
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

#[derive(Clone)]
pub(super) enum LinkedDurableRunResolution {
    None,
    Terminal(RecoveredTerminalJob),
    Active(String),
    Unresolved(String),
}

fn resolve_linked_durable_run(stored: &StoredJob) -> LinkedDurableRunResolution {
    let Some(run_id) = linked_agent_task_run_id(stored) else {
        return LinkedDurableRunResolution::None;
    };
    let result = match agent_task_service::terminal_run_result(&run_id) {
        Ok(Some(result)) => result,
        Ok(None) => return LinkedDurableRunResolution::Active(run_id),
        Err(_) => return LinkedDurableRunResolution::Unresolved(run_id),
    };
    let status = match result.value.status {
        AgentTaskAggregateStatus::Succeeded | AgentTaskAggregateStatus::CandidateRecoverable => {
            JobStatus::Succeeded
        }
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
    LinkedDurableRunResolution::Terminal(RecoveredTerminalJob {
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

fn linked_agent_task_run_id(stored: &StoredJob) -> Option<String> {
    let run_id = stored
        .remote_runner
        .as_ref()?
        .request
        .run_ref_metadata()?
        .get("agent_task_run_id")?
        .as_str()?
        .trim()
        .to_string();
    (!run_id.is_empty()).then_some(run_id)
}

/// The reverse runner records the executing child in periodic progress
/// heartbeats. A live child is still authoritative even after its daemon lease
/// owner exits, so reconciliation must leave its durable run files intact.
fn last_progress_child_pid(events: &[JobEvent]) -> Option<u32> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Progress)
        .and_then(|event| event.data.as_ref())
        .and_then(|data| data.get("process"))
        .and_then(|process| process.get("root_pid"))
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
}

fn last_progress_child_started_at(events: &[JobEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Progress)
        .and_then(|event| event.data.as_ref())
        .and_then(|data| data.get("process"))
        .and_then(|process| process.get("started_at"))
        .and_then(Value::as_str)
        .map(str::to_string)
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

    pub(crate) fn start_with_child_identity(&self, pid: u32, started_at: String) -> Result<Job> {
        self.store
            .start_with_child_identity(self.job_id, pid, started_at)
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
