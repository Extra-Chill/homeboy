//! Owner liveness for queued runner executions (#18601).
//!
//! A queued job has no process yet, so nothing on the daemon proves that
//! anyone still waits for it. The client that waits on a queued job opts in to
//! owner liveness, either at submission (envelope metadata
//! [`QUEUED_OWNER_HEARTBEAT_METADATA`]) or with its first
//! `POST /jobs/:id/observe`, and then heartbeats that route while the job is
//! queued. When the heartbeats stop for [`QUEUED_OWNER_DEADLINE_MS`], the
//! client was cancelled, killed, or gave up. Its entry would otherwise hold a
//! queue position and a gate-reserve slot ahead of live work indefinitely, so
//! the daemon cancels it with a named reason.
//!
//! Jobs that never opted in, such as detached handoffs and submissions from
//! older clients, keep the existing no-deadline queue policy. Admitted jobs
//! leave this ledger: once a job holds a child reservation, losing its observer
//! is not evidence against it. The ledger lives in process memory, so after a
//! daemon restart a job becomes reapable again only after its client
//! heartbeats the new generation.

use super::*;

/// Envelope metadata key with which a submitting client declares that it
/// heartbeats the job while it is queued.
pub const QUEUED_OWNER_HEARTBEAT_METADATA: &str = "queued_owner_heartbeat";

/// Owner silence after which an opted-in queued job is abandoned. Waiting
/// clients heartbeat about once a minute and ride out daemon transport outages
/// of up to 15 minutes, so this outlasts a tolerated outage and many missed
/// beats, but not an owner that has gone away.
pub const QUEUED_OWNER_DEADLINE_MS: u64 = 30 * 60 * 1000;

/// Prefix of the reason recorded on a job reaped for owner silence.
pub(crate) const QUEUED_OWNER_ABANDONED_REASON: &str = "queued_owner_abandoned";

impl JobStore {
    /// Record an owner heartbeat for a queued job, opting it in to owner
    /// liveness. A job that is no longer queued leaves the ledger. Returns the
    /// current job so the client can stop heartbeating once it is admitted.
    pub(crate) fn observe_queued_job(&self, job_id: Uuid) -> Result<Job> {
        self.observe_queued_job_at(job_id, timestamp_ms())
    }

    pub(crate) fn observe_queued_job_at(&self, job_id: Uuid, now: u64) -> Result<Job> {
        let job = self.get(job_id)?;
        let mut observers = self
            .queued_observers
            .lock()
            .expect("queued observers mutex poisoned");
        if job.status == JobStatus::Queued {
            observers.insert(job_id, now);
        } else {
            observers.remove(&job_id);
        }
        Ok(job)
    }

    /// Milliseconds since the last owner heartbeat of each opted-in queued job.
    pub(crate) fn queued_owner_heartbeat_ages(&self) -> HashMap<Uuid, u64> {
        let now = timestamp_ms();
        self.queued_observers
            .lock()
            .expect("queued observers mutex poisoned")
            .iter()
            .map(|(job_id, last)| (*job_id, now.saturating_sub(*last)))
            .collect()
    }

    pub(crate) fn reconcile_abandoned_queued_jobs(&self) -> Result<Vec<Uuid>> {
        self.reconcile_abandoned_queued_jobs_at(timestamp_ms(), QUEUED_OWNER_DEADLINE_MS)
    }

