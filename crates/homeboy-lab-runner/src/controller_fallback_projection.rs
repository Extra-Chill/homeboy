//! Durable controller fallback and later projection for sealed runner staging.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use homeboy_core::{Error, Result};

use crate::runner_staging_operation::{
    submit_remote_runner_staging, RemoteRunnerStagingEnvelope, RemoteRunnerStagingReceipt,
    RemoteRunnerStagingTransport, RunnerStagingArtifacts,
};

const STORE_SCHEMA: &str = "homeboy/controller-fallback-projection/v1";
const STARTUP_RECONCILIATION_BATCH_SIZE: usize = 8;
const REMOTE_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// Controller-visible durable receipt for a runner-owned admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeferredControllerReceipt {
    pub schema: String,
    pub mission_id: String,
    pub runner_receipt: RemoteRunnerStagingReceipt,
    pub controller_projection: String,
}

impl DeferredControllerReceipt {
    fn new(mission_id: impl Into<String>, runner_receipt: RemoteRunnerStagingReceipt) -> Self {
        Self {
            schema: STORE_SCHEMA.to_string(),
            mission_id: mission_id.into(),
            runner_receipt,
            controller_projection: "deferred".to_string(),
        }
    }

    fn validate_for(&self, envelope: &RemoteRunnerStagingEnvelope) -> Result<()> {
        if self.schema != STORE_SCHEMA
            || self.mission_id.trim().is_empty()
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

/// Terminal evidence from the runner-owned store. The controller copies these
/// identities without replacing or re-materializing runner artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerTerminalEvidence {
    pub outcome: String,
    pub artifacts: RunnerStagingArtifacts,
}

/// The one controller-owned finalization projection for a deferred mission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControllerMissionProjection {
    pub mission_id: String,
    pub runner_id: String,
    #[serde(alias = "runner_staging_id")]
    pub runner_job_id: String,
    pub terminal_outcome: String,
    pub artifacts: RunnerStagingArtifacts,
    pub finalization_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: String,
    receipts: BTreeMap<String, DeferredControllerReceipt>,
    projections: BTreeMap<String, ControllerMissionProjection>,
    #[serde(default)]
    reconciliation: BTreeMap<String, ReconciliationObservation>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: STORE_SCHEMA.to_string(),
            receipts: BTreeMap::new(),
            projections: BTreeMap::new(),
            reconciliation: BTreeMap::new(),
        }
    }
}

/// Durable status for work intentionally left for a later bounded pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationObservation {
    pub state: String,
    pub detail: String,
}

/// File-backed controller receipt/projection ledger. Runner admission is
/// atomic in its own store; this ledger only records accepted receipts.
pub struct ControllerFallbackProjectionStore {
    path: PathBuf,
}

