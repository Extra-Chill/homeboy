//! Automatic retention passes and their bookkeeping (#15947).
//!
//! Each retention kind keeps its last-run state as one coordination record in
//! the observation store, keyed by the kind, and admits a single pass at a
//! time through an exclusive coordination lease owned by the current process.
//! A crashed pass is reaped by its recorded process identity, so the next pass
//! proceeds without waiting on an age heuristic.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{cleanup_shared_cargo_targets, CargoTargetCleanupOptions, CargoTargetCleanupOutput};
use crate::defaults::RetentionConfig;
use crate::engine::temp::{
    cleanup_runtime_tmp_bounded, RuntimeTempCleanupOptions, RuntimeTempCleanupOutput,
};
use crate::observation::coordination::{
    ExpectedRevision, LeaseAcquire, LeaseAdmission, LeaseOwner, LeaseRequest, RecordOutcome,
    RecordWrite,
};
use crate::observation::ObservationStore;
use crate::{Error, Result};

/// Coordination namespace owned by automatic retention.
pub const AUTOMATIC_RETENTION_NAMESPACE: &str = "automatic-retention";

/// Backstop expiry for a running-pass lease. A dead owner on this host is
/// reaped by process identity long before this; the TTL only bounds owners
/// whose liveness cannot be proven from here.
const RUNNING_LEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const STATE_RUNNING: &str = "running";

/// The retention passes that keep bookkeeping in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticRetentionKind {
    /// Bounded shared cargo-target retention.
    CargoTarget,
    /// Bounded runtime-temp retention.
    RuntimeTemp,
    /// The `cleanup automatic-retention` controller pass.
    Controller,
}

impl AutomaticRetentionKind {
    /// The record and lease key for this kind.
    pub fn key(self) -> &'static str {
        match self {
            Self::CargoTarget => "cargo-target",
            Self::RuntimeTemp => "runtime-temp",
            Self::Controller => "controller",
        }
    }

    /// Where this kind's state lives, for output: `<namespace>/<key>`.
    pub fn state_key(self) -> String {
        format!("{AUTOMATIC_RETENTION_NAMESPACE}/{}", self.key())
    }
}

#[derive(Debug, Serialize)]
pub struct AutomaticRetentionOutput {
    pub command: &'static str,
    pub status: &'static str,
    pub max_run_seconds: u64,
    pub row_limit: usize,
    /// Coordination record holding this pass's state, as `<namespace>/<key>`.
    pub state_key: String,
    pub resume_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo_targets: Option<CargoTargetCleanupOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_tmp: Option<RuntimeTempRetentionOutput>,
}

/// Bounded runtime-temp retention facts for the one root Homeboy allocates into.
#[derive(Debug, Serialize)]
pub struct RuntimeTempRetentionOutput {
    pub root: String,
    pub managed_bytes: u64,
    pub protected_bytes: u64,
    pub reclaimable_bytes: u64,
    pub reclaimed_bytes: u64,
    pub continuation_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub cleanup: RuntimeTempCleanupOutput,
}

/// Record payload for one retention kind. The pass status is the record state.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AutomaticRetentionState {
    last_started_unix_ms: u64,
    #[serde(default)]
    last_finished_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
}

/// One admitted retention pass. It holds the kind's exclusive running lease
/// until [`AutomaticRetentionPass::finish`] or drop releases it.
pub struct AutomaticRetentionPass {
    store: ObservationStore,
    kind: AutomaticRetentionKind,
    holder_token: String,
    revision: i64,
    state: AutomaticRetentionState,
}

impl AutomaticRetentionPass {
    /// The resume cursor the previous pass of this kind left behind.
    pub fn cursor(&self) -> Option<&str> {
        self.state.cursor.as_deref()
    }

    /// Record the pass outcome and release the running lease.
    pub fn finish(mut self, status: &str, cursor: Option<String>) -> Result<()> {
        self.state.cursor = cursor;
        self.state.last_finished_unix_ms = unix_ms(SystemTime::now());
        let state = self.state.clone();
        self.write(status, &state)
    }

    fn write(&mut self, status: &str, state: &AutomaticRetentionState) -> Result<()> {
        self.revision = write_state(
            &self.store,
            self.kind,
            ExpectedRevision::Revision(self.revision),
            status,
            state,
        )?;
        Ok(())
    }
}

