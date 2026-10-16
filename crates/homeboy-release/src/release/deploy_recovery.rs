//! Release deploy recovery state, kept in the observation store (#15947).
//!
//! A failed release deploy leaves one checkpoint per component: a
//! coordination record keyed by component id, written and retired through a
//! revision compare-and-set. Resuming that checkpoint is admitted through an
//! exclusive coordination lease under the same namespace and key, so two
//! recoveries for one component can never both redeploy. A crashed holder is
//! reaped by process start identity on the next admission attempt, never by
//! PID alone.

use std::path::{Path, PathBuf};
use std::time::Duration;

use homeboy_core::error::{Error, Result};
use homeboy_core::observation::coordination::{
    CoordinationRecord, ExpectedRevision, LeaseAcquire, LeaseAdmission, LeaseOwner, LeaseRequest,
    RecordOutcome, RecordWrite,
};
use homeboy_core::observation::ObservationStore;
use homeboy_deploy::{PreparedDeployArtifact, PreparedDeployProjection};
use serde_json::json;

/// Coordination namespace owned by release deploy recovery.
pub(super) const NAMESPACE: &str = "release-deploy-recovery";

const STATE_PENDING: &str = "pending";

/// Upper bound on how long an admission survives without its holder releasing
/// it. Dead holders are reaped far sooner by process identity; the TTL only
/// bounds holders whose liveness cannot be proven either way.
const ADMISSION_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Compare-and-set attempts before a concurrent writer is reported.
const CAS_ATTEMPTS: usize = 3;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct DeploymentRecovery {
    pub(super) component_id: String,
    pub(super) expected_version: Option<String>,
    pub(super) projects: Vec<String>,
    pub(super) artifact: PreparedDeployArtifact,
    pub(super) projection: PreparedDeployProjection,
    pub(super) deploy_run_id: String,
    #[serde(default)]
    pub(super) component_path: String,
    #[serde(default)]
    pub(super) package_owned_paths: Vec<String>,
}

/// A loaded checkpoint and the revision it was read at.
pub(super) struct StoredRecovery {
    pub(super) recovery: DeploymentRecovery,
    pub(super) revision: i64,
}

/// RAII admission for one component's recovery. Dropping it releases the
/// lease; a crashed holder is reclaimed by the next admission's reap.
pub(super) struct RecoveryAdmission {
    db_path: PathBuf,
    component_id: String,
    holder_token: String,
}

impl Drop for RecoveryAdmission {
    fn drop(&mut self) {
        if let Ok(store) = ObservationStore::open_initialized_for_lifecycle_at(&self.db_path) {
            let _ =
                store.release_coordination_lease(NAMESPACE, &self.component_id, &self.holder_token);
        }
    }
}

fn db_path(data_root: &Path) -> PathBuf {
    homeboy_core::paths::observation_db_in_root(data_root)
}

fn open_store(data_root: &Path) -> Result<ObservationStore> {
    ObservationStore::open_initialized_for_lifecycle_at(db_path(data_root))
}

/// Admit one recovery for `component_id`, or `None` when another live
/// recovery already holds it.
pub(super) fn admit(data_root: &Path, component_id: &str) -> Result<Option<RecoveryAdmission>> {
    let store = open_store(data_root)?;
    store.reap_coordination_leases(NAMESPACE)?;
    let weight = json!({});
    let payload = json!({ "started_at": chrono::Utc::now().to_rfc3339() });
    match store.acquire_coordination_lease(LeaseRequest {
        namespace: NAMESPACE,
        key: component_id,
        ttl: ADMISSION_TTL,
        owner: LeaseOwner::current_process(),
        admission: LeaseAdmission::Exclusive,
        weight: &weight,
        payload: &payload,
    })? {
        LeaseAcquire::Acquired(holder) => Ok(Some(RecoveryAdmission {
            db_path: db_path(data_root),
            component_id: component_id.to_string(),
            holder_token: holder.holder_token,
        })),
        LeaseAcquire::Contended(_) => Ok(None),
    }
}

/// Write the checkpoint for `recovery.component_id`, replacing any earlier
/// checkpoint of the component.
pub(super) fn save(data_root: &Path, recovery: &DeploymentRecovery) -> Result<()> {
    let store = open_store(data_root)?;
    let payload = serde_json::to_value(recovery)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    for _ in 0..CAS_ATTEMPTS {
        let expected = store
            .coordination_record(NAMESPACE, &recovery.component_id)?
            .map_or(ExpectedRevision::Absent, |record| {
                ExpectedRevision::Revision(record.revision)
            });
        let outcome = store.put_coordination_record(
            NAMESPACE,
            &recovery.component_id,
            expected,
            RecordWrite {
                state: STATE_PENDING,
                payload: &payload,
                expires_at_ms: None,
            },
        )?;
        if matches!(outcome, RecordOutcome::Applied(_)) {
            return Ok(());
        }
    }
    Err(concurrent_writer(&recovery.component_id))
}

/// The checkpoint for `component_id`, if one is pending.
pub(super) fn load(data_root: &Path, component_id: &str) -> Result<Option<StoredRecovery>> {
    open_store(data_root)?
        .coordination_record(NAMESPACE, component_id)?
        .map(|record| parse(&record))
        .transpose()
}