impl ControllerFallbackProjectionStore {
    /// Shared controller ledger survives daemon restarts independently of the
    /// runner-owned staging store.
    pub fn open_default() -> Result<Self> {
        Self::open(homeboy_core::paths::homeboy_data()?.join("controller-fallback-projection.json"))
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { path: path.into() };
        if store.load()?.schema != STORE_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "controller_fallback_store",
                "unsupported controller fallback projection store schema",
                Some(store.path.display().to_string()),
                None,
            ));
        }
        Ok(store)
    }

    /// Preflight happens inside `submit_remote_runner_staging` before the
    /// transport mutation boundary, so refusals spend no provider budget.
    pub fn submit_detached<T: RemoteRunnerStagingTransport>(
        &self,
        transport: &mut T,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<DeferredControllerReceipt> {
        let receipt = DeferredControllerReceipt::new(
            &envelope.handoff.run_id,
            submit_remote_runner_staging(transport, envelope)?,
        );
        receipt.validate_for(envelope)?;
        let _lock = self.lock()?;
        let mut state = self.load()?;
        if let Some(existing) = state.receipts.get(&receipt.mission_id) {
            if existing != &receipt {
                return Err(Error::validation_invalid_argument(
                    "idempotency_key",
                    "controller fallback mission is already bound to a different runner receipt",
                    Some(receipt.mission_id),
                    None,
                ));
            }
            return Ok(existing.clone());
        }
        state
            .receipts
            .insert(receipt.mission_id.clone(), receipt.clone());
        self.persist(&state)?;
        Ok(receipt)
    }

    /// Reconcile a bounded receipt batch against authoritative runner jobs.
    /// Nonterminal jobs remain deferred; only a terminal snapshot reaches the
    /// agent-task lifecycle finalizer and this projection ledger.
    pub fn reconcile_after_controller_restart_with<Snapshot, Finalize>(
        &self,
        limit: usize,
        snapshot: Snapshot,
        finalize: Finalize,
    ) -> Result<Vec<ControllerMissionProjection>>
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
    ) -> Result<Vec<ControllerMissionProjection>>
    where
        Snapshot: Fn(&str, &str) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot>
            + Send
            + Sync
            + 'static,
        Finalize: Fn(&str, &homeboy_core::api_jobs::RunnerJobLogSnapshot) -> Result<bool>,
    {
        let state = self.load()?;
        let receipts = state
            .receipts
            .iter()
            .filter(|(mission_id, _)| !state.projections.contains_key(*mission_id))
            .take(limit)
            .map(|(mission_id, receipt)| (mission_id.clone(), receipt.clone()))
            .collect::<Vec<_>>();
        let snapshot = Arc::new(snapshot);
        let mut projections = Vec::new();

        for (mission_id, receipt) in receipts {
            let result = remote_snapshot_with_timeout(
                Arc::clone(&snapshot),
                receipt.runner_receipt.handoff.runner_id.clone(),
                receipt.runner_receipt.handoff.runner_job_id.clone(),
                timeout,
            );
            let snapshot = match result {
                Ok(snapshot) if snapshot.job.status.is_terminal() => snapshot,
                Ok(snapshot) => {
                    self.record_observation(
                        &mission_id,
                        "pending",
                        format!(
                            "runner job remains {}",
                            snapshot.job.status.daemon_status_label()
                        ),
                    )?;
                    continue;
                }
                Err(error) => {
                    self.record_observation(&mission_id, "retryable", error.message)?;
                    continue;
                }
            };

            // The ledger lock serializes contenders before they enter lifecycle CAS.
            // A restart or concurrent controller can then replay the same evidence safely.
            let _lock = self.lock()?;
            let mut state = self.load()?;
            if state.projections.contains_key(&mission_id) {
                continue;
            }
            if let Err(error) = finalize(&mission_id, &snapshot) {
                state.reconciliation.insert(
                    mission_id.clone(),
                    ReconciliationObservation {
                        state: "retryable".to_string(),
                        detail: error.message,
                    },
                );
                self.persist(&state)?;
                continue;
            }
            let projection = self.project_terminal_evidence_in_state(
                &mut state,
                &mission_id,
                RunnerTerminalEvidence {
                    outcome: snapshot.job.status.daemon_status_label().to_string(),
                    artifacts: receipt.runner_receipt.artifacts,
                },
            )?;
            state.reconciliation.remove(&mission_id);
            self.persist(&state)?;
            projections.push(projection);
        }
        Ok(projections)
    }

    /// Projects explicit runner terminal evidence and fails closed if later
    /// evidence differs from the first finalized projection.
    pub fn project_terminal_evidence(
        &self,
        mission_id: &str,
        evidence: RunnerTerminalEvidence,
    ) -> Result<ControllerMissionProjection> {
        if evidence.outcome.trim().is_empty()
            || evidence.artifacts.lifecycle_id.trim().is_empty()
            || evidence.artifacts.source_artifact_id.trim().is_empty()
            || evidence.artifacts.workspace_artifact_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "runner_terminal_evidence",
                "runner terminal evidence requires an outcome and all staged artifacts",
                Some(mission_id.to_string()),
                None,
            ));
        }
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let projection =
            self.project_terminal_evidence_in_state(&mut state, mission_id, evidence)?;
        self.persist(&state)?;
        Ok(projection)
    }

    fn project_terminal_evidence_in_state(
        &self,
        state: &mut State,
        mission_id: &str,
        evidence: RunnerTerminalEvidence,
    ) -> Result<ControllerMissionProjection> {
        let receipt = state.receipts.get(mission_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "mission_id",
                "controller cannot project a mission without a deferred runner receipt",
                Some(mission_id.to_string()),
                None,
            )
        })?;
        let projection = ControllerMissionProjection {
            mission_id: mission_id.to_string(),
            runner_id: receipt.runner_receipt.handoff.runner_id.clone(),
            runner_job_id: receipt.runner_receipt.handoff.runner_job_id.clone(),
            terminal_outcome: evidence.outcome,
            artifacts: evidence.artifacts,
            finalization_owner: "controller".to_string(),
        };
        if let Some(existing) = state.projections.get(mission_id) {
            if existing != &projection {
                return Err(Error::validation_invalid_argument(
                    "runner_terminal_evidence",
                    "controller mission already has a different terminal projection",
                    Some(mission_id.to_string()),
                    None,
                ));
            }
            return Ok(existing.clone());
        }
        state
            .projections
            .insert(mission_id.to_string(), projection.clone());
        Ok(projection)
    }

    fn record_observation(&self, mission_id: &str, state: &str, detail: String) -> Result<()> {
        let _lock = self.lock()?;
        let mut ledger = self.load()?;
        if !ledger.projections.contains_key(mission_id) {
            ledger.reconciliation.insert(
                mission_id.to_string(),
                ReconciliationObservation {
                    state: state.to_string(),
                    detail,
                },
            );
            self.persist(&ledger)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn observation(&self, mission_id: &str) -> Result<Option<ReconciliationObservation>> {
        Ok(self.load()?.reconciliation.get(mission_id).cloned())
    }

    fn load(&self) -> Result<State> {
        if !self.path.exists() {
            return Ok(State::default());
        }
        serde_json::from_slice(&fs::read(&self.path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read {}", self.path.display())),
            )
        })?)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse {}", self.path.display())),
            )
        })
    }

    fn persist(&self, state: &State) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        let bytes = serde_json::to_vec(state).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize controller fallback projection".to_string()),
            )
        })?;
        let parent = self.path.parent().expect("ledger path has parent");
        let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create ledger temporary in {}", parent.display())),
            )
        })?;
        temporary.write_all(&bytes).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write controller fallback projection".to_string()),
            )
        })?;
        temporary.as_file().sync_all().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("sync controller fallback projection".to_string()),
            )
        })?;
        temporary.persist(&self.path).map_err(|error| {
            Error::internal_io(
                error.error.to_string(),
                Some(format!("publish {}", self.path.display())),
            )
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("sync {}", parent.display())),
                )
            })
    }

    fn lock(&self) -> Result<File> {
        let parent = self.path.parent().expect("ledger path has parent");
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("open controller fallback lock".to_string()),
                )
            })?;
        file.lock_exclusive().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("lock controller fallback ledger".to_string()),
            )
        })?;
        Ok(file)
    }
}

