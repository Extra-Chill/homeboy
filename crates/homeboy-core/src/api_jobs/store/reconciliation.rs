//! Daemon-lease reconciliation and recovery for `JobStore`.
//!
//! Split from the `api_jobs::store` god file: the daemon-lease job diagnostics,
//! active-job recovery evidence, and the family of dead / exact-loss / terminal
//! / leaseless reconciliation routines. Implemented as a second `impl JobStore`
//! block over the same type; `use super::*` inherits the parent module's local
//! helper types/functions and its private `JobStore` fields remain reachable.

use std::sync::atomic::Ordering;

use uuid::Uuid;

use super::super::persistence::{
    apply_event_retention, recovered_terminal_from_result, stale_after_restart_classification,
    timestamp_ms,
};
use super::super::types::{
    DaemonActiveJobRecoveryDisposition, DaemonActiveJobRecoveryEvidence, DaemonLeaseJobDiagnostics,
    DaemonLinkedDurableRunState, Job, JobEvent, JobEventKind, JobStatus,
    LeaselessOrphanAffectedJob, LeaselessOrphanJobDiagnostics,
};
use super::JobStore;
use super::*;
use crate::error::{Error, Result};

impl JobStore {
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

    /// Read active-job recovery evidence without reconciling or persisting jobs.
    /// Typed local-child identity is authoritative; legacy progress payloads are
    /// intentionally not used to infer ownership.
    pub fn active_daemon_job_recovery_evidence(
        &self,
        current_lease_id: Option<&str>,
        _pid_is_alive: impl Fn(u32) -> bool,
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
                let child_pid = stored
                    .local_child
                    .as_ref()
                    .and_then(|child| child.process.as_ref())
                    .map(|process| process.pid);
                let disposition = if terminal_evidence.is_some() {
                    DaemonActiveJobRecoveryDisposition::TerminalEvidence
                } else if let Some(child) = stored.local_child.as_ref() {
                    match local_child_liveness(child) {
                        LocalChildLiveness::Dead => DaemonActiveJobRecoveryDisposition::DeadChild,
                        LocalChildLiveness::Live => {
                            DaemonActiveJobRecoveryDisposition::ProtectedLive
                        }
                        LocalChildLiveness::Unsupported(_) => {
                            DaemonActiveJobRecoveryDisposition::BlockingAmbiguous
                        }
                    }
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
                    child_started_at: None,
                    linked_durable_run_id: None,
                    linked_durable_run_state: None,
                    linked_durable_run_terminal_status: None,
                    disposition,
                })
            })
            .collect::<Vec<_>>();
        evidence.sort_by_key(|job| (job.created_at_ms, job.job_id));
        evidence
    }

    #[cfg(test)]
    pub(crate) fn active_daemon_job_recovery_evidence_with_linked_durable_run_resolver(
        &self,
        current_lease_id: Option<&str>,
        pid_is_alive: impl Fn(u32) -> bool,
        resolve_linked: impl Fn(&StoredJob) -> LinkedDurableRunResolution,
    ) -> Vec<DaemonActiveJobRecoveryEvidence> {
        let mut evidence = self.active_daemon_job_recovery_evidence(current_lease_id, pid_is_alive);
        let inner = self.inner.lock().expect("job store mutex poisoned");
        for item in &mut evidence {
            let stored = inner.jobs.get(&item.job_id).expect("evidence job exists");
            match resolve_linked(stored) {
                LinkedDurableRunResolution::None => {}
                LinkedDurableRunResolution::Terminal(recovered) => {
                    item.linked_durable_run_id = Some(recovered.run_id);
                    item.linked_durable_run_state = Some(DaemonLinkedDurableRunState::Terminal);
                    item.linked_durable_run_terminal_status = Some(recovered.status);
                    item.disposition = DaemonActiveJobRecoveryDisposition::TerminalEvidence;
                }
                LinkedDurableRunResolution::Active(run_id) => {
                    item.linked_durable_run_id = Some(run_id);
                    item.linked_durable_run_state = Some(DaemonLinkedDurableRunState::Active);
                    item.disposition = DaemonActiveJobRecoveryDisposition::BlockingAmbiguous;
                }
                LinkedDurableRunResolution::Unresolved(run_id) => {
                    item.linked_durable_run_id = Some(run_id);
                    item.linked_durable_run_state = Some(DaemonLinkedDurableRunState::Unresolved);
                    item.disposition = DaemonActiveJobRecoveryDisposition::BlockingAmbiguous;
                }
            }
        }
        evidence
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

    /// Terminalize an operator-supplied, complete set of PID-less jobs after a
    /// daemon's unexpected death has been proven by the lifecycle controller.
    /// This is deliberately separate from automatic reconciliation: every
    /// active job must be named, owned by the exact lease, and have no child
    /// identity that could contradict the operator's absence confirmation.
    pub fn reconcile_exact_daemon_loss_jobs(
        &self,
        expected_lease_id: &str,
        expected_job_ids: &[Uuid],
        daemon_pid: u32,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        let expected = expected_job_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if expected.is_empty() || expected.len() != expected_job_ids.len() {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "dead-daemon recovery requires a non-empty, unique exact active job set",
                None,
                None,
            ));
        }
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let active = inner
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .map(|stored| stored.job.id)
            .collect::<std::collections::BTreeSet<_>>();
        if active != expected {
            return Err(Error::validation_invalid_argument(
                "job_id",
                format!(
                    "dead-daemon recovery job IDs must name the exact active durable-job set; expected {:?}, found {:?}",
                    expected, active
                ),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }
        for job_id in &expected {
            let stored = inner.jobs.get(job_id).expect("active job exists");
            if stored.job.daemon_lease_id.as_deref() != Some(expected_lease_id) {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    format!("job `{job_id}` is not owned by daemon lease `{expected_lease_id}`"),
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if stored
                .local_child
                .as_ref()
                .and_then(|child| child.process.as_ref())
                .is_some()
            {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    format!("job `{job_id}` has persisted child-process evidence; refusing operator no-PID recovery"),
                    Some(job_id.to_string()),
                    None,
                ));
            }
        }
        let now = timestamp_ms();
        for job_id in &expected {
            let stored = inner.jobs.get_mut(job_id).expect("active job exists");
            stored.job.status = JobStatus::Failed;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason = Some(
                "daemon lost after unexpected termination; operator confirmed workloads absent"
                    .to_string(),
            );
            let data = serde_json::json!({
                "status": JobStatus::Failed,
                "reason": "operator_confirmed_daemon_loss_after_unexpected_termination",
                "daemon_lease_id": expected_lease_id,
                "daemon_pid": daemon_pid,
                "operator_confirmed_workload_processes_absent": true,
                "exact_active_job_set": expected,
            });
            for (kind, message) in [
                (
                    JobEventKind::Error,
                    "daemon lost after unexpected termination",
                ),
                (
                    JobEventKind::Status,
                    "job marked failed after exact operator daemon-loss reconciliation",
                ),
            ] {
                let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                stored.events.push(JobEvent {
                    sequence,
                    job_id: *job_id,
                    kind,
                    timestamp_ms: now,
                    message: Some(message.to_string()),
                    data: Some(data.clone()),
                });
            }
            apply_event_retention(&mut stored.events, self.event_retention_limit());
            stored.job.event_count = stored.events.len();
        }
        drop(inner);
        self.persist()?;
        Ok(DaemonLeaseJobDiagnostics {
            expected_lease_id: expected_lease_id.to_string(),
            matching_job_ids: expected.into_iter().collect(),
            ..DaemonLeaseJobDiagnostics::default()
        })
    }

    /// Terminalize one explicitly confirmed pre-spawn reservation after its
    /// daemon owner is proven dead. This intentionally refuses every other
    /// active job so orphan adoption cannot infer ownership or bulk-recover.
    pub fn recover_expired_pidless_reservation_for_dead_daemon_lease(
        &self,
        expected_lease_id: &str,
        confirmed_job_ids: &[Uuid],
    ) -> Result<DaemonLeaseJobDiagnostics> {
        let [job_id] = confirmed_job_ids else {
            return Err(Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                "exactly one job ID must be confirmed for expired PID-less reservation recovery",
                Some(expected_lease_id.to_string()),
                None,
            ));
        };
        let now = timestamp_ms();
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        let active_job_ids = inner
            .jobs
            .values()
            .filter(|stored| {
                matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                    && stored.job.daemon_lease_id.as_deref() == Some(expected_lease_id)
            })
            .map(|stored| stored.job.id)
            .collect::<Vec<_>>();
        if active_job_ids.as_slice() != [*job_id] {
            return Err(Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                format!(
                    "expired PID-less reservation recovery requires the only active job for lease `{expected_lease_id}`; active jobs: {}",
                    active_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                ),
                Some(job_id.to_string()),
                None,
            ));
        }
        let stored = inner
            .jobs
            .get_mut(job_id)
            .expect("confirmed active job exists");
        let reservation = stored.local_child.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                "confirmed job has no local child reservation",
                Some(job_id.to_string()),
                None,
            )
        })?;
        if stored.job.status != JobStatus::Queued
            || reservation.process.is_some()
            || reservation
                .reservation_expires_at_ms
                .is_none_or(|expires_at| expires_at > now)
        {
            return Err(Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                "confirmed job is not an expired PID-less pre-spawn reservation",
                Some(job_id.to_string()),
                None,
            ));
        }
        let terminal_result = serde_json::json!({
            "status": JobStatus::Failed,
            "reason": "operator_confirmed_expired_pidless_reservation_after_dead_daemon_lease",
            "retryable": true,
            "expected_lease_id": expected_lease_id,
            "reservation_id": reservation.reservation_id,
            "reservation_expires_at_ms": reservation.reservation_expires_at_ms,
            "operator_confirmation": true,
        });
        stored.job.status = JobStatus::Failed;
        stored.job.updated_at_ms = now;
        stored.job.finished_at_ms = Some(now);
        stored.job.stale_reason =
            Some("local child reservation lease expired before spawn".to_string());
        for (kind, message) in [
            (
                JobEventKind::Error,
                "expired PID-less reservation recovered after dead daemon lease",
            ),
            (
                JobEventKind::Result,
                "retryable terminal reservation recovery",
            ),
            (
                JobEventKind::Status,
                "job marked failed after explicit expired reservation recovery",
            ),
        ] {
            let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
            stored.events.push(JobEvent {
                sequence,
                job_id: *job_id,
                kind,
                timestamp_ms: now,
                message: Some(message.to_string()),
                data: Some(terminal_result.clone()),
            });
        }
        apply_event_retention(&mut stored.events, self.event_retention_limit());
        stored.job.event_count = stored.events.len();
        drop(inner);
        self.persist()?;
        Ok(DaemonLeaseJobDiagnostics {
            expected_lease_id: expected_lease_id.to_string(),
            matching_job_ids: vec![*job_id],
            ..DaemonLeaseJobDiagnostics::default()
        })
    }

    /// Reconcile only jobs whose linked durable run has already reached a
    /// terminal state. This deliberately does not inspect, stop, or alter live
    /// children, so it is safe when a daemon's aggregate count includes both
    /// stale handoffs and genuine work.
    pub fn reconcile_terminal_linked_daemon_jobs(&self) -> Result<Vec<Uuid>> {
        self.reconcile_terminal_linked_daemon_jobs_with_resolver(
            recovered_terminal_agent_task_result,
        )
    }

    pub(crate) fn reconcile_terminal_linked_daemon_jobs_with_resolver(
        &self,
        resolve_terminal: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
    ) -> Result<Vec<Uuid>> {
        let terminal = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            inner
                .jobs
                .values()
                .filter(|stored| {
                    matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                })
                .filter_map(|stored| resolve_terminal(stored).map(|result| (stored.job.id, result)))
                .collect::<Vec<_>>()
        };

        let mut reconciled = Vec::with_capacity(terminal.len());
        for (job_id, result) in terminal {
            let now = timestamp_ms();
            let mut inner = self.inner.lock().expect("job store mutex poisoned");
            let stored = inner.jobs.get_mut(&job_id).expect("terminal job exists");
            stored.job.status = result.status;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason = None;
            for artifact in result.artifacts {
                if !stored
                    .job
                    .artifacts
                    .iter()
                    .any(|existing| existing.id == artifact.id)
                {
                    stored.job.artifacts.push(artifact);
                }
            }
            drop(inner);
            self.persist()?;
            reconciled.push(job_id);
        }
        Ok(reconciled)
    }

    #[cfg(test)]
    pub(crate) fn reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs(
        &self,
        expected_lease_id: &str,
        confirmed_no_pid_job_ids: &[Uuid],
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs_and_linked_resolver(
            expected_lease_id,
            confirmed_no_pid_job_ids,
            |_| false,
            |stored| {
                stored
                    .remote_runner
                    .as_ref()
                    .and_then(|remote| remote.request.lifecycle.as_ref())
                    .and_then(|lifecycle| lifecycle.durable_run_id.clone())
                    .map(LinkedDurableRunResolution::Unresolved)
                    .unwrap_or(LinkedDurableRunResolution::None)
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn reconcile_dead_daemon_lease_jobs_with_confirmed_no_pid_jobs_and_linked_resolver(
        &self,
        expected_lease_id: &str,
        confirmed_no_pid_job_ids: &[Uuid],
        pid_is_alive: impl Fn(u32) -> bool,
        resolve_linked: impl Fn(&StoredJob) -> LinkedDurableRunResolution,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let matching = inner
            .jobs
            .values()
            .filter(|stored| {
                matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                    && stored.job.daemon_lease_id.as_deref() == Some(expected_lease_id)
            })
            .map(|stored| (stored.job.id, resolve_linked(stored)))
            .collect::<Vec<_>>();
        drop(inner);
        if let Some((job_id, LinkedDurableRunResolution::Unresolved(run_id))) = matching
            .iter()
            .find(|(_, resolution)| matches!(resolution, LinkedDurableRunResolution::Unresolved(_)))
        {
            return Err(Error::validation_invalid_argument(
                "expected-lease-id",
                format!("refusing dead-daemon recovery because linked durable run `{run_id}` for job `{job_id}` cannot be safely resolved"),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }
        let protected_job_ids = matching
            .iter()
            .filter_map(|(job_id, resolution)| {
                matches!(resolution, LinkedDurableRunResolution::Active(_)).then_some(*job_id)
            })
            .collect::<Vec<_>>();
        if !protected_job_ids.is_empty() {
            return Ok(DaemonLeaseJobDiagnostics {
                expected_lease_id: expected_lease_id.to_string(),
                matching_job_ids: matching.iter().map(|(job_id, _)| *job_id).collect(),
                protected_job_ids,
                ..DaemonLeaseJobDiagnostics::default()
            });
        }
        let live_progress_job_ids = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            matching
                .iter()
                .filter_map(|(job_id, resolution)| {
                    if !matches!(resolution, LinkedDurableRunResolution::None) {
                        return None;
                    }
                    let stored = inner.jobs.get(job_id).expect("job exists");
                    stored
                        .events
                        .iter()
                        .rev()
                        .find_map(|event| {
                            event
                                .data
                                .as_ref()
                                .and_then(|data| data["process"]["root_pid"].as_u64())
                                .and_then(|pid| u32::try_from(pid).ok())
                        })
                        .filter(|pid| pid_is_alive(*pid))
                        .map(|_| *job_id)
                })
                .collect::<Vec<_>>()
        };
        if !live_progress_job_ids.is_empty() {
            return Ok(DaemonLeaseJobDiagnostics {
                expected_lease_id: expected_lease_id.to_string(),
                matching_job_ids: matching.iter().map(|(job_id, _)| *job_id).collect(),
                protected_job_ids: live_progress_job_ids,
                ..DaemonLeaseJobDiagnostics::default()
            });
        }
        for (job_id, resolution) in &matching {
            if let LinkedDurableRunResolution::Terminal(recovered) = resolution {
                let mut inner = self.inner.lock().expect("job store mutex poisoned");
                let stored = inner.jobs.get_mut(job_id).expect("job exists");
                stored.job.status = recovered.status;
                stored.job.finished_at_ms = Some(timestamp_ms());
                stored.job.updated_at_ms = stored.job.finished_at_ms.expect("timestamp");
                drop(inner);
                self.persist()?;
            }
        }
        if matching
            .iter()
            .any(|(_, resolution)| matches!(resolution, LinkedDurableRunResolution::Terminal(_)))
        {
            return Ok(DaemonLeaseJobDiagnostics {
                expected_lease_id: expected_lease_id.to_string(),
                matching_job_ids: matching.iter().map(|(job_id, _)| *job_id).collect(),
                ..DaemonLeaseJobDiagnostics::default()
            });
        }
        let unresolved_job_ids = matching
            .iter()
            .filter_map(|(job_id, resolution)| {
                matches!(resolution, LinkedDurableRunResolution::None).then_some(*job_id)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let confirmed_job_ids = confirmed_no_pid_job_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if confirmed_job_ids != unresolved_job_ids {
            let invalid = confirmed_job_ids
                .symmetric_difference(&unresolved_job_ids)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::validation_invalid_argument(
                "confirm_untracked_child_dead",
                format!("exact confirmation is required for unresolved active job(s) {invalid}"),
                Some(expected_lease_id.to_string()),
                None,
            ));
        }
        if !confirmed_job_ids.is_empty() {
            for job_id in confirmed_job_ids {
                self.fail(job_id, "operator confirmed untracked child is dead")?;
                self.append_status_event_with_data(
                    job_id,
                    JobStatus::Failed,
                    "job marked failed after operator confirmation",
                    serde_json::json!({
                        "reason": "operator_confirmed_untracked_child_dead_after_dead_daemon_lease",
                        "operator_confirmation": true,
                    }),
                )?;
            }
            return Ok(DaemonLeaseJobDiagnostics {
                expected_lease_id: expected_lease_id.to_string(),
                matching_job_ids: matching.iter().map(|(job_id, _)| *job_id).collect(),
                ..DaemonLeaseJobDiagnostics::default()
            });
        }
        self.reconcile_dead_daemon_lease_jobs(expected_lease_id)
    }

    #[cfg(test)]
    pub(crate) fn reconcile_dead_daemon_lease_jobs_with_child_liveness(
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
    pub(crate) fn start_with_child_identity(
        &self,
        job_id: Uuid,
        pid: u32,
        _started_at: String,
    ) -> Result<Job> {
        self.reserve_local_child(job_id)?;
        self.start_with_reserved_child_identity(
            job_id,
            pid,
            None,
            LocalChildStartDiscriminator::Unsupported {
                evidence: "legacy test child identity".to_string(),
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn reconcile_dead_daemon_lease_jobs_with_child_liveness_and_terminal_result(
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

    pub(crate) fn active_runner_jobs(&self) -> Vec<super::super::types::ActiveRunnerJobSummary> {
        let now = timestamp_ms();
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut jobs: Vec<super::super::types::ActiveRunnerJobSummary> = inner
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .map(|stored| {
                stored
                    .remote_runner
                    .as_ref()
                    .map(|remote| {
                        super::super::summary::active_runner_job_summary(
                            &stored.job,
                            &remote.request,
                            now,
                        )
                    })
                    .or_else(|| {
                        stored.local_runner.as_ref().map(|local| {
                            super::super::summary::active_local_runner_job_summary(
                                &stored.job,
                                local,
                                now,
                            )
                        })
                    })
                    .unwrap_or_else(|| {
                        super::super::summary::active_daemon_job_summary(&stored.job, now)
                    })
            })
            .collect();
        jobs.sort_by_key(|job| (job.started_at_ms, job.job_id.clone()));
        jobs
    }

    /// The already-enqueued, non-terminal job for a controller `durable_run_id`,
    /// if one exists.
    ///
    /// A daemon `/exec` submission is not idempotent at the transport layer: a
    /// dropped connection or timeout can hide that the daemon already accepted
    /// the request. The controller-minted `durable_run_id` is a stable key for
    /// the unit of work, so the daemon can treat a resubmission carrying the same
    /// key as a no-op that returns the existing job instead of enqueuing a
    /// duplicate. Only `Queued`/`Running` jobs are considered — a terminal job
    /// for the same run id is finished, so a resubmission is a genuinely new
    /// attempt and must enqueue a fresh job.
    pub(crate) fn active_runner_job_for_durable_run_id(&self, durable_run_id: &str) -> Option<Job> {
        if durable_run_id.trim().is_empty() {
            return None;
        }
        let inner = self.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .filter(|stored| stored_job_durable_run_id(stored).as_deref() == Some(durable_run_id))
            // Deterministic across a resubmission race: the oldest active job for
            // the run id is the canonical one to return.
            .min_by_key(|stored| (stored.job.created_at_ms, stored.job.id))
            .map(|stored| stored.job.clone())
    }

    pub(crate) fn stale_runner_jobs(&self) -> Vec<super::super::types::ActiveRunnerJobSummary> {
        let now = timestamp_ms();
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut jobs: Vec<super::super::types::ActiveRunnerJobSummary> = inner
            .jobs
            .values()
            .filter(|stored| {
                stored.job.status == JobStatus::Failed && stored.job.stale_reason.is_some()
            })
            .filter_map(|stored| {
                let request = stored.remote_runner.as_ref()?.request.clone();
                Some(super::super::summary::active_runner_job_summary(
                    &stored.job,
                    &request,
                    now,
                ))
            })
            .collect();
        jobs.sort_by_key(|job| (job.updated_at_ms, job.job_id.clone()));
        jobs
    }
}