impl Drop for AutomaticRetentionPass {
    fn drop(&mut self) {
        let _ = self.store.release_coordination_lease(
            AUTOMATIC_RETENTION_NAMESPACE,
            self.kind.key(),
            &self.holder_token,
        );
    }
}

/// Admit one pass of `kind`, or `None` when another live pass holds it.
///
/// Holders whose owning process is proven dead on this host are reaped first,
/// so a crashed pass never blocks the next one. The admitted pass's record is
/// moved to `running` with a fresh start time.
pub fn begin_automatic_retention_pass(
    store: ObservationStore,
    kind: AutomaticRetentionKind,
    now: SystemTime,
) -> Result<Option<AutomaticRetentionPass>> {
    store.reap_coordination_leases(AUTOMATIC_RETENTION_NAMESPACE)?;
    let acquired = store.acquire_coordination_lease(LeaseRequest {
        namespace: AUTOMATIC_RETENTION_NAMESPACE,
        key: kind.key(),
        ttl: RUNNING_LEASE_TTL,
        owner: LeaseOwner::current_process(),
        admission: LeaseAdmission::Exclusive,
        weight: &Value::Null,
        payload: &Value::Null,
    })?;
    let holder = match acquired {
        LeaseAcquire::Acquired(holder) => holder,
        LeaseAcquire::Contended(_) => return Ok(None),
    };
    let mut pass = AutomaticRetentionPass {
        store,
        kind,
        holder_token: holder.holder_token,
        revision: 0,
        state: AutomaticRetentionState::default(),
    };
    let current = pass
        .store
        .coordination_record(AUTOMATIC_RETENTION_NAMESPACE, kind.key())?;
    let expected = match &current {
        Some(record) => {
            // An unreadable payload only loses the resume cursor; the next
            // pass restarts its sweep from the beginning.
            pass.state = serde_json::from_value(record.payload.clone()).unwrap_or_default();
            ExpectedRevision::Revision(record.revision)
        }
        None => ExpectedRevision::Absent,
    };
    pass.state.last_started_unix_ms = unix_ms(now);
    let state = pass.state.clone();
    pass.revision = write_state(&pass.store, kind, expected, STATE_RUNNING, &state)?;
    Ok(Some(pass))
}

/// Run one bounded shared cargo-target retention pass that stops at
/// `deadline`. The caller owns the budget: a phase of a larger pass passes its
/// share, a standalone admission passes its own.
pub fn run_automatic_cargo_retention(
    deadline: Option<SystemTime>,
) -> Result<AutomaticRetentionOutput> {
    let retention = crate::defaults::load_config().retention;
    let store = ObservationStore::open_initialized_for_lifecycle()?;
    run_automatic_cargo_retention_in(&retention, store, None, SystemTime::now(), deadline)
}

/// Run the runtime-temp owner, stopping at an entry boundary at `deadline`.
/// Also run before allocating more temporary storage when its filesystem is
/// under configured capacity pressure.
pub fn run_automatic_runtime_temp_retention(
    deadline: Option<SystemTime>,
) -> Result<AutomaticRetentionOutput> {
    let retention = crate::defaults::load_config().retention;
    let store = ObservationStore::open_initialized_for_lifecycle()?;
    run_automatic_runtime_temp_retention_in(&retention, store, SystemTime::now(), deadline)
}

fn run_automatic_cargo_retention_in(
    retention: &RetentionConfig,
    store: ObservationStore,
    cargo_root: Option<PathBuf>,
    now: SystemTime,
    deadline: Option<SystemTime>,
) -> Result<AutomaticRetentionOutput> {
    let kind = AutomaticRetentionKind::CargoTarget;
    let base = output_base(retention, kind);
    let Some(pass) = begin_automatic_retention_pass(store, kind, now)? else {
        return Ok(base);
    };

    let cargo_targets = cleanup_shared_cargo_targets(CargoTargetCleanupOptions {
        root: cargo_root,
        apply: true,
        older_than: Duration::from_secs(retention.shared_store_days.saturating_mul(86_400)),
        lease_ttl: Duration::from_secs(retention.shared_store_lease_seconds),
        max_bytes: retention.shared_store_max_bytes,
        limit: usize::try_from(retention.limit).unwrap_or(0),
        cursor: pass.cursor().map(str::to_string),
        now,
        deadline,
    })?;
    let status = pass_status(
        cargo_targets.applied_count == 0,
        cargo_targets.continuation_required,
    );
    pass.finish(status, cargo_targets.next_cursor.clone())?;
    Ok(AutomaticRetentionOutput {
        status,
        cargo_targets: Some(cargo_targets),
        ..base
    })
}

