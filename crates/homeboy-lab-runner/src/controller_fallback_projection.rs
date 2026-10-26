//! Durable controller fallback receipts for sealed runner staging. The
//! ledger is a pending-receipt queue only: the agent-task lifecycle owns
//! terminal projection.

use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use homeboy_control_plane_contract::{resolve, IdentityKind, MissionId, ResolveError};
use homeboy_core::observation::coordination::{
    CoordinationRecord, ExpectedRevision, LeaseAcquire, LeaseAdmission, LeaseOwner, LeaseRequest,
    RecordOutcome, RecordWrite,
};
use homeboy_core::observation::ObservationStore;
use homeboy_core::{Error, Result};

use crate::runner_staging_operation::{
    submit_remote_runner_staging, RemoteRunnerStagingEnvelope, RemoteRunnerStagingReceipt,
    RemoteRunnerStagingTransport,
};

/// Coordination namespace owned by the controller fallback projection
/// (#15947). Each pending receipt is one record keyed by its exact handoff run
/// id; the reconciliation cursor is the record under [`CURSOR_KEY`].
pub(crate) const NAMESPACE: &str = "controller-fallback-projection";
const CURSOR_KEY: &str = "cursor";
const STATE_PENDING: &str = "pending";
const STATE_CURSOR: &str = "cursor";

/// Receipt schema. Receipts are keyed by the exact handoff run id, so each
/// attempt owns its own admission idempotence and finalization while the
/// resolved mission stays grouping data on the record. Only pending receipts
/// exist: a finalized receipt is deleted, because the agent-task lifecycle is
/// the authority for terminal state.
const RECEIPT_SCHEMA: &str = "homeboy/controller-fallback-projection/v3";
const STARTUP_RECONCILIATION_BATCH_SIZE: usize = 8;
const REMOTE_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for one finalization claim. A crashed claimant's lease is
/// reaped once its process is proven dead, or after this TTL otherwise.
const FINALIZE_LEASE_TTL: Duration = Duration::from_secs(300);
/// Cursor compare-and-set retries before a reservation gives up. Each conflict
/// means another reconciler advanced the cursor, so this only bounds a storm.
const CURSOR_CAS_ATTEMPTS: usize = 32;

/// Controller-visible durable receipt for a runner-owned admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeferredControllerReceipt {
    pub schema: String,
    pub mission_id: MissionId,
    pub runner_receipt: RemoteRunnerStagingReceipt,
    pub controller_projection: String,
}

impl DeferredControllerReceipt {
    fn new(mission_id: MissionId, runner_receipt: RemoteRunnerStagingReceipt) -> Self {
        Self {
            schema: RECEIPT_SCHEMA.to_string(),
            mission_id,
            runner_receipt,
            controller_projection: "deferred".to_string(),
        }
    }

    fn validate_for(&self, envelope: &RemoteRunnerStagingEnvelope) -> Result<()> {
        if self.schema != RECEIPT_SCHEMA
            || self.mission_id.as_str().trim().is_empty()
            || self.controller_projection != "deferred"
        {
            return Err(Error::validation_invalid_argument(
                "controller_fallback_receipt",
                "deferred controller receipt is malformed",
                Some(envelope.handoff.run_id.clone()),
                None,
            ));
        }
        self.runner_receipt.validate_for(envelope)
    }
}

fn mission_from_handoff_run_id(run_id: &str) -> Result<MissionId> {
    match resolve(IdentityKind::RunId, run_id) {
        Ok(resolved) => resolved.mission.ok_or_else(|| {
            resolve_to_error(
                ResolveError::MalformedRun {
                    value: run_id.to_string(),
                },
                run_id,
            )
        }),
        Err(ResolveError::MalformedRun { .. }) => mission_id_from_grouping(run_id),
        Err(error) => Err(resolve_to_error(error, run_id)),
    }
}

fn mission_id_from_grouping(value: &str) -> Result<MissionId> {
    match resolve(IdentityKind::CookId, value) {
        Ok(resolved) => resolved.mission.ok_or_else(|| {
            resolve_to_error(
                ResolveError::Empty {
                    kind: IdentityKind::CookId,
                },
                value,
            )
        }),
        Err(error) => Err(resolve_to_error(error, value)),
    }
}

fn resolve_to_error(error: ResolveError, id: &str) -> Error {
    Error::validation_invalid_argument("mission_id", error.to_string(), Some(id.to_string()), None)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    /// Exact handoff run id of the last receipt reserved by a bounded pass.
    run_id: String,
}

