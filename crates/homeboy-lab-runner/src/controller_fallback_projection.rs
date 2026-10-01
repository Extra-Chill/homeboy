//! Durable controller fallback and later projection for sealed runner staging.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use homeboy_control_plane_contract::{resolve, IdentityKind, MissionId, ResolveError};
use homeboy_core::{Error, Result};

use crate::runner_staging_operation::{
    submit_remote_runner_staging, RemoteRunnerStagingEnvelope, RemoteRunnerStagingReceipt,
    RemoteRunnerStagingTransport, RunnerStagingArtifacts,
};

/// v2 keys receipts, projections, and observations by the exact handoff run
/// id, so each attempt owns its own admission idempotence and finalization
/// while the resolved mission stays grouping data on the record.
const STORE_SCHEMA: &str = "homeboy/controller-fallback-projection/v2";
/// v1 keyed the same records by the resolved parent mission, which rejected a
/// later attempt under the same mission after its runner had admitted it.
/// [`ControllerFallbackProjectionStore::load`] normalizes v1 ledgers in
/// memory; the owning layer persists the v2 layout at its next write.
const STORE_SCHEMA_V1: &str = "homeboy/controller-fallback-projection/v1";
const STARTUP_RECONCILIATION_BATCH_SIZE: usize = 8;
const REMOTE_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

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
            schema: STORE_SCHEMA.to_string(),
            mission_id,
            runner_receipt,
            controller_projection: "deferred".to_string(),
        }
    }

    fn validate_for(&self, envelope: &RemoteRunnerStagingEnvelope) -> Result<()> {
        if self.schema != STORE_SCHEMA
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

/// Terminal evidence from the runner-owned store. The controller copies these
/// identities without replacing or re-materializing runner artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerTerminalEvidence {
    pub outcome: String,
    pub artifacts: RunnerStagingArtifacts,
}

/// The one controller-owned finalization projection for a deferred handoff
/// run. The mission id stays the grouping identity of the owning receipt.
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
    /// Keyed by the exact handoff run id of the admitted runner receipt.
    receipts: BTreeMap<String, DeferredControllerReceipt>,
    /// Keyed by the exact handoff run id of the receipt that owns the
    /// projection; `mission_id` inside stays the grouping identity.
    projections: BTreeMap<String, ControllerMissionProjection>,
    /// Reconciliation observations keyed by exact handoff run id.
    #[serde(default)]
    reconciliation: BTreeMap<String, ReconciliationObservation>,
}

