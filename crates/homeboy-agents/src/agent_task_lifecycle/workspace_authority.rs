//! Terminal workspace authority, kept as one coordination record per run in
//! the observation store (#15947).
//!
//! The record under namespace [`NAMESPACE`] is keyed by run id and carries the
//! exact runner binding. Its lifecycle is an explicit state field:
//! [`STATE_HELD`] while the retained workspace is protected, then
//! [`STATE_RELEASE_PENDING`] while the workspace is being removed, and finally
//! the record is deleted. Every transition is a revision compare-and-set, so
//! two concurrent transitions can never both apply. Completed releases leave
//! an append-only audit entry so a late terminal projection cannot resurrect
//! authority over a workspace that was already reaped.

use homeboy_core::observation::coordination::{
    CoordinationRecord, ExpectedRevision, RecordOutcome, RecordWrite, MAX_COORDINATION_AUDIT_READ,
};
use homeboy_core::observation::ObservationStore;
use serde::{Deserialize, Serialize};

use super::*;

/// Coordination namespace owned by terminal workspace authority.
const NAMESPACE: &str = "workspace-terminal-authority";
const STATE_HELD: &str = "held";
const STATE_RELEASE_PENDING: &str = "release_pending";
const AUDIT_EVENT_RELEASED: &str = "released";
/// Bound on compare-and-set retries for one transition under contention.
const MAX_CAS_ATTEMPTS: usize = 16;

const WORKSPACE_TERMINAL_AUTHORITY_SCHEMA: &str = "homeboy/workspace-terminal-authority/v1";

/// Terminal authority retained independently from compactable run records until
/// the runner workspace that it protects has been durably removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTerminalAuthorityReceipt {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub remote_workspace: String,
}

/// A retained Lab workspace that remains authoritatively bound to one terminal
/// agent task. Callers address files below this root relatively.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetainedWorkspace {
    pub run_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub remote_workspace: String,
}

/// Payload of one authority record. `runner_job_id` is present once a terminal
/// receipt was persisted; a release may begin before any receipt exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AuthorityPayload {
    schema: String,
    run_id: String,
    runner_id: String,
    remote_workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runner_job_id: Option<String>,
}

impl AuthorityPayload {
    fn binds(&self, run_id: &str, runner_id: &str, remote_workspace: &str) -> bool {
        self.run_id == run_id
            && self.runner_id == runner_id
            && self.remote_workspace == remote_workspace
    }
}

/// The binding recorded in a release audit entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ReleasedBinding {
    runner_id: String,
    remote_workspace: String,
}

/// What a begin-release call did. Exactly one concurrent caller observes
/// [`BeginRelease::Began`] for one held record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginRelease {
    Began,
    AlreadyPending,
    /// The run is bound to a different runner workspace; nothing to freeze.
    Unbound,
}

#[derive(Debug, Clone)]
enum StoreLocation {
    Roots(paths::PathRoots),
    #[cfg(test)]
    Database(std::path::PathBuf),
}

/// Terminal workspace authority bound to one installation's observation store.
#[derive(Debug, Clone)]
pub(crate) struct WorkspaceTerminalAuthorityStore {
    location: StoreLocation,
}

impl WorkspaceTerminalAuthorityStore {
    /// Bind terminal workspace authority to explicit roots (#7505).
    pub(crate) fn from_roots(roots: &paths::PathRoots) -> Self {
        Self {
            location: StoreLocation::Roots(roots.clone()),
        }
    }

    /// Resolve the process roots *once* for one whole authority operation, so
    /// one authority decision reads and writes one installation (#7505).
    fn from_environment() -> Result<Self> {
        Ok(Self::from_roots(&paths::PathRoots::from_environment()?))
    }

    #[cfg(test)]
    fn at_database(path: std::path::PathBuf) -> Self {
        Self {
            location: StoreLocation::Database(path),
        }
    }

    fn open(&self) -> Result<ObservationStore> {
        match &self.location {
            StoreLocation::Roots(roots) => {
                ObservationStore::open_initialized_for_lifecycle_in_roots(roots)
            }
            #[cfg(test)]
            StoreLocation::Database(path) => {
                ObservationStore::open_initialized_for_lifecycle_at(path.clone())
            }
        }
    }

