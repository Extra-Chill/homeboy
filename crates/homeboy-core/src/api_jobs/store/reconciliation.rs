//! Daemon-lease reconciliation and recovery for `JobStore`.
//!
//! Split from the `api_jobs::store` god file: active-job recovery evidence,
//! dead-generation startup reconciliation, operator-attested absence, and
//! terminal-evidence reconciliation. Implemented as a second `impl JobStore`
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
    Job, JobEvent, JobEventKind, JobStatus,
};
use super::JobStore;
use super::*;
use crate::api_jobs::types::DaemonLinkedDurableRunState;
use crate::error::{Error, Result};

impl JobStore {
    /// Adopt every non-terminal pre-scope controller at startup.
    /// The caller holds the exclusive daemon owner lock: all current daemons
    /// stamp scopes, so no pre-scope owner can still be alive, regardless of lease.
    pub fn adopt_scopeless_controller_jobs(&self) -> Result<()> {
        let Some(scope) = self.daemon_scope.as_ref() else {
            return Ok(());
        };
        self.durable_transaction(|inner| {
            for stored in inner.jobs.values_mut().filter(|stored| {
                stored.daemon_scope.is_none()
                    && stored.controller_job.is_some()
                    && !stored.job.status.is_terminal()
            }) {
                let controller = stored.controller_job.as_ref().expect("controller exists");
                let invalid = crate::daemon::controller_job_driver::driver(
                    &controller.job_type,
                    controller.version,
                )
                .map_err(|_| "controller_driver_missing")
                .and_then(|driver| {
                    driver
                        .execution_owner(&controller.request, controller.checkpoint.as_ref())
                        .map_err(|_| "controller_request_invalid")
                });
                stored.daemon_scope = Some(scope.clone());
                let now = timestamp_ms();
                let reason = if let Err(reason) = invalid {
                    stored.job.status = JobStatus::Failed;
                    stored.job.finished_at_ms = Some(now);
                    stored.job.stale_reason = Some(reason.into());
                    reason
                } else {
                    "controller_adopted_after_owner_death"
                };
                stored.job.updated_at_ms = now;
                let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                stored.events.push(JobEvent {
                    sequence,
                    job_id: stored.job.id,
                    kind: JobEventKind::Status,
                    timestamp_ms: now,
                    message: Some("scope-less controller adopted after owner death".into()),
                    data: Some(serde_json::json!({
                        "reason": reason,
                        "status": stored.job.status,
                        "daemon_lease_id": stored.job.daemon_lease_id,
                    })),
                });
                apply_event_retention(&mut stored.events, self.event_retention_limit());
                stored.job.event_count = stored.events.len();
            }
            Ok(())
        })
    }