/// Controller receipt queue kept as coordination records. Runner admission is
/// atomic in its own store; this queue only records accepted receipts, keyed
/// by each receipt's exact handoff run id, until the agent-task lifecycle
/// finalizes them. The resolved mission remains grouping data on the record
/// and never gates a later attempt.
pub struct ControllerFallbackProjectionStore {
    database: PathBuf,
}

impl ControllerFallbackProjectionStore {
    /// Shared controller queue survives daemon restarts independently of the
    /// runner-owned staging store.
    pub fn open_default() -> Result<Self> {
        let roots = homeboy_core::paths::PathRoots::from_environment()?;
        let data_root = roots.data();
        Ok(Self::open_at(homeboy_core::paths::observation_db_in_root(
            data_root,
        )))
    }

    /// Queue in the observation database at an explicit path.
    pub fn open_at(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
        }
    }

    fn store(&self) -> Result<ObservationStore> {
        ObservationStore::open_initialized_for_lifecycle_at(&self.database)
    }

    /// Preflight happens inside `submit_remote_runner_staging` before the
    /// transport mutation boundary, so refusals spend no provider budget.
    ///
    /// Idempotence is owned by the exact handoff run id: replaying the same
    /// run returns its admitted receipt, a different receipt for the same run
    /// is an evidence conflict that fails closed, and a distinct attempt under
    /// the same mission is admitted on its own.
    pub fn submit_detached<T: RemoteRunnerStagingTransport>(
        &self,
        transport: &mut T,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<DeferredControllerReceipt> {
        let mission_id = mission_from_handoff_run_id(&envelope.handoff.run_id)?;
        let runner_receipt = submit_remote_runner_staging(transport, envelope)?;
        let receipt = DeferredControllerReceipt::new(mission_id, runner_receipt);
        receipt.validate_for(envelope)?;
        let run_key = receipt.runner_receipt.handoff.run_id.clone();
        if run_key == CURSOR_KEY {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "controller fallback run id collides with the reconciliation cursor key",
                Some(run_key),
                None,
            ));
        }
        let payload = to_payload(&receipt)?;
        let outcome = self.store()?.put_coordination_record(
            NAMESPACE,
            &run_key,
            ExpectedRevision::Absent,
            RecordWrite {
                state: STATE_PENDING,
                payload: &payload,
                expires_at_ms: None,
            },
        )?;
        match outcome {
            RecordOutcome::Applied(_) => Ok(receipt),
            RecordOutcome::Conflict {
                current: Some(record),
            } => {
                let existing = parse_receipt(&record)?;
                if existing != receipt {
                    return Err(Error::validation_invalid_argument(
                        "idempotency_key",
                        "controller fallback run is already bound to a different runner receipt",
                        Some(run_key),
                        None,
                    ));
                }
                Ok(existing)
            }
            RecordOutcome::Conflict { current: None } => Err(Error::internal_unexpected(format!(
                "controller fallback receipt for `{run_key}` changed while admitting it"
            ))),
        }
    }

    /// Pending receipts in run-id order.
    fn pending(
        &self,
        store: &ObservationStore,
    ) -> Result<Vec<(String, DeferredControllerReceipt)>> {
        store
            .coordination_records(NAMESPACE)?
            .into_iter()
            .filter(|record| record.key != CURSOR_KEY)
            .map(|record| Ok((record.key.clone(), parse_receipt(&record)?)))
            .collect()
    }

    /// Reconcile a bounded receipt batch against authoritative runner jobs. A
    /// terminal snapshot reaches the idempotent agent-task lifecycle
    /// finalizer, and on success its receipt is removed from this queue; a
    /// finalize error, a nonterminal job, or a snapshot error keeps the
    /// receipt for the next pass. Each receipt reconciles under its exact
    /// handoff run id, so one attempt's snapshot can never finalize another
    /// attempt or the parent mission.
    ///
    /// Returns the exact handoff run ids finalized in this pass.
    pub fn reconcile_after_controller_restart_with<Snapshot, Finalize>(
        &self,
        limit: usize,
        snapshot: Snapshot,
        finalize: Finalize,
    ) -> Result<Vec<String>>
    where
        Snapshot: Fn(&str, &str) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot>
            + Send
            + Sync
            + 'static,
        Finalize: Fn(&str, &homeboy_core::api_jobs::RunnerJobLogSnapshot) -> Result<bool>,
    {
        self.reconcile_after_controller_restart_with_timeout(
            limit,
            REMOTE_STATUS_TIMEOUT,
            snapshot,
            finalize,
        )
    }

    fn reconcile_after_controller_restart_with_timeout<Snapshot, Finalize>(
        &self,
        limit: usize,
        timeout: Duration,
        snapshot: Snapshot,
        finalize: Finalize,
    ) -> Result<Vec<String>>
    where
        Snapshot: Fn(&str, &str) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot>
            + Send
            + Sync
            + 'static,
        Finalize: Fn(&str, &homeboy_core::api_jobs::RunnerJobLogSnapshot) -> Result<bool>,
    {
        // No observation database means no deferred receipts. Return before
        // opening it, so the detached startup pass leaves an untouched data
        // root untouched.
        if !self.database.exists() {
            return Ok(Vec::new());
        }
        let store = self.store()?;
        let receipts = self.reserve_reconciliation_batch_in(&store, limit)?;
        if receipts.is_empty() {
            return Ok(Vec::new());
        }
        // A claimant whose process died mid-finalization must not pin its
        // receipt until the lease TTL runs out.
        store.reap_coordination_leases(NAMESPACE)?;
        let snapshot = Arc::new(snapshot);
        let mut finalized = Vec::new();

        for (run_id, receipt) in receipts {
            let result = remote_snapshot_with_timeout(
                Arc::clone(&snapshot),
                receipt.runner_receipt.handoff.runner_id.clone(),
                receipt.runner_receipt.handoff.runner_job_id.clone(),
                timeout,
            );
            // A nonterminal job or an unreachable runner stays pending for
            // the next bounded pass; the lifecycle owns terminal state, so
            // nothing is persisted for either.
            let snapshot = match result {
                Ok(snapshot) if snapshot.job.status.is_terminal() => snapshot,
                Ok(_) | Err(_) => continue,
            };
            if finalize_claimed(&store, &run_id, &snapshot, &finalize)? {
                finalized.push(run_id);
            }
        }
        Ok(finalized)
    }

    /// Reserve a fair bounded pass by advancing the cursor record through a
    /// revision compare-and-set. Advance before probing, so a timed-out
    /// process or overlapping startup cannot repeatedly pin reconciliation to
    /// the first unreachable receipts; a concurrent reservation that loses
    /// the CAS recomputes its batch after the winner's cursor.
    #[cfg(test)]
    fn reserve_reconciliation_batch(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, DeferredControllerReceipt)>> {
        self.reserve_reconciliation_batch_in(&self.store()?, limit)
    }

    fn reserve_reconciliation_batch_in(
        &self,
        store: &ObservationStore,
        limit: usize,
    ) -> Result<Vec<(String, DeferredControllerReceipt)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        for _ in 0..CURSOR_CAS_ATTEMPTS {
            let pending = self.pending(store)?;
            if pending.is_empty() {
                return Ok(Vec::new());
            }
            let cursor_record = store.coordination_record(NAMESPACE, CURSOR_KEY)?;
            let cursor = cursor_record
                .as_ref()
                .map(|record| {
                    serde_json::from_value::<CursorPayload>(record.payload.clone())
                        .map(|cursor| cursor.run_id)
                        .map_err(|error| {
                            Error::internal_json(
                                error.to_string(),
                                Some("parse controller fallback cursor".to_string()),
                            )
                        })
                })
                .transpose()?;
            let start = cursor
                .as_ref()
                .and_then(|cursor| pending.iter().position(|(run_id, _)| run_id > cursor))
                .unwrap_or(0);
            let batch = pending
                .iter()
                .cycle()
                .skip(start)
                .take(limit.min(pending.len()))
                .cloned()
                .collect::<Vec<_>>();
            let payload = to_payload(&CursorPayload {
                run_id: batch.last().expect("nonempty batch").0.clone(),
            })?;
            let expected = cursor_record.map_or(ExpectedRevision::Absent, |record| {
                ExpectedRevision::Revision(record.revision)
            });
            match store.put_coordination_record(
                NAMESPACE,
                CURSOR_KEY,
                expected,
                RecordWrite {
                    state: STATE_CURSOR,
                    payload: &payload,
                    expires_at_ms: None,
                },
            )? {
                RecordOutcome::Applied(_) => return Ok(batch),
                RecordOutcome::Conflict { .. } => continue,
            }
        }
        Err(Error::internal_unexpected(format!(
            "controller fallback cursor kept changing across {CURSOR_CAS_ATTEMPTS} reservation attempts"
        )))
    }
}