fn run_automatic_runtime_temp_retention_in(
    retention: &RetentionConfig,
    store: ObservationStore,
    now: SystemTime,
    deadline: Option<SystemTime>,
) -> Result<AutomaticRetentionOutput> {
    let kind = AutomaticRetentionKind::RuntimeTemp;
    let base = output_base(retention, kind);
    let Some(pass) = begin_automatic_retention_pass(store, kind, now)? else {
        return Ok(base);
    };
    let runtime_tmp = cleanup_runtime_tmp_retention(retention, pass.cursor(), deadline)?;
    let status = pass_status(
        runtime_tmp.reclaimed_bytes == 0,
        runtime_tmp.continuation_required,
    );
    pass.finish(status, runtime_tmp.next_cursor.clone())?;
    Ok(AutomaticRetentionOutput {
        status,
        runtime_tmp: Some(runtime_tmp),
        ..base
    })
}

fn pass_status(no_progress: bool, continuation_required: bool) -> &'static str {
    match (no_progress, continuation_required) {
        (true, true) => "no_progress",
        (false, true) => "partial",
        _ => "completed",
    }
}

fn cleanup_runtime_tmp_retention(
    retention: &RetentionConfig,
    cursor: Option<&str>,
    deadline: Option<SystemTime>,
) -> Result<RuntimeTempRetentionOutput> {
    let cleanup = cleanup_runtime_tmp_bounded(RuntimeTempCleanupOptions {
        apply: true,
        older_than_days: retention.runtime_tmp_days,
        managed_older_than_days: None,
        prefix: None,
        limit: usize::try_from(retention.limit).unwrap_or(0),
        run_max_bytes: retention.runtime_run_max_bytes,
        run_max_count: retention.runtime_run_max_count,
        cursor,
        deadline: deadline.map(|deadline| {
            std::time::Instant::now()
                + deadline
                    .duration_since(SystemTime::now())
                    .unwrap_or(Duration::ZERO)
        }),
    })?;
    let managed_bytes = cleanup
        .rows
        .iter()
        .filter(|row| row.owner_id.is_some())
        .map(|row| row.size_bytes)
        .sum();
    let protected_bytes = cleanup
        .rows
        .iter()
        .filter(|row| row.protection_reason.is_some())
        .map(|row| row.size_bytes)
        .sum();
    Ok(RuntimeTempRetentionOutput {
        root: cleanup.runtime_tmp_root.clone(),
        managed_bytes,
        protected_bytes,
        reclaimable_bytes: cleanup.totals.planned_size_bytes,
        reclaimed_bytes: cleanup.totals.removed_size_bytes,
        continuation_required: cleanup.has_more,
        next_cursor: cleanup.next_cursor.clone(),
        cleanup,
    })
}

fn output_base(
    retention: &RetentionConfig,
    kind: AutomaticRetentionKind,
) -> AutomaticRetentionOutput {
    AutomaticRetentionOutput {
        command: "cleanup.automatic_retention",
        status: "busy",
        max_run_seconds: retention.automatic_retention_max_run_seconds,
        row_limit: usize::try_from(retention.limit).unwrap_or(0),
        state_key: kind.state_key(),
        resume_command: "homeboy cleanup automatic-retention".to_string(),
        cargo_targets: None,
        runtime_tmp: None,
    }
}