    /// Overlap requires a controller this store can advance, not merely a
    /// nonterminal row in the shared ledger. Unowned work requires repair.
    pub(crate) fn controller_instance_activity(&self, instance: &str) -> Result<Option<bool>> {
        let Some(job) = self.controller_job_for_key(instance) else {
            return Ok(None);
        };
        if job.status.is_terminal() {
            return Ok(Some(false));
        }
        let advanceable = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            inner.jobs.get(&job.id).is_some_and(|stored| {
                self.owns(stored)
                    && !matches!(
                        stored_job_execution_liveness(stored, &local_child_liveness).state,
                        Some(LocalChildLiveness::Unsupported(_))
                    )
            })
        };
        if advanceable {
            return Ok(Some(true));
        }
        Err(Error::validation_invalid_argument(
            "instance",
            "cadence blocked by an instance this daemon cannot advance; owner reconciliation required",
            Some(instance.to_string()),
            None,
        ))
    }

    /// Read active-job recovery evidence without reconciling or persisting jobs.
    /// Typed execution-owner identity is authoritative; legacy progress payloads are
    /// intentionally not used to infer ownership. A terminal linked durable run
    /// proves absence for a generic handoff, but driver-owned supervision must
    /// collect its own outcome: its launcher may outlive a linked attempt.
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
                // A job stamped with another daemon scope is that daemon's
                // work in the shared ledger (#16104), even when this scope has
                // no lease yet. Scope-less records keep the lease rule.
                if let (Some(own), Some(job_scope)) =
                    (self.daemon_scope.as_ref(), stored.daemon_scope.as_ref())
                {
                    if own != job_scope {
                        return None;
                    }
                }
                let terminal_evidence =
                    recovered_terminal_from_result(&stored.events).map(|(status, _)| status);
                let linked_durable_run_id = stored_job_durable_run_id(stored);
                let linked_terminal = linked_durable_run_id.as_deref().and_then(
                    super::super::agent_task_terminal_recovery::recovered_terminal_agent_task_job,
                );
                let linked_durable_run_state = linked_durable_run_id
                    .as_deref()
                    .and_then(super::super::agent_task_terminal_recovery::linked_durable_run_state);
                let ownership = stored_job_execution_liveness(stored, &local_child_liveness);
                // A driver's supervised launcher can outlive one linked attempt.
                // Only the driver can turn that attempt into the work's outcome.
                let terminal_evidence = terminal_evidence.or_else(|| {
                    (!ownership.driver_owned)
                        .then(|| linked_terminal.as_ref().map(|recovered| recovered.status))
                        .flatten()
                });
                let child_pid = ownership.pid;
                let queued_for_replacement = job.status == JobStatus::Queued
                    && stored
                        .controller_job
                        .as_ref()
                        .is_some_and(|state| state.pending_admission_reason.is_some());
                let disposition = if queued_for_replacement {
                    DaemonActiveJobRecoveryDisposition::QueuedForReplacement
                } else if terminal_evidence.is_some() {
                    DaemonActiveJobRecoveryDisposition::TerminalEvidence
                } else if ownership.driver_supervises_live(stored) {
                    DaemonActiveJobRecoveryDisposition::DriverRecovery
                } else if let Some(liveness) = ownership.state {
                    match liveness {
                        LocalChildLiveness::Dead
                            if ownership.driver_owned && child_pid.is_none() =>
                        {
                            DaemonActiveJobRecoveryDisposition::DriverRecovery
                        }
                        LocalChildLiveness::Dead => DaemonActiveJobRecoveryDisposition::DeadChild,
                        LocalChildLiveness::IdentityMismatch => {
                            DaemonActiveJobRecoveryDisposition::ReusedChildPid
                        }
                        LocalChildLiveness::Live => {
                            DaemonActiveJobRecoveryDisposition::ProtectedLive
                        }
                        LocalChildLiveness::Unsupported(_) => {
                            DaemonActiveJobRecoveryDisposition::BlockingAmbiguous
                        }
                    }
                } else if resumable_in_daemon_controller_work(stored) {
                    // The daemon is this job's only executor and the driver
                    // published a checkpoint. If that daemon is gone, the
                    // replacement's controller recovery resumes the driver
                    // from it, so nothing needs attesting (#15420).
                    DaemonActiveJobRecoveryDisposition::DriverRecovery
                } else if current_lease_id
                    .is_some_and(|lease| job.daemon_lease_id.as_deref() == Some(lease))
                {
                    DaemonActiveJobRecoveryDisposition::MissingChildIdentityRecoverable
                } else if self.owns(stored)
                    && stored.remote_runner.is_none()
                    && (stored.controller_job.is_some() || job.status == JobStatus::Running)
                    // A linked attempt that is still active may be the live
                    // workload (a detached Cook outlives its daemon); it stays
                    // protected until that run terminalizes, then reconciles.
                    && linked_durable_run_state != Some(DaemonLinkedDurableRunState::Active)
                {
                    // A dead generation's in-scope job with no recorded child
                    // identity: startup recovery leaves it untouched, and only
                    // `daemon recover --attest-absent` terminalizes it.
                    DaemonActiveJobRecoveryDisposition::NeedsAttestation
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
                    controller_owned: stored.controller_job.is_some(),
                    linked_durable_run_id,
                    linked_durable_run_state,
                    linked_durable_run_terminal_status: linked_terminal
                        .as_ref()
                        .map(|recovered| recovered.status),
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

    /// Reconcile every active job of a dead daemon generation in this store's
    /// scope: each in-scope job whose lease is not `current_lease_id`.
    ///
    /// The caller holds the daemon owner lock. Only the lock holder writes the
    /// lease, so while it is held every other lease in this scope belongs to a
    /// generation that is gone, whether or not its `state.json` survived. Jobs
    /// of another scope in the shared ledger are never selected.
    ///
    /// A job whose recorded child still runs is protected; one whose recorded
    /// child is dead is terminalized with its retained evidence. Controller
    /// jobs are left to `recover_controller_jobs`, which owns their resume.
    /// A running job with no recorded child identity is left untouched and
    /// reported in `needs_attestation_job_ids`: nothing in process can prove
    /// its workload is gone.
    pub fn reconcile_dead_generation_jobs(
        &self,
        current_lease_id: &str,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_generation_jobs_with_local_child_liveness(
            current_lease_id,
            local_child_liveness,
            recovered_terminal_agent_task_result,
        )
    }

    /// Active in-scope jobs stamped with a lease other than `current_lease_id`:
    /// while the caller holds the owner lock, these are dead generations' work.
    pub(crate) fn active_dead_generation_job_ids(
        &self,
        current_lease_id: &str,
    ) -> std::collections::BTreeSet<Uuid> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .values()
            .filter(|stored| {
                matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                    && self.owns(stored)
                    && stored.job.daemon_lease_id.as_deref() != Some(current_lease_id)
            })
            .map(|stored| stored.job.id)
            .collect()
    }

    /// Terminalize exactly `job_ids` on an operator's attestation that their
    /// workload is gone. Each job must be active, in this store's scope, owned
    /// by a dead generation (any lease other than `live_lease_id`, which the
    /// caller proves by holding or probing the owner lock), and have no recorded
    /// child identity that could contradict the attestation. Any failed check
    /// refuses the whole set; the attestation is persisted on every job.
    pub fn attest_dead_generation_jobs_absent(
        &self,
        live_lease_id: Option<&str>,
        job_ids: &[Uuid],
    ) -> Result<Vec<Uuid>> {
        let attested = job_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if attested.is_empty() || attested.len() != job_ids.len() {
            return Err(Error::validation_invalid_argument(
                "attest_absent",
                "--attest-absent requires a non-empty set of unique job IDs",
                None,
                None,
            ));
        }
        self.durable_transaction(|inner| {
            for job_id in &attested {
                let refuse = |reason: String| {
                    Err(Error::validation_invalid_argument(
                        "attest_absent",
                        format!("refusing the attested job set: job `{job_id}` {reason}"),
                        Some(job_id.to_string()),
                        None,
                    ))
                };
                let stored = match self.read_stored_job(inner, *job_id, false) {
                    Ok(stored) => stored,
                    Err(_) => return refuse("does not exist".to_string()),
                };
                if !matches!(stored.job.status, JobStatus::Queued | JobStatus::Running) {
                    return refuse("is not active".to_string());
                }
                if !self.owns(&stored) {
                    return refuse("belongs to another daemon scope".to_string());
                }
                if live_lease_id.is_some() && stored.job.daemon_lease_id.as_deref() == live_lease_id
                {
                    return refuse("belongs to the live daemon generation".to_string());
                }
                if stored_job_execution_liveness(&stored, &local_child_liveness)
                    .state
                    .is_some()
                {
                    return refuse(
                        "has a recorded child identity; daemon startup recovery proves its state"
                            .to_string(),
                    );
                }
            }
            let now = timestamp_ms();
            for job_id in &attested {
                // The attestation also covers work a linked durable run may
                // still own elsewhere; record that link so the evidence shows
                // exactly what was vouched for (#15556).
                let linked_durable_run = inner
                    .jobs
                    .get(job_id)
                    .and_then(stored_job_durable_run_id)
                    .map(|run_id| {
                        let state =
                            super::super::agent_task_terminal_recovery::linked_durable_run_state(
                                &run_id,
                            );
                        serde_json::json!({ "run_id": run_id, "state": state })
                    });
                let stored = inner.jobs.get_mut(job_id).expect("attested job exists");
                stored.job.status = JobStatus::Failed;
                stored.job.updated_at_ms = now;
                stored.job.finished_at_ms = Some(now);
                stored.job.stale_reason = Some(
                    "daemon generation died; operator attested its workload absent".to_string(),
                );
                let data = serde_json::json!({
                    "status": JobStatus::Failed,
                    "reason": "operator_attested_workload_absent_after_daemon_death",
                    "retryable": true,
                    "daemon_lease_id": stored.job.daemon_lease_id,
                    "operator_confirmed_workload_processes_absent": true,
                    "attested_job_ids": attested,
                    "linked_durable_run": linked_durable_run,
                });
                for (kind, message) in [
                    (
                        JobEventKind::Error,
                        "daemon generation died before recording a child identity",
                    ),
                    (
                        JobEventKind::Status,
                        "job marked failed after operator attested its workload absent",
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
            Ok(attested.iter().copied().collect())
        })
    }

    /// Reconcile only active jobs whose durable terminal evidence proves no
    /// workload remains: their linked durable run already reached a terminal
    /// state, or their own event log recorded a terminal result. This
    /// deliberately does not inspect, stop, or alter live children, so it is
    /// safe when a daemon's aggregate count includes both stale handoffs and
    /// genuine work.
    /// Reconcile active jobs whose durable terminal evidence proves no workload
    /// remains. Driver-owned jobs are normally left to their driver, but a job
    /// that is still queued and never started has no execution at all: no
    /// child, no checkpoint, no start time. It cannot hold a workload, so its
    /// linked run's terminal evidence retires it too. Otherwise such a job,
    /// left on a draining daemon generation that no longer schedules queued
    /// work, blocks that generation's stop forever (and with it every daemon
    /// upgrade handoff).
    pub fn reconcile_terminal_linked_daemon_jobs(&self) -> Result<Vec<Uuid>> {
        self.reconcile_terminal_linked_daemon_jobs_with_resolver(
            recovered_terminal_agent_task_evidence,
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
                        && self.owns(stored)
                })
                .filter(|stored| {
                    never_started(stored)
                        || !stored_job_execution_liveness(stored, &local_child_liveness)
                            .driver_owned
                })
                .filter_map(|stored| resolve_terminal(stored).map(|result| (stored.job.id, result)))
                .collect::<Vec<_>>()
        };

        let mut reconciled = Vec::with_capacity(terminal.len());
        for (job_id, result) in terminal {
            let now = timestamp_ms();
            self.durable_transaction(|inner| {
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
                reconciled.push(job_id);
                Ok(())
            })?;
        }
        Ok(reconciled)
    }

    #[cfg(test)]
    pub(crate) fn reconcile_dead_generation_jobs_with_child_liveness(
        &self,
        current_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_generation_jobs_with_child_liveness_and_terminal_result(
            current_lease_id,
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
        self.reserve_local_child_at_with_runner_capacity(
            job_id,
            crate::api_jobs::timestamp_ms(),
            None,
        )
        .map(|_| ())?;
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
    pub(crate) fn reconcile_dead_generation_jobs_with_child_liveness_and_terminal_result(
        &self,
        current_lease_id: &str,
        pid_is_alive: impl Fn(u32) -> bool,
        terminal_child_result: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.reconcile_dead_generation_jobs_with_local_child_liveness(
            current_lease_id,
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

    fn reconcile_dead_generation_jobs_with_local_child_liveness(
        &self,
        current_lease_id: &str,
        inspect_local_child: impl Fn(&LocalChildExecution) -> LocalChildLiveness,
        terminal_child_result: impl Fn(&StoredJob) -> Option<RecoveredTerminalJob>,
    ) -> Result<DaemonLeaseJobDiagnostics> {
        self.durable_transaction(|inner| {
        let mut diagnostics = DaemonLeaseJobDiagnostics::default();
        let mut selected = inner
            .jobs
            .values()
            .filter(|stored| {
                matches!(stored.job.status, JobStatus::Queued | JobStatus::Running)
                    && self.owns(stored)
                    && stored.job.daemon_lease_id.as_deref() != Some(current_lease_id)
            })
            .map(|stored| stored.job.id)
            .collect::<Vec<_>>();
        selected.sort();
        let mut dispositions = Vec::with_capacity(selected.len());
        let reconciliation_now = timestamp_ms();
        for job_id in &selected {
            let stored = inner.jobs.get(job_id).expect("selected job exists");
            diagnostics.matching_job_ids.push(*job_id);
            // Driver-owned work resumes, or stays protected, through
            // `recover_controller_jobs`; generic reconciliation never fails it.
            if stored.controller_job.is_some() {
                diagnostics.preserved_controller_job_ids.push(*job_id);
                continue;
            }
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
                } else if let Some(liveness) = stored_job_execution_liveness(stored, &inspect_local_child).state {
                    match liveness {
                        LocalChildLiveness::Live => {
                            diagnostics.protected_job_ids.push(*job_id);
                            DeadLeaseJobDisposition::ProtectedLive
                        }
                        LocalChildLiveness::Dead | LocalChildLiveness::IdentityMismatch => {
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
                } else if stored.job.status == JobStatus::Queued {
                    DeadLeaseJobDisposition::TerminalizeDead
                } else {
                    // Running with no recorded child identity: nothing can
                    // prove the workload is gone (#15556 chaos case 2).
                    diagnostics.needs_attestation_job_ids.push(*job_id);
                    continue;
                };
            dispositions.push((*job_id, disposition));
        }

        let now = timestamp_ms();
        for (job_id, disposition) in dispositions {
            let stored = inner.jobs.get_mut(&job_id).expect("selected job exists");
            let dead_lease_id = stored.job.daemon_lease_id.clone();
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
                        "daemon_lease_id": dead_lease_id,
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
                        "daemon_lease_id": dead_lease_id,
                        "child_terminal_result": recovered.terminal_result,
                    })),
                });
                apply_event_retention(&mut stored.events, self.event_retention_limit());
                stored.job.event_count = stored.events.len();
                continue;
            }
            if let DeadLeaseJobDisposition::ProtectedUnsupported(evidence) = disposition {
                let reason = "local_child_identity_unsupported";
                let duplicate = stored.events.last().is_some_and(|event| {
                    event.data.as_ref().is_some_and(|data| {
                        data["reason"] == reason
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
                        message: Some("execution-owner recovery deferred".to_string()),
                        data: Some(serde_json::json!({
                            "reason": reason,
                            "evidence": evidence,
                            "recovery": "Execution-owner absence is not proven; work stays protected until authoritative process or driver evidence is available.",
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
                        "daemon_lease_id": dead_lease_id,
                    }),
                ),
                (
                    JobEventKind::Status,
                    "job marked failed after dead daemon lease".to_string(),
                    serde_json::json!({
                        "status": JobStatus::Failed,
                        "reason": "dead_daemon_lease",
                        "daemon_lease_id": dead_lease_id,
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
        diagnostics.preserved_controller_job_ids.sort();
        diagnostics.needs_attestation_job_ids.sort();
        Ok(diagnostics)
        })
    }

    /// Whether this store's daemon executes a job stamped with `job_lease_id`.
    ///
    /// Every daemon running as one Unix user opens the same HOME-scoped
    /// ledger, whatever its `HOMEBOY_DAEMON_STATE_DIR` (#16089). The executing
    /// daemon stamps its lease on each job it creates or claims, so a job
    /// stamped with another lease is another daemon's work: it cannot occupy
    /// this daemon's capacity, and replacing this daemon cannot interrupt it.
    /// An unstamped record has no proven owner and stays counted. A store
    /// without a lease (CLI inspection) projects the whole ledger.
    pub(crate) fn executes_lease(&self, job_lease_id: Option<&str>) -> bool {
        match (self.daemon_lease_id.as_deref(), job_lease_id) {
            (Some(own), Some(job)) => own == job,
            _ => true,
        }
    }

    /// This daemon's typed live-job ownership snapshot; see
    /// [`Self::executes_lease`]. The full ledger stays available through
    /// [`Self::list`] for operator inventory.
    pub(crate) fn active_runner_jobs(&self) -> Vec<super::super::types::ActiveRunnerJobSummary> {
        let now = timestamp_ms();
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let mut jobs: Vec<super::super::types::ActiveRunnerJobSummary> = inner
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .filter(|stored| self.executes_lease(stored.job.daemon_lease_id.as_deref()))
            .map(|stored| {
                let re_adoptable = stored.re_adoptable_across_daemon_restart();
                stored
                    .remote_runner
                    .as_ref()
                    .map(|remote| {
                        super::super::summary::active_runner_job_summary(
                            &stored.job,
                            &remote.envelope,
                            now,
                            re_adoptable,
                        )
                    })
                    .or_else(|| {
                        stored.local_runner.as_ref().map(|local| {
                            super::super::summary::active_local_runner_job_summary(
                                &stored.job,
                                local,
                                now,
                                re_adoptable,
                            )
                        })
                    })
                    .unwrap_or_else(|| {
                        super::super::summary::active_daemon_job_summary(
                            &stored.job,
                            now,
                            re_adoptable,
                        )
                    })
            })
            .collect();
        jobs.sort_by_key(|job| (job.started_at_ms, job.job_id.clone()));
        jobs
    }

    /// The active execution admitted under a caller-owned transport submission
    /// key. Capacity reservations use the same key but must be consumed rather
    /// than returned as the accepted execution.
    pub(crate) fn active_execution_job_for_admission_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Option<Job> {
        if idempotency_key.trim().is_empty() {
            return None;
        }
        let inner = self.inner.lock().expect("job store mutex poisoned");
        inner
            .jobs
            .values()
            .filter(|stored| matches!(stored.job.status, JobStatus::Queued | JobStatus::Running))
            .filter(|stored| stored.job.operation != "runner.admission")
            .filter(|stored| stored.admission_idempotency_key.as_deref() == Some(idempotency_key))
            .min_by_key(|stored| (stored.job.created_at_ms, stored.job.id))
            .map(|stored| stored.job.clone())
    }

    pub(crate) fn stale_runner_jobs(&self) -> Vec<super::super::types::ActiveRunnerJobSummary> {
        let now = timestamp_ms();
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let entries = self
            .jobs_with_history(&inner, super::sql_store::TerminalJobs::Stale)
            .unwrap_or_else(|_| inner.jobs.clone());
        let mut jobs: Vec<super::super::types::ActiveRunnerJobSummary> = entries
            .values()
            .filter(|stored| {
                stored.job.status == JobStatus::Failed && stored.job.stale_reason.is_some()
            })
            .filter(|stored| self.executes_lease(stored.job.daemon_lease_id.as_deref()))
            .filter_map(|stored| {
                let envelope = &stored.remote_runner.as_ref()?.envelope;
                Some(super::super::summary::active_runner_job_summary(
                    &stored.job,
                    envelope,
                    now,
                    false,
                ))
            })
            .collect();
        jobs.sort_by_key(|job| (job.updated_at_ms, job.job_id.clone()));
        jobs
    }
}

/// A queued job that was never started has no execution to protect.
fn never_started(stored: &StoredJob) -> bool {
    stored.job.status == JobStatus::Queued
        && stored.job.started_at_ms.is_none()
        && stored.local_child.is_none()
        && stored
            .controller_job
            .as_ref()
            .is_none_or(|controller| controller.checkpoint.is_none())
}