/// Finalize one receipt under an exclusive lease on its run id, so concurrent
/// reconcilers enter lifecycle finalization at most once. The receipt is read
/// after the lease is won and deleted at that exact revision; a receipt a
/// previous holder already finalized reads as absent and is skipped.
fn finalize_claimed<Finalize>(
    store: &ObservationStore,
    run_id: &str,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    finalize: &Finalize,
) -> Result<bool>
where
    Finalize: Fn(&str, &homeboy_core::api_jobs::RunnerJobLogSnapshot) -> Result<bool>,
{
    let empty = serde_json::json!({});
    let holder = match store.acquire_coordination_lease(LeaseRequest {
        namespace: NAMESPACE,
        key: run_id,
        ttl: FINALIZE_LEASE_TTL,
        owner: LeaseOwner::current_process(),
        admission: LeaseAdmission::Exclusive,
        weight: &empty,
        payload: &empty,
    })? {
        LeaseAcquire::Acquired(holder) => holder,
        // Another reconciler is finalizing this run right now.
        LeaseAcquire::Contended(_) => return Ok(false),
    };
    let result = (|| {
        let Some(record) = store.coordination_record(NAMESPACE, run_id)? else {
            return Ok(false);
        };
        if finalize(run_id, snapshot).is_err() {
            // A failed finalization keeps its receipt so the next pass
            // retries it; the lifecycle store records the failure itself.
            return Ok(false);
        }
        Ok(matches!(
            store.delete_coordination_record(NAMESPACE, run_id, record.revision)?,
            RecordOutcome::Applied(_)
        ))
    })();
    store.release_coordination_lease(NAMESPACE, run_id, &holder.holder_token)?;
    result
}