/// Production startup reconciliation for deferred runner staging. It reads at
/// most eight runner jobs so ordinary CLI startup remains bounded by work count.
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
    use crate::runner_staging_operation::tests_support::{envelope, Transport};
    use homeboy_core::api_jobs::{Job, JobStatus, RunnerJobLogSnapshot};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::time::Instant;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn store() -> ControllerFallbackProjectionStore {
        ControllerFallbackProjectionStore::open(
            tempdir().expect("temp").keep().join("controller.json"),
        )
        .expect("store")
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
    fn terminal_runner_evidence_projects_once_after_restart() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope)
            .expect("admit");
        let projected = store
            .project_terminal_evidence(
                &receipt.mission_id,
                RunnerTerminalEvidence {
                    outcome: "succeeded".to_string(),
                    artifacts: receipt.runner_receipt.artifacts.clone(),
                },
            )
            .expect("project");
        assert_eq!(projected.terminal_outcome, "succeeded");
        assert_eq!(projected.artifacts, receipt.runner_receipt.artifacts);
    }

    #[test]
    fn explicit_terminal_evidence_cannot_replace_startup_projection() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope)
            .expect("admit");
        store
            .project_terminal_evidence(
                &receipt.mission_id,
                RunnerTerminalEvidence {
                    outcome: "succeeded".to_string(),
                    artifacts: receipt.runner_receipt.artifacts.clone(),
                },
            )
            .expect("first terminal projection");
        assert!(store
            .project_terminal_evidence(
                &receipt.mission_id,
                RunnerTerminalEvidence {
                    outcome: "failed".to_string(),
                    artifacts: receipt.runner_receipt.artifacts,
                },
            )
            .is_err());
    }

    #[test]
    fn startup_reconciliation_returns_before_a_blocked_remote_query() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope)
            .expect("admit deferred receipt");
        let started = Instant::now();

        let projected = store
            .reconcile_after_controller_restart_with_timeout(
                8,
                Duration::from_millis(20),
                |_, _| {
                    thread::sleep(Duration::from_secs(1));
                    Ok(snapshot(JobStatus::Succeeded))
                },
                |_, _| Ok(true),
            )
            .expect("bounded reconciliation");

        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(projected.is_empty());
        assert_eq!(
            store.observation(&receipt.mission_id).expect("observation"),
            Some(ReconciliationObservation {
                state: "retryable".to_string(),
                detail: "runner status query timed out after 20ms: timed out waiting on channel"
                    .to_string(),
            })
        );
    }

    #[test]
    fn nonterminal_runner_status_stays_pending_for_the_next_bounded_pass() {
        let store = store();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope())
            .expect("admit deferred receipt");

        let projected = store
            .reconcile_after_controller_restart_with(
                8,
                |_, _| Ok(snapshot(JobStatus::Running)),
                |_, _| panic!("nonterminal status must not enter lifecycle finalization"),
            )
            .expect("nonterminal reconciliation");

        assert!(projected.is_empty());
        assert_eq!(
            store.observation(&receipt.mission_id).expect("observation"),
            Some(ReconciliationObservation {
                state: "pending".to_string(),
                detail: "runner job remains running".to_string(),
            })
        );
    }

    #[test]
    fn terminal_success_and_failure_project_the_staged_artifacts() {
        for (status, outcome) in [
            (JobStatus::Succeeded, "succeeded"),
            (JobStatus::Failed, "failed"),
        ] {
            let store = store();
            let envelope = envelope();
            let mut runner = Transport::compatible();
            let receipt = store
                .submit_detached(&mut runner, &envelope)
                .expect("admit deferred receipt");
            let projected = store
                .reconcile_after_controller_restart_with(
                    8,
                    move |_, _| Ok(snapshot(status)),
                    |_, _| Ok(true),
                )
                .expect("terminal reconciliation");

            assert_eq!(projected.len(), 1);
            assert_eq!(projected[0].terminal_outcome, outcome);
            assert_eq!(projected[0].artifacts, receipt.runner_receipt.artifacts);
        }
    }

    #[test]
    fn concurrent_reconcilers_enter_lifecycle_finalization_once_and_replay_after_restart() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("controller.json");
        let store = ControllerFallbackProjectionStore::open(&path).expect("store");
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
                ControllerFallbackProjectionStore::open(path)
                    .expect("concurrent store")
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

        let replay = ControllerFallbackProjectionStore::open(path)
            .expect("restarted store")
            .reconcile_after_controller_restart_with(
                8,
                |_, _| Ok(snapshot(JobStatus::Succeeded)),
                |_, _| panic!("terminal lifecycle CAS must not be re-entered after restart"),
            )
            .expect("restart replay");
        assert!(replay.is_empty());
    }
}