    pub(crate) fn persist_terminal_from_record(&self, record: &AgentTaskRunRecord) -> Result<()> {
        if !record.state.is_terminal() {
            return Ok(());
        }
        let Some(identity) = accepted_lab_runner_job_identity_from_record(record) else {
            return Ok(());
        };
        let Some(remote_workspace) = record
            .metadata
            .get("remote_workspace")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        self.persist(WorkspaceTerminalAuthorityReceipt {
            schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
            run_id: record.run_id.clone(),
            runner_id: identity.runner_id,
            runner_job_id: identity.runner_job_id,
            remote_workspace: remote_workspace.to_string(),
        })
    }

    fn persist(&self, receipt: WorkspaceTerminalAuthorityReceipt) -> Result<()> {
        let store = self.open()?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            // A completed release suppresses late terminal projections: the
            // workspace this receipt would protect has already been removed.
            if was_released(
                &store,
                &receipt.run_id,
                &receipt.runner_id,
                &receipt.remote_workspace,
            )? {
                return Ok(());
            }
            let Some((record, payload)) = read_authority(&store, &receipt.run_id)? else {
                let payload = to_payload(&AuthorityPayload {
                    schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
                    run_id: receipt.run_id.clone(),
                    runner_id: receipt.runner_id.clone(),
                    remote_workspace: receipt.remote_workspace.clone(),
                    runner_job_id: Some(receipt.runner_job_id.clone()),
                })?;
                match put(
                    &store,
                    &receipt.run_id,
                    ExpectedRevision::Absent,
                    STATE_HELD,
                    &payload,
                )? {
                    RecordOutcome::Applied(_) => return Ok(()),
                    RecordOutcome::Conflict { .. } => continue,
                }
            };
            let consistent = receipt.schema == WORKSPACE_TERMINAL_AUTHORITY_SCHEMA
                && payload.binds(
                    &receipt.run_id,
                    &receipt.runner_id,
                    &receipt.remote_workspace,
                );
            return match record.state.as_str() {
                // A release in flight already owns this workspace's fate.
                STATE_RELEASE_PENDING if consistent => Ok(()),
                STATE_HELD
                    if consistent
                        && payload.runner_job_id.as_deref()
                            == Some(receipt.runner_job_id.as_str()) =>
                {
                    Ok(())
                }
                _ => Err(Error::validation_invalid_argument(
                    "workspace_terminal_authority",
                    "workspace terminal authority receipt conflicts with existing immutable receipt",
                    Some(receipt.run_id.clone()),
                    None,
                )),
            };
        }
        Err(contention_error(&receipt.run_id))
    }

    fn resolve_authority(
        &self,
        run_id: &str,
        runner_id: &str,
        remote_workspace: &str,
        job_id: Option<&str>,
    ) -> Result<Option<WorkspaceTerminalAuthorityReceipt>> {
        let store = self.open()?;
        let current = read_authority(&store, run_id)?;
        let bound = current
            .as_ref()
            .filter(|(_, payload)| payload.binds(run_id, runner_id, remote_workspace));
        let Some((record, payload)) = bound else {
            if was_released(&store, run_id, runner_id, remote_workspace)? {
                return Err(Error::validation_invalid_argument(
                    "workspace_terminal_authority",
                    "workspace terminal authority release is released",
                    Some(run_id.to_string()),
                    None,
                ));
            }
            return Ok(None);
        };
        match record.state.as_str() {
            STATE_RELEASE_PENDING => {
                return Err(Error::validation_invalid_argument(
                    "workspace_terminal_authority",
                    "workspace terminal authority release is pending",
                    Some(run_id.to_string()),
                    None,
                ))
            }
            STATE_HELD => {}
            _ => return Err(malformed_record_error(run_id)),
        }
        let receipt_job = payload.runner_job_id.clone().unwrap_or_default();
        let substituted_job = job_id.is_some_and(|job_id| job_id != receipt_job);
        let valid = !receipt_job.is_empty() && !substituted_job;
        if valid {
            return Ok(Some(WorkspaceTerminalAuthorityReceipt {
                schema: payload.schema.clone(),
                run_id: payload.run_id.clone(),
                runner_id: payload.runner_id.clone(),
                runner_job_id: receipt_job,
                remote_workspace: payload.remote_workspace.clone(),
            }));
        }
        // A substituted job identity is a conflicting authority assertion.
        // Freeze cleanup so later compaction cannot make the workspace appear
        // safely addressable after a failed exact-binding check. The freeze
        // lands in the same installation whose record just failed (#7505).
        if substituted_job {
            let _ = self.begin_release_in(&store, run_id, runner_id, remote_workspace);
        }
        Err(Error::validation_invalid_argument(
            "workspace_terminal_authority",
            "workspace terminal authority receipt is malformed or contradicts workspace ownership",
            Some(run_id.to_string()),
            None,
        ))
    }

    fn resolve_retained_workspace(&self, run_id: &str) -> Result<RetainedWorkspace> {
        let store = self.open()?;
        let Some((_, payload)) = read_authority(&store, run_id)? else {
            return Err(if has_release_audit(&store, run_id)? {
                unavailable_retained_workspace_error(run_id)
            } else {
                Error::validation_invalid_argument(
                    "run_id",
                    "agent task has no retained Lab workspace record",
                    Some(run_id.to_string()),
                    None,
                )
            });
        };
        drop(store);
        let receipt = self
            .resolve_authority(
                run_id,
                &payload.runner_id,
                &payload.remote_workspace,
                payload.runner_job_id.as_deref(),
            )?
            .ok_or_else(|| unavailable_retained_workspace_error(run_id))?;
        Ok(RetainedWorkspace {
            run_id: receipt.run_id,
            runner_id: receipt.runner_id,
            runner_job_id: receipt.runner_job_id,
            remote_workspace: receipt.remote_workspace,
        })
    }

    fn release_is_pending(
        &self,
        run_id: &str,
        runner_id: &str,
        remote_workspace: &str,
    ) -> Result<bool> {
        let store = self.open()?;
        Ok(
            read_authority(&store, run_id)?.is_some_and(|(record, payload)| {
                record.state == STATE_RELEASE_PENDING
                    && payload.binds(run_id, runner_id, remote_workspace)
            }),
        )
    }

    fn begin_release(&self, run_id: &str, runner_id: &str, remote_workspace: &str) -> Result<()> {
        let store = self.open()?;
        self.begin_release_in(&store, run_id, runner_id, remote_workspace)
            .map(|_| ())
    }

    fn begin_release_in(
        &self,
        store: &ObservationStore,
        run_id: &str,
        runner_id: &str,
        remote_workspace: &str,
    ) -> Result<BeginRelease> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let (expected, payload) = match read_authority(store, run_id)? {
                None => {
                    if was_released(store, run_id, runner_id, remote_workspace)? {
                        return Err(Error::validation_invalid_argument(
                            "workspace_terminal_authority",
                            "workspace terminal authority was already released",
                            Some(run_id.to_string()),
                            None,
                        ));
                    }
                    // A release may begin before any terminal receipt exists;
                    // the pending record still blocks a racing projection.
                    (
                        ExpectedRevision::Absent,
                        AuthorityPayload {
                            schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
                            run_id: run_id.to_string(),
                            runner_id: runner_id.to_string(),
                            remote_workspace: remote_workspace.to_string(),
                            runner_job_id: None,
                        },
                    )
                }
                Some((_, payload)) if !payload.binds(run_id, runner_id, remote_workspace) => {
                    return Ok(BeginRelease::Unbound)
                }
                Some((record, _)) if record.state == STATE_RELEASE_PENDING => {
                    return Ok(BeginRelease::AlreadyPending)
                }
                Some((record, payload)) if record.state == STATE_HELD => {
                    (ExpectedRevision::Revision(record.revision), payload)
                }
                Some(_) => return Err(malformed_record_error(run_id)),
            };
            match put(
                store,
                run_id,
                expected,
                STATE_RELEASE_PENDING,
                &to_payload(&payload)?,
            )? {
                RecordOutcome::Applied(_) => return Ok(BeginRelease::Began),
                RecordOutcome::Conflict { .. } => continue,
            }
        }
        Err(contention_error(run_id))
    }

    fn abort_release(&self, run_id: &str, runner_id: &str, remote_workspace: &str) -> Result<()> {
        let store = self.open()?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some((record, payload)) = read_authority(&store, run_id)? else {
                return Ok(());
            };
            if !payload.binds(run_id, runner_id, remote_workspace) || record.state == STATE_HELD {
                return Ok(());
            }
            if record.state != STATE_RELEASE_PENDING {
                return Err(malformed_record_error(run_id));
            }
            // Restore the held receipt, or drop a release that never had one.
            let outcome = if payload.runner_job_id.is_some() {
                put(
                    &store,
                    run_id,
                    ExpectedRevision::Revision(record.revision),
                    STATE_HELD,
                    &record.payload,
                )?
            } else {
                store.delete_coordination_record(NAMESPACE, run_id, record.revision)?
            };
            match outcome {
                RecordOutcome::Applied(_) => return Ok(()),
                RecordOutcome::Conflict { .. } => continue,
            }
        }
        Err(contention_error(run_id))
    }

    fn remove_authority(
        &self,
        run_id: &str,
        runner_id: &str,
        remote_workspace: &str,
    ) -> Result<()> {
        let store = self.open()?;
        // Audit the release before deleting so no window exists in which a
        // late terminal projection could recreate the authority.
        store.append_coordination_audit(
            NAMESPACE,
            run_id,
            AUDIT_EVENT_RELEASED,
            &serde_json::to_value(ReleasedBinding {
                runner_id: runner_id.to_string(),
                remote_workspace: remote_workspace.to_string(),
            })
            .map_err(|error| Error::internal_json(error.to_string(), Some(run_id.to_string())))?,
        )?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some((record, payload)) = read_authority(&store, run_id)? else {
                return Ok(());
            };
            if !payload.binds(run_id, runner_id, remote_workspace) {
                return Ok(());
            }
            match store.delete_coordination_record(NAMESPACE, run_id, record.revision)? {
                RecordOutcome::Applied(_) => return Ok(()),
                RecordOutcome::Conflict { .. } => continue,
            }
        }
        Err(contention_error(run_id))
    }
}