fn parse_receipt(record: &CoordinationRecord) -> Result<DeferredControllerReceipt> {
    serde_json::from_value(record.payload.clone()).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!(
                "parse controller fallback receipt `{}`",
                record.key
            )),
        )
    })
}

fn to_payload<T: Serialize>(value: &T) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize controller fallback projection".to_string()),
        )
    })
}

/// Production startup reconciliation for deferred runner staging. It reads at
/// most eight runner jobs per pass, rotating durably through pending receipts so
/// unavailable historical jobs cannot monopolize the bounded recovery budget.
/// Returns the number of receipts finalized by the lifecycle in this pass.
///
/// Deliberately has no injected sibling. Both reconciliation callbacks below
/// are bare ambient function references — `crate::runner_job_log_snapshot`
/// resolves runner session state and `project_terminal_runner_result` resolves
/// the durable lifecycle root — so an injected data root here would project a
/// ledger from one home against lifecycle state from another (#7505).
pub fn reconcile_on_controller_startup() -> Result<usize> {
    Ok(ControllerFallbackProjectionStore::open_default()?
        .reconcile_after_controller_restart_with(
            STARTUP_RECONCILIATION_BATCH_SIZE,
            crate::runner_job_log_snapshot,
            homeboy_agents::agent_task_lifecycle::project_terminal_runner_result,
        )?
        .len())
}