/// Retire the checkpoint for `component_id`, whatever revision it is at.
pub(super) fn clear(data_root: &Path, component_id: &str) -> Result<()> {
    let store = open_store(data_root)?;
    for _ in 0..CAS_ATTEMPTS {
        let Some(record) = store.coordination_record(NAMESPACE, component_id)? else {
            return Ok(());
        };
        if matches!(
            store.delete_coordination_record(NAMESPACE, component_id, record.revision)?,
            RecordOutcome::Applied(_)
        ) {
            return Ok(());
        }
    }
    Err(concurrent_writer(component_id))
}

/// Retire the checkpoint only if it is still the revision a recovery read.
/// A newer checkpoint written meanwhile is kept.
pub(super) fn clear_at(data_root: &Path, component_id: &str, revision: i64) -> Result<()> {
    open_store(data_root)?.delete_coordination_record(NAMESPACE, component_id, revision)?;
    Ok(())
}

fn parse(record: &CoordinationRecord) -> Result<StoredRecovery> {
    let recovery = serde_json::from_value(record.payload.clone()).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("parse release deploy recovery `{}`", record.key)),
        )
    })?;
    Ok(StoredRecovery {
        recovery,
        revision: record.revision,
    })
}

fn concurrent_writer(component_id: &str) -> Error {
    Error::internal_unexpected(format!(
        "release deploy recovery checkpoint for '{component_id}' kept changing under a concurrent writer"
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use homeboy_core::process::ProcessStartIdentity;

    use super::*;

    fn dead_owner() -> LeaseOwner {
        let live = LeaseOwner::current_process();
        let start_identity = match live.start_identity.clone().expect("start identity") {
            ProcessStartIdentity::Linux { starttime_ticks } => ProcessStartIdentity::Linux {
                starttime_ticks: starttime_ticks.wrapping_add(1),
            },
            ProcessStartIdentity::Macos {
                start_seconds,
                start_microseconds,
            } => ProcessStartIdentity::Macos {
                start_seconds: start_seconds.wrapping_add(1),
                start_microseconds,
            },
        };
        LeaseOwner {
            start_identity: Some(start_identity),
            ..live
        }
    }

    #[test]
    fn concurrent_recoveries_for_one_component_admit_exactly_one() {
        const CALLERS: usize = 6;
        let temp = tempfile::tempdir().expect("temp dir");
        drop(open_store(temp.path()).expect("create store"));
        let barrier = Arc::new(Barrier::new(CALLERS));
        let threads: Vec<_> = (0..CALLERS)
            .map(|_| {
                let root = temp.path().to_path_buf();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    admit(&root, "fixture").expect("admit")
                })
            })
            .collect();
        let admissions: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            admissions
                .iter()
                .filter(|admission| admission.is_some())
                .count(),
            1
        );
        assert!(
            admit(temp.path(), "other").expect("admit other").is_some(),
            "admission is per component"
        );

        drop(admissions);
        assert!(
            admit(temp.path(), "fixture").expect("readmit").is_some(),
            "a released admission frees the component"
        );
    }

    #[test]
    fn crashed_recovery_is_reaped_and_checkpoint_survives_reopen() {
        let temp = tempfile::tempdir().expect("temp dir");
        let store = open_store(temp.path()).expect("open store");
        let empty = json!({});
        assert!(matches!(
            store
                .acquire_coordination_lease(LeaseRequest {
                    namespace: NAMESPACE,
                    key: "fixture",
                    ttl: ADMISSION_TTL,
                    owner: dead_owner(),
                    admission: LeaseAdmission::Exclusive,
                    weight: &empty,
                    payload: &empty,
                })
                .expect("seed crashed holder"),
            LeaseAcquire::Acquired(_)
        ));
        drop(store);

        let record = DeploymentRecovery {
            component_id: "fixture".to_string(),
            expected_version: Some("1.2.4".to_string()),
            projects: vec!["retry".to_string()],
            artifact: PreparedDeployArtifact {
                component_id: "fixture".to_string(),
                path: "/tmp/fixture.zip".to_string(),
                durable_path: "/tmp/fixture.zip".to_string(),
                size_bytes: 0,
                sha256: String::new(),
                version: "1.2.4".to_string(),
                tag: "v1.2.4".to_string(),
                source_commit: String::new(),
            },
            projection: PreparedDeployProjection::default(),
            deploy_run_id: "deploy-run-1".to_string(),
            component_path: String::new(),
            package_owned_paths: Vec::new(),
        };
        save(temp.path(), &record).expect("save checkpoint");
        save(temp.path(), &record).expect("replace checkpoint");

        // A fresh open stands in for the restarted process.
        let stored = load(temp.path(), "fixture")
            .expect("load")
            .expect("checkpoint survives reopen");
        assert_eq!(stored.recovery.deploy_run_id, "deploy-run-1");
        assert_eq!(stored.revision, 2);
        let admission = admit(temp.path(), "fixture")
            .expect("admit")
            .expect("crashed holder is reaped by process identity");

        clear_at(temp.path(), "fixture", stored.revision).expect("clear");
        assert!(load(temp.path(), "fixture").expect("load").is_none());
        drop(admission);
        clear(temp.path(), "fixture").expect("clearing an absent checkpoint is a no-op");
    }
}