fn unix_ms(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Compare-and-set the kind's record and return the written revision.
fn write_state(
    store: &ObservationStore,
    kind: AutomaticRetentionKind,
    expected: ExpectedRevision,
    status: &str,
    state: &AutomaticRetentionState,
) -> Result<i64> {
    let payload = serde_json::to_value(state).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize automatic retention state".to_string()),
        )
    })?;
    match store.put_coordination_record(
        AUTOMATIC_RETENTION_NAMESPACE,
        kind.key(),
        expected,
        RecordWrite {
            state: status,
            payload: &payload,
            expires_at_ms: None,
        },
    )? {
        RecordOutcome::Applied(Some(record)) => Ok(record.revision),
        RecordOutcome::Applied(None) | RecordOutcome::Conflict { .. } => {
            Err(Error::internal_unexpected(format!(
                "automatic retention state '{}' changed while its pass held the running lease",
                kind.state_key()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Barrier, Mutex, OnceLock};

    use super::*;
    use crate::cleanup::cargo_targets::acquire_shared_cargo_target_in;
    use crate::observation::coordination::LeaseHolder;
    use tempfile::TempDir;

    static TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn isolated_store() -> (TempDir, PathBuf) {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("observations.sqlite");
        drop(open(&path));
        (directory, path)
    }

    fn open(path: &Path) -> ObservationStore {
        ObservationStore::open_initialized_for_lifecycle_at(path).expect("open store")
    }

    fn record(path: &Path, kind: AutomaticRetentionKind) -> (String, AutomaticRetentionState) {
        let record = open(path)
            .coordination_record(AUTOMATIC_RETENTION_NAMESPACE, kind.key())
            .unwrap()
            .expect("retention record");
        (
            record.state,
            serde_json::from_value(record.payload).unwrap(),
        )
    }

    fn holders(path: &Path, kind: AutomaticRetentionKind) -> Vec<LeaseHolder> {
        open(path)
            .coordination_lease_holders(AUTOMATIC_RETENTION_NAMESPACE, kind.key())
            .unwrap()
    }

    fn enabled_retention() -> RetentionConfig {
        RetentionConfig {
            limit: 1,
            shared_store_days: 30,
            shared_store_max_bytes: 4,
            shared_store_lease_seconds: 60,
            ..RetentionConfig::default()
        }
    }

    fn store(root: &Path, owner: &str, bytes: usize, now: SystemTime) -> PathBuf {
        let lease = acquire_shared_cargo_target_in(root, owner, now).unwrap();
        let path = lease.target_dir().to_path_buf();
        fs::write(path.join("artifact"), vec![b'x'; bytes]).unwrap();
        drop(lease);
        path
    }

    /// A lease owner on this host whose process has exited.
    fn dead_owner() -> LeaseOwner {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        LeaseOwner {
            pid: Some(pid),
            ..LeaseOwner::current_process()
        }
    }

    #[test]
    fn cargo_budget_converges_across_bounded_resumable_passes() {
        let _serial = serial();
        let (_db, db) = isolated_store();
        let cargo = TempDir::new().unwrap();
        let now = SystemTime::now();
        let first = store(cargo.path(), "first", 4, now);
        let second = store(cargo.path(), "second", 4, now);
        let mut config = enabled_retention();
        config.shared_store_max_bytes = 0;

        let first_pass = run_automatic_cargo_retention_in(
            &config,
            open(&db),
            Some(cargo.path().to_path_buf()),
            now,
            None,
        )
        .unwrap();
        assert_eq!(first_pass.status, "partial");
        assert_eq!(
            first_pass.cargo_targets.as_ref().unwrap().inspected_count,
            1
        );
        assert_ne!(first.exists(), second.exists());
        // The resume cursor survives a reopen of the store.
        let (state, payload) = record(&db, AutomaticRetentionKind::CargoTarget);
        assert_eq!(state, "partial");
        assert!(payload.cursor.is_some());
        assert!(holders(&db, AutomaticRetentionKind::CargoTarget).is_empty());

        let second_pass = run_automatic_cargo_retention_in(
            &config,
            open(&db),
            Some(cargo.path().to_path_buf()),
            now,
            None,
        )
        .unwrap();
        assert_eq!(second_pass.status, "completed");
        assert!(!second.exists());
        let (state, payload) = record(&db, AutomaticRetentionKind::CargoTarget);
        assert_eq!(state, "completed");
        assert_eq!(payload.cursor, None);
        assert!(payload.last_finished_unix_ms >= payload.last_started_unix_ms);
    }

    #[test]
    fn active_lease_is_protected_and_bounded_pass_records_no_progress() {
        let _serial = serial();
        let (_db, db) = isolated_store();
        let cargo = TempDir::new().unwrap();
        let now = SystemTime::now();
        let lease =
            acquire_shared_cargo_target_in(cargo.path(), "active", now - Duration::from_secs(1))
                .unwrap();
        fs::write(lease.target_dir().join("artifact"), b"payload").unwrap();
        let other = store(cargo.path(), "other", 8, now);
        let output = run_automatic_cargo_retention_in(
            &enabled_retention(),
            open(&db),
            Some(cargo.path().to_path_buf()),
            now,
            None,
        )
        .unwrap();
        assert_eq!(output.status, "no_progress");
        assert_eq!(output.cargo_targets.as_ref().unwrap().inspected_count, 1);
        assert!(lease.target_dir().exists());
        assert!(other.exists());
        assert_eq!(
            record(&db, AutomaticRetentionKind::CargoTarget).0,
            "no_progress"
        );
    }

    #[test]
    fn runtime_temp_retention_reclaims_stale_roots_and_protects_live_owners() {
        // The runtime temp root is process-global environment that other
        // tests set under the shared env guard; hold it so they cannot
        // redirect the live owner mid-test. The guard restores it on drop.
        let _env = crate::test_support::home_env_guard();
        let _serial = serial();
        let (_db, db) = isolated_store();
        // The live owner must land in this root, which requires a socket-safe
        // path: under the long per-run test TMPDIR (always on macOS) the owner
        // falls back to the invocation runtime root and is never inspected.
        let runtime = tempfile::tempdir_in("/tmp").unwrap();
        let env_name = crate::product_identity::PRODUCT_IDENTITY.env_var("RUNTIME_TMPDIR");
        std::env::set_var(&env_name, runtime.path());
        let stale = ["stale-ownerless-a", "stale-ownerless-b"].map(|name| {
            let path = runtime.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("payload"), b"stale payload").unwrap();
            fs::File::open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(1))
                .unwrap();
            path
        });
        let live = crate::engine::temp::RuntimeTempOwner::allocate("live", "test").unwrap();
        fs::write(live.path().join("payload"), b"live payload").unwrap();
        let mut config = enabled_retention();
        config.runtime_tmp_days = 0;
        config.limit = 1;

        // `limit` bounds reclaim candidates: the live owner is walked past and
        // protected, and the pass still reaches one stale root behind it.
        let first =
            run_automatic_runtime_temp_retention_in(&config, open(&db), SystemTime::now(), None)
                .unwrap();
        let first_runtime = first.runtime_tmp.unwrap();
        assert!(live.path().exists());
        assert!(first_runtime.protected_bytes >= b"live payload".len() as u64);
        assert!(first_runtime.reclaimed_bytes >= b"stale payload".len() as u64);
        assert_eq!(stale.iter().filter(|path| path.exists()).count(), 1);
        assert!(first_runtime.continuation_required);
        assert_eq!(first_runtime.root, runtime.path().display().to_string());
        assert_eq!(
            record(&db, AutomaticRetentionKind::RuntimeTemp).1.cursor,
            first_runtime.next_cursor
        );

        // The persisted cursor resumes the walk and reclaims the remainder.
        let second =
            run_automatic_runtime_temp_retention_in(&config, open(&db), SystemTime::now(), None)
                .unwrap();
        let second_runtime = second.runtime_tmp.unwrap();
        assert!(stale.iter().all(|path| !path.exists()));
        assert!(live.path().exists());
        assert!(second_runtime.reclaimable_bytes >= b"stale payload".len() as u64);
        assert!(second_runtime.reclaimed_bytes >= b"stale payload".len() as u64);
    }

    #[test]
    fn concurrent_pass_is_admission_rejected_without_mutating_state() {
        let _serial = serial();
        let (_db, db) = isolated_store();
        let cargo = TempDir::new().unwrap();
        let running = begin_automatic_retention_pass(
            open(&db),
            AutomaticRetentionKind::CargoTarget,
            SystemTime::now(),
        )
        .unwrap()
        .expect("first pass admitted");
        let before = open(&db)
            .coordination_record(AUTOMATIC_RETENTION_NAMESPACE, "cargo-target")
            .unwrap()
            .unwrap();

        let output = run_automatic_cargo_retention_in(
            &enabled_retention(),
            open(&db),
            Some(cargo.path().to_path_buf()),
            SystemTime::now(),
            None,
        )
        .unwrap();

        assert_eq!(output.status, "busy");
        assert_eq!(output.state_key, "automatic-retention/cargo-target");
        let after = open(&db)
            .coordination_record(AUTOMATIC_RETENTION_NAMESPACE, "cargo-target")
            .unwrap()
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(after.state, STATE_RUNNING);
        drop(running);
        assert!(holders(&db, AutomaticRetentionKind::CargoTarget).is_empty());
    }

    #[test]
    fn two_claimants_admit_exactly_one_pass() {
        const CLAIMANTS: usize = 2;
        let (_db, db) = isolated_store();
        let barrier = Arc::new(Barrier::new(CLAIMANTS));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let threads: Vec<_> = (0..CLAIMANTS)
            .map(|_| {
                let db = db.clone();
                let barrier = Arc::clone(&barrier);
                let released = Arc::clone(&released);
                std::thread::spawn(move || {
                    let store = open(&db);
                    barrier.wait();
                    let pass = begin_automatic_retention_pass(
                        store,
                        AutomaticRetentionKind::RuntimeTemp,
                        SystemTime::now(),
                    )
                    .unwrap();
                    let admitted = pass.is_some();
                    if admitted {
                        // Hold the pass until the loser has been judged.
                        let _ = released.lock().unwrap().recv();
                    }
                    admitted
                })
            })
            .collect();
        // Wait until one holder exists and the other claimant has returned.
        let start = std::time::Instant::now();
        while holders(&db, AutomaticRetentionKind::RuntimeTemp).is_empty()
            || threads.iter().filter(|thread| thread.is_finished()).count() < 1
        {
            assert!(start.elapsed() < Duration::from_secs(30), "claims stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
        release.send(()).unwrap();
        let admitted = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 1);
        assert!(holders(&db, AutomaticRetentionKind::RuntimeTemp).is_empty());
    }

    #[test]
    fn kinds_hold_independent_running_leases() {
        let (_db, db) = isolated_store();
        let cargo = begin_automatic_retention_pass(
            open(&db),
            AutomaticRetentionKind::CargoTarget,
            SystemTime::now(),
        )
        .unwrap();
        let runtime = begin_automatic_retention_pass(
            open(&db),
            AutomaticRetentionKind::RuntimeTemp,
            SystemTime::now(),
        )
        .unwrap();
        assert!(cargo.is_some());
        assert!(runtime.is_some());
    }

    #[test]
    fn crashed_owner_is_reaped_by_process_identity() {
        let (_db, db) = isolated_store();
        let kind = AutomaticRetentionKind::Controller;
        let crashed = open(&db)
            .acquire_coordination_lease(LeaseRequest {
                namespace: AUTOMATIC_RETENTION_NAMESPACE,
                key: kind.key(),
                ttl: RUNNING_LEASE_TTL,
                owner: dead_owner(),
                admission: LeaseAdmission::Exclusive,
                weight: &Value::Null,
                payload: &Value::Null,
            })
            .unwrap();
        assert!(matches!(crashed, LeaseAcquire::Acquired(_)));

        // The dead holder has not expired; only its process identity frees it.
        let pass = begin_automatic_retention_pass(open(&db), kind, SystemTime::now())
            .unwrap()
            .expect("crashed owner reaped");
        let live = holders(&db, kind);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].owner, LeaseOwner::current_process());
        pass.finish("completed", None).unwrap();
        assert_eq!(record(&db, kind).0, "completed");
        assert!(holders(&db, kind).is_empty());
    }

    #[test]
    fn live_owner_in_this_process_is_not_reaped() {
        let (_db, db) = isolated_store();
        let kind = AutomaticRetentionKind::Controller;
        let _held = begin_automatic_retention_pass(open(&db), kind, SystemTime::now())
            .unwrap()
            .expect("first pass");
        assert!(
            begin_automatic_retention_pass(open(&db), kind, SystemTime::now())
                .unwrap()
                .is_none()
        );
        assert_eq!(holders(&db, kind).len(), 1);
    }
}