fn remote_snapshot_with_timeout<Snapshot>(
    snapshot: Arc<Snapshot>,
    runner_id: String,
    job_id: String,
    timeout: Duration,
) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot>
where
    Snapshot: Fn(&str, &str) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot>
        + Send
        + Sync
        + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(snapshot(&runner_id, &job_id));
    });
    receiver.recv_timeout(timeout).map_err(|error| {
        Error::internal_unexpected(format!(
            "runner status query timed out after {}ms: {error}",
            timeout.as_millis()
        ))
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_staging_operation::tests_support::{envelope, envelope_for_run, Transport};
    use homeboy_core::api_jobs::{Job, JobStatus, RunnerJobLogSnapshot};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, Mutex};
    use tempfile::tempdir;
    use uuid::Uuid;

    /// The blocked DLA Cook mission from the field report: its first attempt
    /// held the v1 mission-keyed receipt, and later attempts were refused by
    /// `submit_detached` after the runner had already admitted them.
    const COOK_MISSION: &str = "agent-task-20bf814f-8536-44da-acb2-951d55ee50ca";
    const FIRST_ATTEMPT_RUN: &str =
        "agent-task-20bf814f-8536-44da-acb2-951d55ee50ca-attempt-1-5d0aad97";
    const RETRY_ATTEMPT_RUN: &str =
        "agent-task-20bf814f-8536-44da-acb2-951d55ee50ca-attempt-3-7ec741b8";

    fn store() -> ControllerFallbackProjectionStore {
        ControllerFallbackProjectionStore::open_at(
            tempdir().expect("temp").keep().join("observations.sqlite"),
        )
    }

    fn pending(
        store: &ControllerFallbackProjectionStore,
    ) -> std::collections::BTreeMap<String, DeferredControllerReceipt> {
        store
            .pending(&store.store().expect("observation store"))
            .expect("pending receipts")
            .into_iter()
            .collect()
    }

    fn snapshot(status: JobStatus) -> RunnerJobLogSnapshot {
        RunnerJobLogSnapshot {
            job: Job {
                id: Uuid::new_v4(),
                operation: "staged-agent-task".to_string(),
                status,
                created_at_ms: 0,
                updated_at_ms: 0,
                started_at_ms: None,
                finished_at_ms: None,
                event_count: 0,
                source_snapshot: None,
                path_materialization_plan: None,
                stale_reason: None,
                daemon_lease_id: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
                runner_job_projection: None,
            },
            events: Vec::new(),
        }
    }

    #[test]
    fn failed_controller_daemon_uses_compatible_runner_once_and_returns_deferred_receipt() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let first = store
            .submit_detached(&mut runner, &envelope)
            .expect("fallback admission");
        let repeated = store
            .submit_detached(&mut runner, &envelope)
            .expect("replay");
        assert_eq!(first, repeated);
        assert_eq!(first.controller_projection, "deferred");
        assert_eq!(runner.provider_budget(), 0);
    }

    #[test]
    fn disconnected_or_incompatible_runner_refuses_before_provider_budget() {
        let envelope = envelope();
        for mut runner in [Transport::incompatible(), Transport::disconnected()] {
            assert!(store().submit_detached(&mut runner, &envelope).is_err());
            assert_eq!(runner.calls(), 0);
            assert_eq!(runner.provider_budget(), 0);
        }
    }

    #[test]
    fn terminal_snapshot_finalizes_once_and_removes_the_receipt() {
        for status in [JobStatus::Succeeded, JobStatus::Failed] {
            let store = store();
            let envelope = envelope();
            let mut runner = Transport::compatible();
            let receipt = store
                .submit_detached(&mut runner, &envelope)
                .expect("admit deferred receipt");
            let run_id = receipt.runner_receipt.handoff.run_id.clone();

            let finalized = store
                .reconcile_after_controller_restart_with(
                    8,
                    move |_, _| Ok(snapshot(status)),
                    |_, _| Ok(true),
                )
                .expect("terminal reconciliation");

            assert_eq!(finalized, vec![run_id.clone()]);
            assert!(
                pending(&store).is_empty(),
                "a finalized receipt leaves the pending queue"
            );

            // A restart replays nothing for the finalized run: it is no
            // longer in the queue, so neither the runner query nor the
            // lifecycle CAS is re-entered.
            let replay = store
                .reconcile_after_controller_restart_with(
                    8,
                    |_, _| panic!("finalized receipts must not be re-queried after restart"),
                    |_, _| panic!("terminal lifecycle CAS must not be re-entered after restart"),
                )
                .expect("restart replay");
            assert!(replay.is_empty());
        }
    }

    #[test]
    fn finalize_error_keeps_the_receipt_for_the_next_pass() {
        let store = store();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope())
            .expect("admit deferred receipt");
        let run_id = receipt.runner_receipt.handoff.run_id.clone();

        let finalized = store
            .reconcile_after_controller_restart_with(
                8,
                |_, _| Ok(snapshot(JobStatus::Succeeded)),
                |_, _| Err(Error::internal_unexpected("lifecycle CAS lost the race")),
            )
            .expect("bounded reconciliation with a failed finalization");

        assert!(finalized.is_empty());
        let remaining = pending(&store);
        assert_eq!(remaining.len(), 1);
        assert!(
            remaining.contains_key(run_id.as_str()),
            "a failed finalization keeps its receipt pending"
        );

        let finalized = store
            .reconcile_after_controller_restart_with(
                8,
                |_, _| Ok(snapshot(JobStatus::Succeeded)),
                |_, _| Ok(true),
            )
            .expect("retry pass");
        assert_eq!(finalized, vec![run_id]);
        assert!(pending(&store).is_empty());
    }

    #[test]
    fn startup_reconciliation_without_a_ledger_writes_nothing() {
        // Every mutating command schedules this pass; with no deferred
        // receipts it must not create the observation database or cursor.
        crate::test_support::with_isolated_home(|_| {
            let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
            let root = roots.data();
            std::fs::create_dir_all(root).expect("data root");
            let store =
                ControllerFallbackProjectionStore::open_default().expect("open projection store");

            let finalized = store
                .reconcile_after_controller_restart_with(
                    8,
                    |_, _| panic!("no receipts means no runner query"),
                    |_, _| panic!("no receipts means no finalization"),
                )
                .expect("reconcile empty ledger");

            assert!(finalized.is_empty());
            assert_eq!(
                std::fs::read_dir(root).expect("read data root").count(),
                0,
                "an empty data root stays empty"
            );

            // An existing database with no receipts gains no cursor record.
            let observations = ObservationStore::open_initialized_for_lifecycle_at(
                homeboy_core::paths::observation_db_in_root(root),
            )
            .expect("create observation store");
            assert!(store
                .reconcile_after_controller_restart_with(
                    8,
                    |_, _| panic!("no receipts means no runner query"),
                    |_, _| panic!("no receipts means no finalization"),
                )
                .expect("reconcile empty queue")
                .is_empty());
            assert!(observations
                .coordination_records(NAMESPACE)
                .expect("namespace records")
                .is_empty());
        });
    }

    #[test]
    fn startup_reconciliation_returns_before_a_blocked_remote_query() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope)
            .expect("admit deferred receipt");
        let run_id = receipt.runner_receipt.handoff.run_id.clone();
        let (finished_query, query_completion) = mpsc::channel();

        let finalized = store
            .reconcile_after_controller_restart_with_timeout(
                8,
                Duration::from_millis(20),
                move |_, _| {
                    thread::sleep(Duration::from_secs(1));
                    let _ = finished_query.send(());
                    Ok(snapshot(JobStatus::Succeeded))
                },
                |_, _| Ok(true),
            )
            .expect("bounded reconciliation");

        assert!(
            matches!(query_completion.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "startup reconciliation waited for the blocked runner query"
        );
        assert!(finalized.is_empty());
        let remaining = pending(&store);
        assert_eq!(remaining.len(), 1);
        assert!(
            remaining.contains_key(run_id.as_str()),
            "a timed-out runner query keeps its receipt pending"
        );
    }

    #[test]
    fn nonterminal_runner_status_stays_pending_for_the_next_bounded_pass() {
        let store = store();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope())
            .expect("admit deferred receipt");
        let run_id = receipt.runner_receipt.handoff.run_id.clone();

        let finalized = store
            .reconcile_after_controller_restart_with(
                8,
                |_, _| Ok(snapshot(JobStatus::Running)),
                |_, _| panic!("nonterminal status must not enter lifecycle finalization"),
            )
            .expect("nonterminal reconciliation");

        assert!(finalized.is_empty());
        let remaining = pending(&store);
        assert_eq!(remaining.len(), 1);
        assert!(
            remaining.contains_key(run_id.as_str()),
            "a nonterminal job keeps its receipt pending"
        );
    }

    #[test]
    fn bounded_batches_reach_terminal_receipts_after_unreachable_jobs_across_restart() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("observations.sqlite");
        let store = ControllerFallbackProjectionStore::open_at(&path);
        let mut runner = Transport::compatible();
        let mut receipts = Vec::new();
        for index in 0..17 {
            receipts.push(
                store
                    .submit_detached(&mut runner, &envelope_for_run(&format!("fair-{index:02}")))
                    .expect("admit exact run"),
            );
        }
        let finalized_runs = Mutex::new(Vec::new());
        let mut finalized = Vec::new();
        for _ in 0..4 {
            let queried = Arc::new(AtomicUsize::new(0));
            let query_count = Arc::clone(&queried);
            finalized.extend(
                ControllerFallbackProjectionStore::open_at(&path)
                    .reconcile_after_controller_restart_with(
                        8,
                        move |_, job_id| {
                            query_count.fetch_add(1, Ordering::SeqCst);
                            let index = job_id
                                .strip_prefix("runner-job-")
                                .expect("runner identity")
                                .parse::<usize>()
                                .expect("job index");
                            match index {
                                1..=8 => Err(Error::internal_unexpected("retained job not found")),
                                16 => Ok(snapshot(JobStatus::Succeeded)),
                                17 => Ok(snapshot(JobStatus::Failed)),
                                _ => Ok(snapshot(JobStatus::Running)),
                            }
                        },
                        |run_id, _| {
                            finalized_runs
                                .lock()
                                .expect("finalized runs")
                                .push(run_id.to_string());
                            Ok(true)
                        },
                    )
                    .expect("bounded reconciliation"),
            );
            assert_eq!(
                queried.load(Ordering::SeqCst),
                8,
                "each startup remains bounded"
            );
        }
        assert_eq!(
            *finalized_runs.lock().expect("finalized runs"),
            vec!["fair-15", "fair-16"],
            "missing and running receipts cannot starve later terminal runs or duplicate finalization",
        );
        assert_eq!(
            finalized,
            vec!["fair-15".to_string(), "fair-16".to_string()]
        );
        // Unreachable and nonterminal receipts stay pending for later passes;
        // finalized receipts leave the queue.
        let remaining = pending(&ControllerFallbackProjectionStore::open_at(&path));
        assert_eq!(remaining.len(), 15);
        assert!(!remaining.contains_key("fair-15"));
        assert!(!remaining.contains_key("fair-16"));
        assert!(remaining.contains_key("fair-00"));
        assert!(remaining.contains_key("fair-14"));
        assert_eq!(
            runner.calls(),
            17,
            "reconciliation never redispatches providers"
        );
    }

    #[test]
    fn concurrent_startups_reserve_distinct_bounded_batches() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("observations.sqlite");
        let store = ControllerFallbackProjectionStore::open_at(&path);
        let mut runner = Transport::compatible();
        for index in 0..17 {
            store
                .submit_detached(
                    &mut runner,
                    &envelope_for_run(&format!("concurrent-{index:02}")),
                )
                .expect("admit run");
        }
        let original_receipts = pending(&store);
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                ControllerFallbackProjectionStore::open_at(path)
                    .reserve_reconciliation_batch(8)
                    .expect("reserve batch")
                    .into_iter()
                    .map(|(run_id, _)| run_id)
                    .collect::<Vec<_>>()
            }));
        }
        let batches = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect::<Vec<_>>();
        assert_eq!(batches[0].len(), 8);
        assert_eq!(batches[1].len(), 8);
        assert!(batches[0].iter().all(|run_id| !batches[1].contains(run_id)));
        assert_eq!(
            pending(&store),
            original_receipts,
            "cursor reservation never rewrites receipts"
        );
        let restarted = ControllerFallbackProjectionStore::open_at(path);
        assert_eq!(
            restarted
                .reserve_reconciliation_batch(1)
                .expect("next batch")[0]
                .0,
            "concurrent-16"
        );
    }

    #[test]
    fn concurrent_reconcilers_enter_lifecycle_finalization_once_and_replay_after_restart() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("observations.sqlite");
        let store = ControllerFallbackProjectionStore::open_at(&path);
        let mut runner = Transport::compatible();
        store
            .submit_detached(&mut runner, &envelope())
            .expect("admit deferred receipt");
        let barrier = Arc::new(Barrier::new(2));
        let finalizations = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let finalizations = Arc::clone(&finalizations);
            workers.push(thread::spawn(move || {
                ControllerFallbackProjectionStore::open_at(path)
                    .reconcile_after_controller_restart_with(
                        8,
                        move |_, _| {
                            barrier.wait();
                            Ok(snapshot(JobStatus::Succeeded))
                        },
                        move |_, _| {
                            finalizations.fetch_add(1, Ordering::SeqCst);
                            Ok(true)
                        },
                    )
            }));
        }
        for worker in workers {
            worker
                .join()
                .expect("worker join")
                .expect("worker reconcile");
        }
        assert_eq!(finalizations.load(Ordering::SeqCst), 1);

        let replay = ControllerFallbackProjectionStore::open_at(path)
            .reconcile_after_controller_restart_with(
                8,
                |_, _| Ok(snapshot(JobStatus::Succeeded)),
                |_, _| panic!("terminal lifecycle CAS must not be re-entered after restart"),
            )
            .expect("restart replay");
        assert!(replay.is_empty());
    }

    #[test]
    fn run_id_supplied_as_a_mission_is_refused() {
        const RUN: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";
        let error = mission_id_from_grouping(RUN).expect_err("run is not a mission");
        assert!(error
            .message
            .contains("encodes a run, not a grouping identity"));
        assert!(error.message.contains(RUN));
    }

    #[test]
    fn distinct_retry_attempts_under_one_cook_mission_are_each_admitted_and_replayed_exactly() {
        let store = store();
        let mut runner = Transport::compatible();
        let first = store
            .submit_detached(&mut runner, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect("first attempt admission");
        let retry = store
            .submit_detached(&mut runner, &envelope_for_run(RETRY_ATTEMPT_RUN))
            .expect("retry attempt admission under the same Cook mission");
        assert_eq!(first.mission_id.as_str(), COOK_MISSION);
        assert_eq!(retry.mission_id, first.mission_id);
        assert_ne!(
            first.runner_receipt.handoff.runner_job_id,
            retry.runner_receipt.handoff.runner_job_id
        );
        let first_replay = store
            .submit_detached(&mut runner, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect("exact first-attempt replay");
        let retry_replay = store
            .submit_detached(&mut runner, &envelope_for_run(RETRY_ATTEMPT_RUN))
            .expect("exact retry-attempt replay");
        assert_eq!(first_replay, first);
        assert_eq!(retry_replay, retry);
        assert_eq!(runner.provider_budget(), 0);
    }

    #[test]
    fn conflicting_runner_receipt_within_the_same_exact_run_fails_closed() {
        let store = store();
        let mut runner = Transport::compatible();
        let admitted = store
            .submit_detached(&mut runner, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect("admit the attempt");
        // A runner that lost its durable store re-mints a different job for
        // the exact run the ledger already bound. That is an evidence conflict.
        let mut reminted = Transport::reminting();
        let error = store
            .submit_detached(&mut reminted, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect_err("same run must not rebind to a different runner receipt");
        assert!(error
            .message
            .contains("already bound to a different runner receipt"));
        assert_eq!(error.details["id"], FIRST_ATTEMPT_RUN);
        let unchanged = store
            .submit_detached(&mut runner, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect("original receipt still replays after the refusal");
        assert_eq!(unchanged, admitted);
    }

    #[test]
    fn reconciliation_finalizes_each_attempt_by_its_exact_run_identity() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("observations.sqlite");
        let store = ControllerFallbackProjectionStore::open_at(&path);
        let mut runner = Transport::compatible();
        let first = store
            .submit_detached(&mut runner, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect("admit first attempt");
        let retry = store
            .submit_detached(&mut runner, &envelope_for_run(RETRY_ATTEMPT_RUN))
            .expect("admit retry attempt");
        let finalizations = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_job = first.runner_receipt.handoff.runner_job_id.clone();

        // Bounded first pass: only the first attempt's runner job is terminal.
        // The retry attempt must stay deferred under its own run identity.
        let finalized = store
            .reconcile_after_controller_restart_with(
                8,
                {
                    let first_job = first_job.clone();
                    move |_, job_id| {
                        if job_id == first_job.as_str() {
                            Ok(snapshot(JobStatus::Succeeded))
                        } else {
                            Ok(snapshot(JobStatus::Running))
                        }
                    }
                },
                {
                    let finalizations = Arc::clone(&finalizations);
                    move |run_id, _| {
                        finalizations
                            .lock()
                            .expect("finalizations")
                            .push(run_id.to_string());
                        Ok(true)
                    }
                },
            )
            .expect("first reconciliation pass");
        assert_eq!(finalized, vec![FIRST_ATTEMPT_RUN.to_string()]);
        assert_eq!(
            finalizations.lock().expect("finalizations").as_slice(),
            [FIRST_ATTEMPT_RUN]
        );
        let remaining = pending(&store);
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(RETRY_ATTEMPT_RUN));

        // Second bounded pass: the retry attempt reaches its own terminal
        // evidence without re-finalizing the first attempt.
        let retry_job = retry.runner_receipt.handoff.runner_job_id.clone();
        let finalized = store
            .reconcile_after_controller_restart_with(
                8,
                move |_, job_id| {
                    assert_eq!(job_id, retry_job.as_str());
                    Ok(snapshot(JobStatus::Succeeded))
                },
                {
                    let finalizations = Arc::clone(&finalizations);
                    move |run_id, _| {
                        finalizations
                            .lock()
                            .expect("finalizations")
                            .push(run_id.to_string());
                        Ok(true)
                    }
                },
            )
            .expect("second reconciliation pass");
        assert_eq!(finalized, vec![RETRY_ATTEMPT_RUN.to_string()]);
        assert_eq!(
            finalizations.lock().expect("finalizations").as_slice(),
            [FIRST_ATTEMPT_RUN, RETRY_ATTEMPT_RUN]
        );
        assert!(pending(&store).is_empty());

        // A restart replays nothing: both attempts are already finalized under
        // their exact run identities.
        let replay = ControllerFallbackProjectionStore::open_at(&path)
            .reconcile_after_controller_restart_with(
                8,
                |_, _| panic!("finalized attempts must not be re-queried after restart"),
                |_, _| panic!("terminal lifecycle CAS must not be re-entered after restart"),
            )
            .expect("restart replay");
        assert!(replay.is_empty());
    }
}