/// The live authority record for `run_id`. A record whose payload does not
/// parse or carries an unsupported schema fails closed.
fn read_authority(
    store: &ObservationStore,
    run_id: &str,
) -> Result<Option<(CoordinationRecord, AuthorityPayload)>> {
    let Some(record) = store.coordination_record(NAMESPACE, run_id)? else {
        return Ok(None);
    };
    let payload: AuthorityPayload = serde_json::from_value(record.payload.clone())
        .map_err(|_| malformed_record_error(run_id))?;
    if payload.schema != WORKSPACE_TERMINAL_AUTHORITY_SCHEMA || payload.run_id != run_id {
        return Err(malformed_record_error(run_id));
    }
    Ok(Some((record, payload)))
}

fn put(
    store: &ObservationStore,
    run_id: &str,
    expected: ExpectedRevision,
    state: &str,
    payload: &Value,
) -> Result<RecordOutcome> {
    store.put_coordination_record(
        NAMESPACE,
        run_id,
        expected,
        RecordWrite {
            state,
            payload,
            expires_at_ms: None,
        },
    )
}

fn released_bindings(store: &ObservationStore, run_id: &str) -> Result<Vec<ReleasedBinding>> {
    Ok(store
        .coordination_audit(NAMESPACE, run_id, MAX_COORDINATION_AUDIT_READ)?
        .into_iter()
        .filter(|entry| entry.event == AUDIT_EVENT_RELEASED)
        .filter_map(|entry| serde_json::from_value(entry.payload).ok())
        .collect())
}