/// Re-key v1 evidence without choosing between conflicting or orphaned records.
fn normalize_v1(state: State) -> Result<State> {
    let State {
        schema: _,
        receipts,
        projections,
        reconciliation,
    } = state;
    let mut normalized = State::default();
    let mut mission_to_run: BTreeMap<String, String> = BTreeMap::new();
    let invalid = |key: &str| {
        Error::validation_invalid_argument(
            "controller_fallback_store",
            "v1 ledger evidence does not identify one exact owning run",
            Some(key.to_string()),
            None,
        )
    };
    for (legacy_key, mut receipt) in receipts {
        let run_key = receipt.runner_receipt.handoff.run_id.clone();
        if legacy_key != receipt.mission_id.as_str()
            || mission_from_handoff_run_id(&run_key)? != receipt.mission_id
            || normalized.receipts.contains_key(&run_key)
        {
            return Err(invalid(&legacy_key));
        }
        mission_to_run.insert(legacy_key, run_key.clone());
        receipt.schema = STORE_SCHEMA.to_string();
        normalized.receipts.insert(run_key, receipt);
    }
    for (legacy_key, projection) in projections {
        let run_key = mission_to_run
            .get(&legacy_key)
            .ok_or_else(|| invalid(&legacy_key))?;
        let receipt = &normalized.receipts[run_key];
        if projection.mission_id != legacy_key
            || projection.runner_id != receipt.runner_receipt.handoff.runner_id
            || projection.runner_job_id != receipt.runner_receipt.handoff.runner_job_id
            || projection.artifacts != receipt.runner_receipt.artifacts
        {
            return Err(invalid(&legacy_key));
        }
        normalized.projections.insert(run_key.clone(), projection);
    }
    for (legacy_key, observation) in reconciliation {
        let run_key = mission_to_run
            .get(&legacy_key)
            .ok_or_else(|| invalid(&legacy_key))?;
        normalized
            .reconciliation
            .insert(run_key.clone(), observation);
    }
    Ok(normalized)
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
/// atomic in its own store; this ledger only records accepted receipts, keyed
/// by each receipt's exact handoff run id. The resolved mission remains
/// grouping data on the record and never gates a later attempt.
pub struct ControllerFallbackProjectionStore {
    path: PathBuf,
}

impl ControllerFallbackProjectionStore {
    /// Shared controller ledger survives daemon restarts independently of the
    /// runner-owned staging store.
    pub fn open_default() -> Result<Self> {
        Self::open_in_roots(homeboy_core::paths::PathRoots::from_environment()?.data())
    }

    /// [`open_default`](Self::open_default) against an explicitly injected data
    /// root.
    ///
    /// The store owns exactly one file below the data root and every later
    /// operation derives from `self.path` (including the sibling `.lock`), so
    /// nothing on this type can reach back into ambient state once it is
    /// constructed. The reconciliation *callbacks* are a separate matter: they
    /// are supplied by the caller and are ambient in the production wiring —
    /// see [`reconcile_on_controller_startup`].
    pub fn open_in_roots(data_root: &Path) -> Result<Self> {
        Self::open(data_root.join("controller-fallback-projection.json"))
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { path: path.into() };
        // v1 mission-keyed ledgers normalize to exact-run ownership in memory
        // (see [`normalize_v1`]); only unknown schemas fail closed.
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
        let _lock = self.lock()?;
        let mut state = self.load()?;
        if let Some(existing) = state.receipts.get(run_key.as_str()) {
            if existing != &receipt {
                return Err(Error::validation_invalid_argument(
                    "idempotency_key",
                    "controller fallback run is already bound to a different runner receipt",
                    Some(run_key),
                    None,
                ));
            }
            return Ok(existing.clone());
        }
        state.receipts.insert(run_key, receipt.clone());
        self.persist(&state)?;
        Ok(receipt)
    }

    /// Reconcile a bounded receipt batch against authoritative runner jobs.
    /// Nonterminal jobs remain deferred; only a terminal snapshot reaches the
    /// agent-task lifecycle finalizer and this projection ledger. Each receipt
    /// reconciles under its exact handoff run id, so one attempt's snapshot
    /// can never finalize another attempt or the parent mission.
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
            .filter(|(run_id, _)| !state.projections.contains_key(*run_id))
            .take(limit)
            .map(|(run_id, receipt)| (run_id.clone(), receipt.clone()))
            .collect::<Vec<_>>();
        let snapshot = Arc::new(snapshot);
        let mut projections = Vec::new();

        for (run_id, receipt) in receipts {
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
                        &run_id,
                        "pending",
                        format!("runner job remains {}", snapshot.job.status.as_str()),
                    )?;
                    continue;
                }
                Err(error) => {
                    self.record_observation(&run_id, "retryable", error.message)?;
                    continue;
                }
            };

            // The ledger lock serializes contenders before they enter lifecycle CAS.
            // A restart or concurrent controller can then replay the same evidence safely.
            let _lock = self.lock()?;
            let mut state = self.load()?;
            if state.projections.contains_key(&run_id) {
                continue;
            }
            if let Err(error) = finalize(&run_id, &snapshot) {
                state.reconciliation.insert(
                    run_id.clone(),
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
                &run_id,
                RunnerTerminalEvidence {
                    outcome: snapshot.job.status.as_str().to_string(),
                    artifacts: receipt.runner_receipt.artifacts,
                },
            )?;
            state.reconciliation.remove(&run_id);
            self.persist(&state)?;
            projections.push(projection);
        }
        Ok(projections)
    }

    /// Projects explicit runner terminal evidence for the exact handoff run
    /// id and fails closed if later evidence differs from the first finalized
    /// projection.
    pub fn project_terminal_evidence(
        &self,
        run_id: &str,
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
                Some(run_id.to_string()),
                None,
            ));
        }
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let projection = self.project_terminal_evidence_in_state(&mut state, run_id, evidence)?;
        self.persist(&state)?;
        Ok(projection)
    }

    fn project_terminal_evidence_in_state(
        &self,
        state: &mut State,
        run_id: &str,
        evidence: RunnerTerminalEvidence,
    ) -> Result<ControllerMissionProjection> {
        let receipt = state.receipts.get(run_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "controller cannot project a run without a deferred runner receipt",
                Some(run_id.to_string()),
                None,
            )
        })?;
        let projection = ControllerMissionProjection {
            mission_id: receipt.mission_id.to_string(),
            runner_id: receipt.runner_receipt.handoff.runner_id.clone(),
            runner_job_id: receipt.runner_receipt.handoff.runner_job_id.clone(),
            terminal_outcome: evidence.outcome,
            artifacts: evidence.artifacts,
            finalization_owner: "controller".to_string(),
        };
        if let Some(existing) = state.projections.get(run_id) {
            if existing != &projection {
                return Err(Error::validation_invalid_argument(
                    "runner_terminal_evidence",
                    "controller run already has a different terminal projection",
                    Some(run_id.to_string()),
                    None,
                ));
            }
            return Ok(existing.clone());
        }
        state
            .projections
            .insert(run_id.to_string(), projection.clone());
        Ok(projection)
    }

    fn record_observation(&self, run_id: &str, state: &str, detail: String) -> Result<()> {
        let _lock = self.lock()?;
        let mut ledger = self.load()?;
        if !ledger.projections.contains_key(run_id) {
            ledger.reconciliation.insert(
                run_id.to_string(),
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
    fn observation(&self, run_id: &str) -> Result<Option<ReconciliationObservation>> {
        Ok(self.load()?.reconciliation.get(run_id).cloned())
    }

    fn load(&self) -> Result<State> {
        if !self.path.exists() {
            return Ok(State::default());
        }
        let state: State = serde_json::from_slice(&fs::read(&self.path).map_err(|error| {
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
        })?;
        if state.schema == STORE_SCHEMA {
            return Ok(state);
        }
        if state.schema == STORE_SCHEMA_V1 {
            return normalize_v1(state);
        }
        Err(Error::validation_invalid_argument(
            "controller_fallback_store",
            "unsupported controller fallback projection store schema",
            Some(self.path.display().to_string()),
            None,
        ))
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
            // This separate lock file protects read-modify-write ledger updates;
            // ledger state itself is atomically replaced by `persist`.
            .truncate(false)
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
                receipt.mission_id.as_str(),
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
                receipt.mission_id.as_str(),
                RunnerTerminalEvidence {
                    outcome: "succeeded".to_string(),
                    artifacts: receipt.runner_receipt.artifacts.clone(),
                },
            )
            .expect("first terminal projection");
        assert!(store
            .project_terminal_evidence(
                receipt.mission_id.as_str(),
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
        let (finished_query, query_completion) = mpsc::channel();

        let projected = store
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
        assert!(projected.is_empty());
        assert_eq!(
            store
                .observation(receipt.mission_id.as_str())
                .expect("observation"),
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
            store
                .observation(receipt.mission_id.as_str())
                .expect("observation"),
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
        let path = directory.path().join("controller.json");
        let store = ControllerFallbackProjectionStore::open(&path).expect("store");
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
        let projected = store
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
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].mission_id, COOK_MISSION);
        assert_eq!(
            projected[0].runner_job_id,
            first.runner_receipt.handoff.runner_job_id
        );
        assert_eq!(projected[0].artifacts, first.runner_receipt.artifacts);
        assert_eq!(
            finalizations.lock().expect("finalizations").as_slice(),
            [FIRST_ATTEMPT_RUN]
        );
        assert_eq!(
            store
                .observation(RETRY_ATTEMPT_RUN)
                .expect("retry observation"),
            Some(ReconciliationObservation {
                state: "pending".to_string(),
                detail: "runner job remains running".to_string(),
            })
        );

        // Second bounded pass: the retry attempt reaches its own terminal
        // evidence without re-finalizing the first attempt.
        let retry_job = retry.runner_receipt.handoff.runner_job_id.clone();
        let projected = store
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
        assert_eq!(projected.len(), 1);
        assert_eq!(
            projected[0].runner_job_id,
            retry.runner_receipt.handoff.runner_job_id
        );
        assert_eq!(projected[0].artifacts, retry.runner_receipt.artifacts);
        assert_eq!(
            finalizations.lock().expect("finalizations").as_slice(),
            [FIRST_ATTEMPT_RUN, RETRY_ATTEMPT_RUN]
        );

        // A restart replays nothing: both attempts are already projected under
        // their exact run identities.
        let replay = ControllerFallbackProjectionStore::open(&path)
            .expect("restarted store")
            .reconcile_after_controller_restart_with(
                8,
                |_, _| panic!("finalized attempts must not be re-queried after restart"),
                |_, _| panic!("terminal lifecycle CAS must not be re-entered after restart"),
            )
            .expect("restart replay");
        assert!(replay.is_empty());
    }

    #[test]
    fn v1_mission_keyed_ledger_is_normalized_to_exact_run_ownership_without_loss() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("controller.json");
        let envelope = envelope_for_run(FIRST_ATTEMPT_RUN);
        let mut runner = Transport::compatible();
        let runner_receipt =
            crate::runner_staging_operation::submit_remote_runner_staging(&mut runner, &envelope)
                .expect("runner admission for the v1-era attempt");
        let v1_receipt = DeferredControllerReceipt {
            schema: STORE_SCHEMA_V1.to_string(),
            mission_id: mission_from_handoff_run_id(FIRST_ATTEMPT_RUN).expect("grouping mission"),
            runner_receipt: runner_receipt.clone(),
            controller_projection: "deferred".to_string(),
        };
        let v1_projection = ControllerMissionProjection {
            mission_id: COOK_MISSION.to_string(),
            runner_id: v1_receipt.runner_receipt.handoff.runner_id.clone(),
            runner_job_id: v1_receipt.runner_receipt.handoff.runner_job_id.clone(),
            terminal_outcome: "succeeded".to_string(),
            artifacts: v1_receipt.runner_receipt.artifacts.clone(),
            finalization_owner: "controller".to_string(),
        };
        let mut v1_state = State {
            schema: STORE_SCHEMA_V1.to_string(),
            receipts: BTreeMap::new(),
            projections: BTreeMap::new(),
            reconciliation: BTreeMap::new(),
        };
        v1_state
            .receipts
            .insert(COOK_MISSION.to_string(), v1_receipt.clone());
        v1_state
            .projections
            .insert(COOK_MISSION.to_string(), v1_projection.clone());
        v1_state.reconciliation.insert(
            COOK_MISSION.to_string(),
            ReconciliationObservation {
                state: "pending".to_string(),
                detail: "runner job remains running".to_string(),
            },
        );
        ControllerFallbackProjectionStore { path: path.clone() }
            .persist(&v1_state)
            .expect("seed v1 ledger");

        let store = ControllerFallbackProjectionStore::open(&path).expect("v1 ledger opens");
        let normalized = store.load().expect("normalized state");
        assert_eq!(normalized.schema, STORE_SCHEMA);
        let migrated_receipt = normalized
            .receipts
            .get(FIRST_ATTEMPT_RUN)
            .expect("receipt re-keyed to its exact run");
        assert_eq!(migrated_receipt.mission_id.as_str(), COOK_MISSION);
        assert_eq!(
            migrated_receipt.runner_receipt, v1_receipt.runner_receipt,
            "runner-owned receipt payload must survive the transition unchanged"
        );
        let migrated_projection = normalized
            .projections
            .get(FIRST_ATTEMPT_RUN)
            .expect("projection re-keyed to its exact run");
        assert_eq!(*migrated_projection, v1_projection);
        assert!(normalized.reconciliation.contains_key(FIRST_ATTEMPT_RUN));

        // The earlier attempt still replays exactly after the transition.
        let mut restarted = Transport::compatible();
        let replay = store
            .submit_detached(&mut restarted, &envelope_for_run(FIRST_ATTEMPT_RUN))
            .expect("earlier attempt replays after the transition");
        assert_eq!(replay.runner_receipt, v1_receipt.runner_receipt);
        // And a later attempt under the same Cook mission is admitted.
        let retry = store
            .submit_detached(&mut restarted, &envelope_for_run(RETRY_ATTEMPT_RUN))
            .expect("retry attempt admitted after the transition");
        assert_eq!(retry.mission_id.as_str(), COOK_MISSION);

        // The first write after the transition persists the v2 layout with
        // both attempts retained.
        let reloaded = store.load().expect("reload persisted ledger");
        assert_eq!(reloaded.schema, STORE_SCHEMA);
        assert_eq!(reloaded.receipts.len(), 2);
        assert!(reloaded.receipts.contains_key(FIRST_ATTEMPT_RUN));
        assert!(reloaded.receipts.contains_key(RETRY_ATTEMPT_RUN));
        assert_eq!(
            reloaded
                .projections
                .get(FIRST_ATTEMPT_RUN)
                .map(|projection| projection.runner_job_id.as_str()),
            Some(v1_receipt.runner_receipt.handoff.runner_job_id.as_str())
        );
    }

    #[test]
    fn malformed_v1_ownership_is_rejected_without_changing_recorded_evidence() {
        for malformed in ["empty_run", "wrong_job", "orphan_observation"] {
            let directory = tempdir().expect("temp directory");
            let path = directory.path().join("controller.json");
            let mut runner = Transport::compatible();
            let runner_receipt =
                submit_remote_runner_staging(&mut runner, &envelope_for_run(FIRST_ATTEMPT_RUN))
                    .expect("admission");
            let mut state = State {
                schema: STORE_SCHEMA_V1.to_string(),
                ..State::default()
            };
            let mut receipt = DeferredControllerReceipt::new(
                mission_from_handoff_run_id(FIRST_ATTEMPT_RUN).expect("mission"),
                runner_receipt,
            );
            receipt.schema = STORE_SCHEMA_V1.to_string();
            if malformed == "empty_run" {
                receipt.runner_receipt.handoff.run_id.clear();
            } else if malformed == "wrong_job" {
                state.projections.insert(
                    COOK_MISSION.to_string(),
                    ControllerMissionProjection {
                        mission_id: COOK_MISSION.to_string(),
                        runner_id: receipt.runner_receipt.handoff.runner_id.clone(),
                        runner_job_id: Uuid::new_v4().to_string(),
                        terminal_outcome: "succeeded".to_string(),
                        artifacts: receipt.runner_receipt.artifacts.clone(),
                        finalization_owner: "controller".to_string(),
                    },
                );
            } else {
                state.reconciliation.insert(
                    "unowned-mission".to_string(),
                    ReconciliationObservation {
                        state: "pending".to_string(),
                        detail: "retained evidence".to_string(),
                    },
                );
            }
            state.receipts.insert(COOK_MISSION.to_string(), receipt);
            let bytes = serde_json::to_vec(&state).expect("serialize evidence");
            fs::write(&path, &bytes).expect("seed ledger");
            assert!(ControllerFallbackProjectionStore::open(&path).is_err());
            assert_eq!(fs::read(&path).expect("retained ledger"), bytes);
        }
    }
}