    /// Cancel opted-in queued jobs whose owner has been silent for
    /// `deadline_ms`, freeing their queue position and gate-reserve slot.
    pub(crate) fn reconcile_abandoned_queued_jobs_at(
        &self,
        now: u64,
        deadline_ms: u64,
    ) -> Result<Vec<Uuid>> {
        let observed = self
            .queued_observers
            .lock()
            .expect("queued observers mutex poisoned")
            .clone();
        if observed.is_empty() {
            return Ok(Vec::new());
        }
        let (abandoned, settled) = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            let mut abandoned = Vec::new();
            let mut settled = Vec::new();
            for (job_id, last) in observed {
                match inner.jobs.get(&job_id) {
                    Some(stored) if Self::awaits_admission(stored) => {
                        let silence = now.saturating_sub(last);
                        if silence >= deadline_ms && self.owns(stored) {
                            abandoned.push((job_id, silence));
                        }
                    }
                    _ => settled.push(job_id),
                }
            }
            (abandoned, settled)
        };
        {
            let mut observers = self
                .queued_observers
                .lock()
                .expect("queued observers mutex poisoned");
            for job_id in &settled {
                observers.remove(job_id);
            }
        }
        let mut reaped = Vec::with_capacity(abandoned.len());
        for (job_id, silence) in abandoned {
            // Re-check under the store lock: admission may have reserved a
            // child since the scan, and an admitted job is not reaped here.
            let still_waiting = {
                let inner = self.inner.lock().expect("job store mutex poisoned");
                inner.jobs.get(&job_id).is_some_and(Self::awaits_admission)
            };
            if !still_waiting {
                continue;
            }
            let reason = format!(
                "{QUEUED_OWNER_ABANDONED_REASON}: no owner heartbeat for {}s while queued; the waiting client is gone, so the job left the queue",
                silence / 1000
            );
            if self.cancel(job_id, reason).is_ok() {
                reaped.push(job_id);
            }
            self.queued_observers
                .lock()
                .expect("queued observers mutex poisoned")
                .remove(&job_id);
        }
        Ok(reaped)
    }

    /// Queued, never admitted to a child, and cancellable through
    /// [`JobStore::cancel`].
    fn awaits_admission(stored: &StoredJob) -> bool {
        stored.job.status == JobStatus::Queued
            && stored.job.started_at_ms.is_none()
            && stored.local_child.is_none()
            && stored.controller_job.is_none()
            && stored.remote_runner.is_none()
            && stored.admission_lease.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_gate(store: &JobStore) -> Uuid {
        let job = store.create("runner.exec");
        store
            .durable_transaction(|inner| {
                inner
                    .jobs
                    .get_mut(&job.id)
                    .unwrap()
                    .job
                    .runner_job_projection = Some(crate::api_jobs::RunnerJobProjection {
                    runner_id: "lab".into(),
                    command: "homeboy review test".into(),
                    cwd: None,
                    source: "runner-daemon".into(),
                    kind: "runner.exec".into(),
                    lifecycle: None,
                });
                Ok(())
            })
            .unwrap();
        job.id
    }

    #[test]
    fn silent_queued_owner_is_reaped_and_frees_its_slot_while_a_heartbeating_one_stays() {
        crate::test_support::with_isolated_home(|home| {
            let store = JobStore::open_without_reconciliation(
                &home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
            )
            .unwrap();
            let deadline = QUEUED_OWNER_DEADLINE_MS;
            let abandoned = queued_gate(&store);
            let waiting = queued_gate(&store);
            let detached = queued_gate(&store);
            assert_eq!(store.gate_capacity()["queued_slots"], 3);

            // Both owners opt in at enqueue; only one keeps heartbeating.
            store.observe_queued_job_at(abandoned, 1_000).unwrap();
            store.observe_queued_job_at(waiting, 1_000).unwrap();
            store
                .observe_queued_job_at(waiting, 1_000 + deadline / 2)
                .unwrap();

            assert!(store
                .reconcile_abandoned_queued_jobs_at(1_000 + deadline - 1, deadline)
                .unwrap()
                .is_empty());
            let reaped = store
                .reconcile_abandoned_queued_jobs_at(1_000 + deadline, deadline)
                .unwrap();
            assert_eq!(reaped, vec![abandoned]);

            let job = store.get(abandoned).unwrap();
            assert_eq!(job.status, JobStatus::Cancelled);
            assert!(job
                .stale_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with(QUEUED_OWNER_ABANDONED_REASON)));
            assert_eq!(store.get(waiting).unwrap().status, JobStatus::Queued);
            // A job whose owner never opted in keeps the no-deadline policy.
            assert_eq!(store.get(detached).unwrap().status, JobStatus::Queued);
            assert_eq!(store.gate_capacity()["queued_slots"], 2);
            assert!(!store
                .active_runner_jobs()
                .iter()
                .any(|job| job.job_id == abandoned.to_string()));
        });
    }
}