fn has_release_audit(store: &ObservationStore, run_id: &str) -> Result<bool> {
    Ok(!released_bindings(store, run_id)?.is_empty())
}

fn was_released(
    store: &ObservationStore,
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<bool> {
    Ok(released_bindings(store, run_id)?.iter().any(|binding| {
        binding.runner_id == runner_id && binding.remote_workspace == remote_workspace
    }))
}

fn to_payload(payload: &AuthorityPayload) -> Result<Value> {
    serde_json::to_value(payload)
        .map_err(|error| Error::internal_json(error.to_string(), Some(payload.run_id.clone())))
}

fn malformed_record_error(run_id: &str) -> Error {
    Error::validation_invalid_argument(
        "workspace_terminal_authority",
        "workspace terminal authority record is malformed or contradicts workspace ownership",
        Some(run_id.to_string()),
        None,
    )
}

fn unavailable_retained_workspace_error(run_id: &str) -> Error {
    Error::validation_invalid_argument(
        "run_id",
        "retained Lab workspace authority is unavailable; the workspace may have been reaped before artifacts were attached",
        Some(run_id.to_string()),
        None,
    )
}

fn contention_error(run_id: &str) -> Error {
    Error::validation_invalid_argument(
        "workspace_terminal_authority",
        "workspace terminal authority record kept changing under contention",
        Some(run_id.to_string()),
        None,
    )
}

#[cfg(any(test, feature = "test-support"))]
fn persist_workspace_terminal_authority(receipt: WorkspaceTerminalAuthorityReceipt) -> Result<()> {
    WorkspaceTerminalAuthorityStore::from_environment()?.persist(receipt)
}

#[cfg(any(test, feature = "test-support"))]
pub fn persist_workspace_terminal_authority_for_test(
    run_id: &str,
    runner_id: &str,
    runner_job_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    persist_workspace_terminal_authority(WorkspaceTerminalAuthorityReceipt {
        schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        runner_id: runner_id.to_string(),
        runner_job_id: runner_job_id.to_string(),
        remote_workspace: remote_workspace.to_string(),
    })
}

/// Resolve terminal workspace authority in the process installation.
///
/// One resolution of the roots covers the record read and the freeze this
/// performs when an exact-binding check fails.
pub fn resolve_workspace_terminal_authority(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    job_id: Option<&str>,
) -> Result<Option<WorkspaceTerminalAuthorityReceipt>> {
    WorkspaceTerminalAuthorityStore::from_environment()?.resolve_authority(
        run_id,
        runner_id,
        remote_workspace,
        job_id,
    )
}

/// Resolve the retained Lab workspace for a terminal agent task without
/// relying on a caller-provided runner path. A released receipt deliberately
/// fails closed so callers can distinguish a reaped workspace from an empty
/// discovery result.
pub fn resolve_retained_workspace(run_id: &str) -> Result<RetainedWorkspace> {
    WorkspaceTerminalAuthorityStore::from_environment()?.resolve_retained_workspace(run_id)
}

pub fn workspace_terminal_authority_release_is_pending(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<bool> {
    WorkspaceTerminalAuthorityStore::from_environment()?.release_is_pending(
        run_id,
        runner_id,
        remote_workspace,
    )
}

pub fn begin_workspace_terminal_authority_release(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    WorkspaceTerminalAuthorityStore::from_environment()?.begin_release(
        run_id,
        runner_id,
        remote_workspace,
    )
}

pub fn abort_workspace_terminal_authority_release(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    WorkspaceTerminalAuthorityStore::from_environment()?.abort_release(
        run_id,
        runner_id,
        remote_workspace,
    )
}

pub fn remove_workspace_terminal_authority(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    WorkspaceTerminalAuthorityStore::from_environment()?.remove_authority(
        run_id,
        runner_id,
        remote_workspace,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_survives_run_record_compaction_and_requires_exact_binding() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let receipt = WorkspaceTerminalAuthorityReceipt {
                schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
                run_id: "run-1".to_string(),
                runner_id: "reverse-or-direct".to_string(),
                runner_job_id: "job-1".to_string(),
                remote_workspace: "/runner/_lab_workspaces/workspace-1".to_string(),
            };
            persist_workspace_terminal_authority(receipt.clone()).expect("persist receipt");
            persist_workspace_terminal_authority(receipt)
                .expect("terminal progression is idempotent");

            assert!(resolve_retained_workspace("run-1")
                .expect("discover from run id after record compaction")
                .remote_workspace
                .ends_with("workspace-1"));

            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .expect("resolve after lifecycle compaction")
            .is_some());
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("other-job")
            )
            .is_err());
            assert!(resolve_retained_workspace("run-1").is_err());
            begin_workspace_terminal_authority_release(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("begin workspace cleanup");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .is_err());
            assert!(workspace_terminal_authority_release_is_pending(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("pending cleanup authority"));
            abort_workspace_terminal_authority_release(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("abort failed workspace cleanup");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .expect("authority restored after cleanup abort")
            .is_some());
            remove_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("remove after workspace cleanup");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .is_err());
            persist_workspace_terminal_authority_for_test(
                "run-1",
                "reverse-or-direct",
                "job-1",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("late terminal projection is suppressed");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .is_err());
        });
    }

    fn isolated_store() -> (tempfile::TempDir, WorkspaceTerminalAuthorityStore) {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("observations.sqlite");
        drop(ObservationStore::open_initialized_for_lifecycle_at(&path).expect("create store"));
        (
            directory,
            WorkspaceTerminalAuthorityStore::at_database(path),
        )
    }

    fn receipt(job: &str) -> WorkspaceTerminalAuthorityReceipt {
        WorkspaceTerminalAuthorityReceipt {
            schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
            run_id: "run-1".to_string(),
            runner_id: "runner-1".to_string(),
            runner_job_id: job.to_string(),
            remote_workspace: "/runner/workspace".to_string(),
        }
    }

    #[test]
    fn malformed_or_mixed_version_records_fail_closed() {
        let (_dir, authority) = isolated_store();
        let store = authority.open().expect("open store");
        let payload = serde_json::json!({
            "schema": "homeboy/workspace-terminal-authority/v2",
            "run_id": "run-1",
            "runner_id": "runner-1",
            "runner_job_id": "job-1",
            "remote_workspace": "/runner/workspace"
        });
        put(
            &store,
            "run-1",
            ExpectedRevision::Absent,
            STATE_HELD,
            &payload,
        )
        .expect("write unsupported record");

        assert!(authority
            .resolve_authority("run-1", "runner-1", "/runner/workspace", Some("job-1"))
            .is_err());
        assert!(authority.resolve_retained_workspace("run-1").is_err());
    }

    #[test]
    fn receipt_is_one_held_record_keyed_by_run_id_and_conflicts_are_rejected() {
        let (_dir, authority) = isolated_store();
        authority.persist(receipt("job-1")).expect("persist");
        authority.persist(receipt("job-1")).expect("idempotent");
        assert!(authority.persist(receipt("job-2")).is_err());

        let record = authority
            .open()
            .unwrap()
            .coordination_record(NAMESPACE, "run-1")
            .unwrap()
            .expect("record keyed by run id");
        assert_eq!(record.state, STATE_HELD);
        assert_eq!(record.revision, 1);
    }

    #[test]
    fn concurrent_begin_release_transitions_exactly_once() {
        const CALLERS: usize = 6;
        let (_dir, authority) = isolated_store();
        authority.persist(receipt("job-1")).expect("persist");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
        let threads: Vec<_> = (0..CALLERS)
            .map(|_| {
                let authority = authority.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let store = authority.open().expect("open store");
                    barrier.wait();
                    authority
                        .begin_release_in(&store, "run-1", "runner-1", "/runner/workspace")
                        .expect("begin release")
                })
            })
            .collect();
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == BeginRelease::Began)
                .count(),
            1
        );
        assert!(outcomes
            .iter()
            .all(|outcome| matches!(outcome, BeginRelease::Began | BeginRelease::AlreadyPending)));
        let record = authority
            .open()
            .unwrap()
            .coordination_record(NAMESPACE, "run-1")
            .unwrap()
            .expect("pending record");
        assert_eq!(record.state, STATE_RELEASE_PENDING);
        assert_eq!(record.revision, 2);
    }

    #[test]
    fn release_state_survives_reopen_and_removal_deletes_the_record() {
        let (_dir, authority) = isolated_store();
        authority.persist(receipt("job-1")).expect("persist");
        authority
            .begin_release("run-1", "runner-1", "/runner/workspace")
            .expect("begin");

        // A fresh handle over the same database observes the pending state.
        let reopened = authority.clone();
        assert!(reopened
            .release_is_pending("run-1", "runner-1", "/runner/workspace")
            .unwrap());
        assert!(!reopened
            .release_is_pending("run-1", "other-runner", "/runner/workspace")
            .unwrap());

        reopened
            .remove_authority("run-1", "runner-1", "/runner/workspace")
            .expect("remove");
        assert!(reopened
            .open()
            .unwrap()
            .coordination_record(NAMESPACE, "run-1")
            .unwrap()
            .is_none());
        assert!(reopened
            .begin_release("run-1", "runner-1", "/runner/workspace")
            .is_err());
        reopened
            .persist(receipt("job-1"))
            .expect("late projection is suppressed");
        assert!(reopened
            .open()
            .unwrap()
            .coordination_record(NAMESPACE, "run-1")
            .unwrap()
            .is_none());
        assert!(reopened.resolve_retained_workspace("run-1").is_err());
    }

    #[test]
    fn begin_without_receipt_is_dropped_by_abort() {
        let (_dir, authority) = isolated_store();
        authority
            .begin_release("run-1", "runner-1", "/runner/workspace")
            .expect("begin without receipt");
        assert!(authority
            .release_is_pending("run-1", "runner-1", "/runner/workspace")
            .unwrap());
        authority
            .abort_release("run-1", "runner-1", "/runner/workspace")
            .expect("abort");
        assert!(authority
            .open()
            .unwrap()
            .coordination_record(NAMESPACE, "run-1")
            .unwrap()
            .is_none());
        assert_eq!(
            authority
                .resolve_authority("run-1", "runner-1", "/runner/workspace", None)
                .unwrap(),
            None
        );
    }
}
